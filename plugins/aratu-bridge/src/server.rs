//! The server thread: it listens on 127.0.0.1, speaks WebSocket and the
//! bridge protocol to one page at a time, makes the control calls' parts,
//! instruments and stages off the audio thread, and drops what comes back.
//! SFZ instruments are decoded and measured on a second thread, so that a
//! load never holds up the events of a page that plays meanwhile.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use manguebyte::spsc::{Consumer, Producer};
use manguebyte_web::{
    Chain, DetachedInstrument, DetachedPart, DetachedSfz, DetachedStages, Engine, EngineConfig,
    InstrumentKind, RecordKind, SfzInstrument, SfzLoad, drum_map_json, ffi,
};

use crate::audio::{Command, FromAudio, MASTER_BEAT, MASTER_TEMPO, ToAudio};
use crate::origin::AllowList;
use crate::protocol::{
    self, MAX_MESSAGE_BYTES, MAX_SFZ_TEXT_BYTES, PROTOCOL_VERSION, SUBPROTOCOL, Welcome, page,
    plugin, query, status,
};
use crate::shared::Shared;
use crate::ws::{self, Decoder, Message, close, opcode};

/// The most parts a session's engine holds.
pub const MAX_PARTS: usize = 64;

/// How many events the engine's own queue holds: those due within a block.
pub const PENDING_CAPACITY: usize = 1 << 14;

/// The most items waiting for room in the queue to the audio thread before
/// events are dropped.
const MAX_BACKLOG: usize = 1 << 18;

/// How long a handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the transport is sent while the host plays, and while it is
/// stopped; a change is sent at once.
const TRANSPORT_EVERY: Duration = Duration::from_millis(10);
const TRANSPORT_EVERY_STOPPED: Duration = Duration::from_millis(100);
/// How often the counters are sent.
const STATUS_EVERY: Duration = Duration::from_millis(250);

/// The engine configuration of a session at `sample_rate` with quanta of
/// up to `max_quantum` frames.
pub fn engine_config(sample_rate: f64, max_quantum: usize) -> EngineConfig {
    EngineConfig {
        max_quantum: max_quantum.clamp(1, 8_192),
        max_parts: MAX_PARTS,
        pending_capacity: PENDING_CAPACITY,
        ..EngineConfig::new(sample_rate)
    }
}

/// The running server thread; dropping it stops the thread and waits for
/// it.
#[derive(Debug)]
pub struct ServerHandle {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shared
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the server thread. `port` says which port the plugin's
/// parameter asks for, unless [`Shared::port_override`] says otherwise.
pub fn spawn(
    shared: Arc<Shared>,
    port: Box<dyn Fn() -> u16 + Send>,
    to_audio: Producer<ToAudio>,
    from_audio: Consumer<FromAudio>,
    origins: AllowList,
) -> ServerHandle {
    let thread = {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("aratu-bridge server".into())
            .spawn(move || {
                Server::new(shared, port, to_audio, from_audio, origins).run();
            })
            .ok()
    };
    ServerHandle { shared, thread }
}

/// What the server knows of the session's parts, to answer control calls
/// at once with the status the engine would give.
#[derive(Debug, Default)]
struct Shadow {
    parts: BTreeMap<u16, ShadowPart>,
}

#[derive(Debug, Clone, Copy)]
struct ShadowPart {
    kind: InstrumentKind,
    chain: Chain,
    /// Every effect the part's chain has had: their stages are made.
    made: Chain,
    /// The load under way, if any.
    load: Option<u64>,
}

/// A job for the loading thread.
enum Job {
    Begin {
        part: u16,
        token: u64,
        text: String,
        request: u32,
    },
    File {
        part: u16,
        token: u64,
        path: String,
        bytes: Vec<u8>,
        request: u32,
    },
    Finish {
        part: u16,
        token: u64,
        kit: bool,
        config: EngineConfig,
        request: u32,
    },
    Abandon {
        part: u16,
    },
}

/// What the loading thread answers.
enum Done {
    Reply(u32, i32, Vec<u8>),
    Loaded {
        part: u16,
        token: u64,
        request: u32,
        sfz: Box<DetachedSfz>,
        summary: String,
    },
}

struct Session {
    stream: TcpStream,
    decoder: Decoder,
    hello: bool,
    shadow: Shadow,
    scratch: Option<Engine>,
    config: EngineConfig,
    last_transport: Instant,
    last_status: Instant,
    sent: Option<(u32, bool, u64)>,
    syncs: Vec<u32>,
}

struct Server {
    shared: Arc<Shared>,
    port: Box<dyn Fn() -> u16 + Send>,
    to_audio: Producer<ToAudio>,
    backlog: VecDeque<ToAudio>,
    from_audio: Consumer<FromAudio>,
    origins: AllowList,
    listener: Option<(u16, TcpListener)>,
    last_bind: Option<(u16, Instant)>,
    session: Option<Session>,
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    worker: Option<JoinHandle<()>>,
    next_token: u64,
    /// Requests for SFZ instruments handed to the audio thread: their
    /// summaries.
    loads: HashMap<u32, String>,
    generation: u64,
    block_seen: (u64, Instant),
    overridden: u64,
    dropped: u64,
}

impl Server {
    fn new(
        shared: Arc<Shared>,
        port: Box<dyn Fn() -> u16 + Send>,
        to_audio: Producer<ToAudio>,
        from_audio: Consumer<FromAudio>,
        origins: AllowList,
    ) -> Self {
        let (jobs, job_queue) = mpsc::channel();
        let (results, done) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("aratu-bridge loader".into())
            .spawn(move || load_worker(&job_queue, &results))
            .ok();
        let generation = shared.generation.load(std::sync::atomic::Ordering::Acquire);
        Self {
            shared,
            port,
            to_audio,
            backlog: VecDeque::new(),
            from_audio,
            origins,
            listener: None,
            last_bind: None,
            session: None,
            jobs: Some(jobs),
            done,
            worker,
            next_token: 1,
            loads: HashMap::new(),
            generation,
            block_seen: (u64::MAX, Instant::now()),
            overridden: 0,
            dropped: 0,
        }
    }

    fn run(mut self) {
        let mut buffer = vec![0u8; 64 * 1024];
        while !self
            .shared
            .shutdown
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.listen();
            self.accept();
            let idle = match self.session.as_mut() {
                Some(session) => match session.stream.read(&mut buffer) {
                    Ok(0) => {
                        self.end_session(None);
                        false
                    }
                    Ok(count) => {
                        session.decoder.push(&buffer[..count]);
                        self.messages();
                        false
                    }
                    Err(error)
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        false
                    }
                    Err(_) => {
                        self.end_session(None);
                        false
                    }
                },
                None => true,
            };
            self.collect();
            self.flush();
            self.report();
            if idle {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        self.end_session(Some((close::GOING_AWAY, "the plugin is going away")));
        self.shared.set_bound_port(None);
        drop(self.jobs.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    /// Listens on the port asked for, binding again when it changes.
    fn listen(&mut self) {
        let wanted = self.shared.port_override().unwrap_or_else(|| (self.port)());
        if self
            .listener
            .as_ref()
            .is_some_and(|(port, _)| *port == wanted)
        {
            return;
        }
        if self.listener.is_some() {
            self.end_session(Some((close::GOING_AWAY, "the port changed")));
            self.listener = None;
            self.shared.set_bound_port(None);
        }
        if self
            .last_bind
            .is_some_and(|(port, last)| port == wanted && last.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_bind = Some((wanted, Instant::now()));
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, wanted));
        if let Ok(listener) = TcpListener::bind(address) {
            if listener.set_nonblocking(true).is_ok() {
                let bound = listener
                    .local_addr()
                    .ok()
                    .map(|address| (wanted, address.port()));
                self.shared.set_bound_port(bound);
                self.listener = Some((wanted, listener));
                self.last_bind = None;
            }
        }
    }

    fn accept(&mut self) {
        let Some((_, listener)) = &self.listener else {
            return;
        };
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let Some(port) = self.shared.bound_port() else {
            return;
        };
        if stream.set_nonblocking(false).is_err()
            || stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).is_err()
        {
            return;
        }
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_nodelay(true);
        let Some((head, rest)) = read_head(&mut stream) else {
            return;
        };
        let refusal = |status, reason: &str| ws::Refusal {
            status,
            reason: reason.to_owned(),
        };
        let response = match ws::parse_request(&head) {
            None => Err(refusal(400, "not an HTTP request")),
            Some(request) => {
                let origins = &self.origins;
                let allowed = |origin: &str| origins.allows(origin);
                let policy = ws::Policy {
                    port,
                    protocol: SUBPROTOCOL,
                    origin_allowed: &allowed,
                };
                match ws::accept(&request, &policy) {
                    Ok(_) if self.session.is_some() => {
                        Err(refusal(409, "another page is connected to this plugin"))
                    }
                    other => other,
                }
            }
        };
        match response {
            Err(refusal) => {
                let _ = stream.write_all(&refusal.response());
            }
            Ok(accepted) => {
                if stream.write_all(&accepted).is_err()
                    || stream
                        .set_read_timeout(Some(Duration::from_millis(1)))
                        .is_err()
                {
                    return;
                }
                let mut decoder = Decoder::new(true, MAX_MESSAGE_BYTES);
                decoder.push(&rest);
                let now = Instant::now();
                self.session = Some(Session {
                    stream,
                    decoder,
                    hello: false,
                    shadow: Shadow::default(),
                    scratch: None,
                    config: engine_config(48_000.0, 512),
                    last_transport: now,
                    last_status: now,
                    sent: None,
                    syncs: Vec::new(),
                });
                self.shared
                    .connected
                    .store(true, std::sync::atomic::Ordering::Release);
                self.messages();
            }
        }
    }

    /// Ends the session, closing with `code` if given; every note is let
    /// go.
    fn end_session(&mut self, code: Option<(u16, &str)>) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        if let Some((code, reason)) = code {
            let mut frame = Vec::new();
            ws::encode_frame(
                &mut frame,
                true,
                opcode::CLOSE,
                &ws::close_payload(code, reason),
                None,
            );
            let _ = session.stream.write_all(&frame);
        }
        let _ = session.stream.shutdown(std::net::Shutdown::Both);
        if session.hello {
            self.queue(ToAudio::Command(Box::new(Command::Panic)));
        }
        self.shared
            .connected
            .store(false, std::sync::atomic::Ordering::Release);
    }

    fn send(&mut self, payload: &[u8]) {
        self.send_frame(opcode::BINARY, payload);
    }

    fn send_frame(&mut self, kind: u8, payload: &[u8]) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let mut frame = Vec::with_capacity(payload.len() + 10);
        ws::encode_frame(&mut frame, true, kind, payload, None);
        if session.stream.write_all(&frame).is_err() {
            self.end_session(None);
        }
    }

    fn reply(&mut self, request: u32, status: i32, payload: &[u8]) {
        self.send(&protocol::reply(request, status, payload));
    }

    /// Handles every whole message received.
    fn messages(&mut self) {
        loop {
            let Some(session) = self.session.as_mut() else {
                return;
            };
            match session.decoder.next_message() {
                Ok(None) => return,
                Ok(Some(Message::Binary(bytes))) => self.message(&bytes),
                Ok(Some(Message::Text(_))) => {
                    self.end_session(Some((close::UNSUPPORTED, "the bridge speaks binary")));
                }
                Ok(Some(Message::Ping(data))) => self.send_frame(opcode::PONG, &data),
                Ok(Some(Message::Pong(_))) => {}
                Ok(Some(Message::Close(code, _))) => {
                    let code = code.unwrap_or(close::NORMAL);
                    self.end_session(Some((code.min(4999), "")));
                }
                Err(failure) => self.end_session(Some((failure.code, failure.reason))),
            }
        }
    }

    fn message(&mut self, bytes: &[u8]) {
        let Some((header, body)) = protocol::split(bytes) else {
            self.end_session(Some((close::POLICY, "a message without a header")));
            return;
        };
        let request = header.request;
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if header.kind == page::HELLO {
            self.hello(request, body);
            return;
        }
        if !session.hello {
            if header.kind != page::EVENTS {
                self.reply(request, status::NOT_READY, b"say hello first");
            }
            return;
        }
        match header.kind {
            page::EVENTS => self.events(body),
            page::CREATE_PART | page::SET_INSTRUMENT | page::REMOVE_PART | page::SET_CHAIN => {
                let status = self.control(header.kind, body);
                self.reply(request, status, &[]);
            }
            page::SFZ_BEGIN | page::SFZ_FILE | page::SFZ_FINISH | page::SFZ_ABANDON => {
                self.sfz(header.kind, request, body);
            }
            page::QUERY => self.query(request, body),
            page::SYNC => {
                if self.backlog.is_empty() {
                    self.reply(request, status::OK, &[]);
                } else if let Some(session) = self.session.as_mut() {
                    session.syncs.push(request);
                }
            }
            page::PANIC => {
                self.queue(ToAudio::Command(Box::new(Command::Panic)));
                self.reply(request, status::OK, &[]);
            }
            _ => self.reply(request, status::BAD_MESSAGE, b"unknown message type"),
        }
    }

    fn hello(&mut self, request: u32, body: &[u8]) {
        let version = protocol::u32_at(body, 0);
        let abi = protocol::u32_at(body, 4);
        if version != Some(PROTOCOL_VERSION) || abi != Some(ffi::ABI_VERSION) {
            let text = format!(
                "the plugin speaks protocol {PROTOCOL_VERSION} and engine interface {}",
                ffi::ABI_VERSION
            );
            self.reply(request, status::BAD_MESSAGE, text.as_bytes());
            self.end_session(Some((4000, "version mismatch")));
            return;
        }
        let rate = self.shared.sample_rate();
        let quantum = self
            .shared
            .max_quantum
            .load(std::sync::atomic::Ordering::Acquire) as usize;
        let config = engine_config(rate, quantum);
        let Ok(engine) = Engine::new(config) else {
            self.reply(
                request,
                status::NOT_READY,
                b"the host has not activated the plugin",
            );
            return;
        };
        self.queue(ToAudio::Command(Box::new(Command::NewEngine(Box::new(
            engine,
        )))));
        self.generation = self
            .shared
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        let epoch = self.shared.snapshot().transport.epoch;
        let welcome = Welcome {
            protocol: PROTOCOL_VERSION,
            abi: ffi::ABI_VERSION,
            sample_rate: rate,
            latency: self
                .shared
                .latency
                .load(std::sync::atomic::Ordering::Acquire),
            max_block: self
                .shared
                .max_block
                .load(std::sync::atomic::Ordering::Acquire),
            max_parts: MAX_PARTS as u32,
            max_message: MAX_MESSAGE_BYTES as u32,
            epoch,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        if let Some(session) = self.session.as_mut() {
            session.hello = true;
            session.shadow = Shadow::default();
            session.scratch = None;
            session.config = config;
            session.sent = None;
        }
        let message = protocol::message(plugin::HELLO, request, &welcome.to_bytes());
        self.send(&message);
    }

    fn events(&mut self, body: &[u8]) {
        let Some((epoch, records)) = protocol::parse_events(body) else {
            self.end_session(Some((close::POLICY, "a malformed events message")));
            return;
        };
        let mut items = Vec::new();
        for beat in records {
            let record = beat.record;
            if record.kind == RecordKind::Param.code()
                && (record.param == MASTER_TEMPO.0 || record.param == MASTER_BEAT.0)
            {
                self.overridden += 1;
                continue;
            }
            if beat.beat.is_nan() {
                items.push(ToAudio::Now(record));
            } else if beat.beat.is_finite() {
                items.push(ToAudio::Event {
                    beat: beat.beat,
                    epoch,
                    record,
                });
            }
        }
        for item in items {
            self.queue(item);
        }
    }

    /// A control call: checked against the shadow, made, and queued.
    fn control(&mut self, kind: u8, body: &[u8]) -> i32 {
        let Some(session) = self.session.as_mut() else {
            return status::NOT_READY;
        };
        let config = session.config;
        let Some(part) = protocol::u16_at(body, 0) else {
            return status::BAD_MESSAGE;
        };
        let word = protocol::u32_at(body, 4);
        let shadow = &mut session.shadow;
        let command = match kind {
            page::CREATE_PART => {
                let (Some(code), Some(bits)) = (word, protocol::u32_at(body, 8)) else {
                    return status::BAD_MESSAGE;
                };
                let Some(kind) = InstrumentKind::from_code(code) else {
                    return ffi::status::BAD_INSTRUMENT;
                };
                let Some(chain) = Chain::from_bits(bits) else {
                    return ffi::status::BAD_CHAIN;
                };
                if !shadow.parts.contains_key(&part) && shadow.parts.len() >= MAX_PARTS {
                    return ffi::status::TOO_MANY_PARTS;
                }
                let Ok(slot) = DetachedPart::new(&config, part, kind, chain) else {
                    return ffi::status::INVALID_CONFIG;
                };
                shadow.parts.insert(
                    part,
                    ShadowPart {
                        kind,
                        chain,
                        made: union(Chain::FULL, chain),
                        load: None,
                    },
                );
                Command::Attach(slot)
            }
            page::SET_INSTRUMENT => {
                let Some(code) = word else {
                    return status::BAD_MESSAGE;
                };
                let Some(kind) = InstrumentKind::from_code(code) else {
                    return ffi::status::BAD_INSTRUMENT;
                };
                let Some(entry) = shadow.parts.get_mut(&part) else {
                    return ffi::status::NO_SUCH_PART;
                };
                let Ok(slot) = DetachedInstrument::new(&config, kind) else {
                    return ffi::status::INVALID_CONFIG;
                };
                entry.kind = kind;
                entry.load = None;
                Command::Instrument(part, slot)
            }
            page::REMOVE_PART => {
                if shadow.parts.remove(&part).is_none() {
                    return ffi::status::NO_SUCH_PART;
                }
                Command::Detach(part, DetachedPart::empty())
            }
            _ => {
                let Some(bits) = word else {
                    return status::BAD_MESSAGE;
                };
                let Some(chain) = Chain::from_bits(bits) else {
                    return ffi::status::BAD_CHAIN;
                };
                let Some(entry) = shadow.parts.get_mut(&part) else {
                    return ffi::status::NO_SUCH_PART;
                };
                let missing =
                    Chain::from_bits(chain.bits() & !entry.made.bits()).unwrap_or(Chain::NONE);
                let Ok(stages) = DetachedStages::new(&config, missing) else {
                    return ffi::status::INVALID_CONFIG;
                };
                entry.chain = chain;
                entry.made = union(entry.made, chain);
                Command::Chain(part, chain, stages)
            }
        };
        self.queue(ToAudio::Command(Box::new(command)));
        ffi::status::OK
    }

    fn sfz(&mut self, kind: u8, request: u32, body: &[u8]) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let config = session.config;
        let Some(part) = protocol::u16_at(body, 0) else {
            self.reply(request, status::BAD_MESSAGE, &[]);
            return;
        };
        let Some(entry) = session.shadow.parts.get_mut(&part) else {
            self.reply(request, ffi::status::NO_SUCH_PART, &[]);
            return;
        };
        if !entry.kind.is_sampler() {
            self.reply(request, ffi::status::NOT_A_SAMPLER, &[]);
            return;
        }
        let kit = entry.kind == InstrumentKind::SamplerKit;
        let job = match kind {
            page::SFZ_BEGIN => {
                let text = &body[4.min(body.len())..];
                if text.len() > MAX_SFZ_TEXT_BYTES {
                    self.reply(
                        request,
                        ffi::status::BAD_INPUT,
                        b"the SFZ text is too large",
                    );
                    return;
                }
                let Ok(text) = std::str::from_utf8(text) else {
                    self.reply(request, ffi::status::BAD_SFZ, b"the SFZ text is not UTF-8");
                    return;
                };
                let token = self.next_token;
                self.next_token += 1;
                entry.load = Some(token);
                Job::Begin {
                    part,
                    token,
                    text: text.to_owned(),
                    request,
                }
            }
            page::SFZ_FILE => {
                let length = protocol::u16_at(body, 2).map(usize::from);
                let Some((path, bytes)) = length.and_then(|length| {
                    let path = std::str::from_utf8(body.get(4..4 + length)?).ok()?;
                    Some((path.to_owned(), body[4 + length..].to_vec()))
                }) else {
                    self.reply(request, status::BAD_MESSAGE, &[]);
                    return;
                };
                let Some(token) = entry.load else {
                    self.reply(request, ffi::status::NO_LOAD, &[]);
                    return;
                };
                Job::File {
                    part,
                    token,
                    path,
                    bytes,
                    request,
                }
            }
            page::SFZ_FINISH => {
                let Some(token) = entry.load else {
                    self.reply(request, ffi::status::NO_LOAD, &[]);
                    return;
                };
                Job::Finish {
                    part,
                    token,
                    kit,
                    config,
                    request,
                }
            }
            _ => {
                entry.load = None;
                self.reply(request, status::OK, &[]);
                Job::Abandon { part }
            }
        };
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    fn query(&mut self, request: u32, body: &[u8]) {
        let (Some(what), argument) = (protocol::u32_at(body, 0), protocol::u32_at(body, 4)) else {
            self.reply(request, status::BAD_MESSAGE, &[]);
            return;
        };
        let part = argument.and_then(|value| u16::try_from(value).ok());
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let config = session.config;
        if session.scratch.is_none()
            && matches!(what, query::PARAMS_JSON | query::MASTER_PARAMS_JSON)
        {
            session.scratch = Engine::new(config).ok();
        }
        let (code, payload): (i32, Vec<u8>) = match what {
            query::DRUM_MAP_JSON => (status::OK, drum_map_json().into_bytes()),
            query::PARAMS_JSON => {
                match part.and_then(|part| session.shadow.parts.get(&part).copied()) {
                    None => (ffi::status::NO_SUCH_PART, Vec::new()),
                    Some(entry) => {
                        let json = session.scratch.as_mut().and_then(|engine| {
                            engine.create_part_with(0, entry.kind, entry.chain).ok()?;
                            engine.params_json(0)
                        });
                        match json {
                            Some(json) => (status::OK, json.into_bytes()),
                            None => (status::NOT_READY, Vec::new()),
                        }
                    }
                }
            }
            query::MASTER_PARAMS_JSON => match session.scratch.as_ref() {
                Some(engine) => (status::OK, engine.master_params_json().into_bytes()),
                None => (status::NOT_READY, Vec::new()),
            },
            query::PART_CHAIN => match part.and_then(|part| session.shadow.parts.get(&part)) {
                Some(entry) => (entry.chain.bits() as i32, Vec::new()),
                None => (ffi::status::NO_SUCH_PART, Vec::new()),
            },
            query::STATS => (status::OK, self.status().to_bytes()),
            query::LATENCY => {
                let latency = self
                    .shared
                    .latency
                    .load(std::sync::atomic::Ordering::Acquire);
                let block = self
                    .shared
                    .max_block
                    .load(std::sync::atomic::Ordering::Acquire);
                let rate = self.shared.sample_rate().max(1.0);
                let ahead = 2.0 * f64::from(block) / rate + 0.03;
                let mut payload = latency.to_le_bytes().to_vec();
                payload.extend_from_slice(&block.to_le_bytes());
                payload.extend_from_slice(&ahead.to_le_bytes());
                (status::OK, payload)
            }
            _ => (status::UNSUPPORTED, Vec::new()),
        };
        self.reply(request, code, &payload);
    }

    fn status(&self) -> protocol::Status {
        let mut status = self.shared.snapshot().status;
        status.overridden += self.overridden;
        status.overflow += self.dropped;
        status
    }

    fn queue(&mut self, item: ToAudio) {
        if self.backlog.is_empty() {
            if let Err(item) = self.to_audio.push(item) {
                self.backlog.push_back(item);
            }
        } else if self.backlog.len() < MAX_BACKLOG || matches!(item, ToAudio::Command(_)) {
            self.backlog.push_back(item);
        } else {
            self.dropped += 1;
        }
    }

    /// Moves what waits into the queue to the audio thread, and answers
    /// syncs once nothing waits.
    fn flush(&mut self) {
        while let Some(item) = self.backlog.pop_front() {
            if let Err(item) = self.to_audio.push(item) {
                self.backlog.push_front(item);
                break;
            }
        }
        if self.backlog.is_empty() {
            let syncs = self
                .session
                .as_mut()
                .map(|session| std::mem::take(&mut session.syncs))
                .unwrap_or_default();
            for request in syncs {
                self.reply(request, status::OK, &[]);
            }
        }
    }

    /// Drops what the audio thread hands back, and answers what is done.
    fn collect(&mut self) {
        while let Some(back) = self.from_audio.pop() {
            match back {
                FromAudio::Done(command, code) => {
                    if let Command::Sfz(_, slot, request) = *command {
                        let summary = self.loads.remove(&request).unwrap_or_default();
                        let make_up = slot.make_up_db();
                        if code == ffi::status::OK {
                            let json = format!(
                                "{},\"make_up\":{}}}",
                                summary.trim_end_matches('}'),
                                make_up.map_or("null".to_owned(), |db| format!("{db}"))
                            );
                            self.reply(request, code, json.as_bytes());
                        } else {
                            self.reply(request, code, b"the part could not take the instrument");
                        }
                        drop(slot);
                    }
                }
                FromAudio::Finished(instrument) => drop(instrument),
            }
        }
        while let Ok(done) = self.done.try_recv() {
            match done {
                Done::Reply(request, code, payload) => self.reply(request, code, &payload),
                Done::Loaded {
                    part,
                    token,
                    request,
                    sfz,
                    summary,
                } => {
                    let current = self.session.as_ref().is_some_and(|session| {
                        session.shadow.parts.get(&part).is_some_and(|entry| {
                            entry.load == Some(token)
                                && (entry.kind == InstrumentKind::SamplerKit) == sfz.is_kit()
                        })
                    });
                    if !current {
                        self.reply(request, status::STALE, b"the part changed while it loaded");
                        continue;
                    }
                    if let Some(entry) = self
                        .session
                        .as_mut()
                        .and_then(|session| session.shadow.parts.get_mut(&part))
                    {
                        entry.load = None;
                    }
                    self.loads.insert(request, summary);
                    self.queue(ToAudio::Command(Box::new(Command::Sfz(
                        part, *sfz, request,
                    ))));
                }
            }
        }
    }

    /// Sends the transport and the counters when they are due.
    fn report(&mut self) {
        let generation = self
            .shared
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        let snapshot = self.shared.snapshot();
        let now = Instant::now();
        if snapshot.transport.block != self.block_seen.0 {
            self.block_seen = (snapshot.transport.block, now);
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !session.hello {
            return;
        }
        if generation != self.generation {
            self.generation = generation;
            session.shadow = Shadow::default();
            session.scratch = None;
            let rate = self.shared.sample_rate();
            let quantum = self
                .shared
                .max_quantum
                .load(std::sync::atomic::Ordering::Acquire) as usize;
            session.config = engine_config(rate, quantum);
            let mut body = protocol::notice::ENGINE_RESET.to_le_bytes().to_vec();
            body.extend_from_slice(
                b"the host changed the sample rate or block size: make the parts again",
            );
            let message = protocol::message(plugin::NOTICE, 0, &body);
            self.send(&message);
            return;
        }
        let transport = snapshot.transport;
        let key = (
            transport.epoch,
            transport.playing,
            transport.tempo.to_bits(),
        );
        let every = if transport.playing {
            TRANSPORT_EVERY
        } else {
            TRANSPORT_EVERY_STOPPED
        };
        let changed = session.sent != Some(key);
        if changed || session.last_transport.elapsed() >= every {
            session.sent = Some(key);
            session.last_transport = now;
            let mut transport = transport;
            transport.age = self.block_seen.1.elapsed().as_secs_f64();
            let message = protocol::message(plugin::TRANSPORT, 0, &transport.to_bytes());
            self.send(&message);
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.last_status.elapsed() >= STATUS_EVERY {
            session.last_status = now;
            let message = protocol::message(plugin::STATUS, 0, &self.status().to_bytes());
            self.send(&message);
        }
    }
}

/// The effects of either chain.
fn union(a: Chain, b: Chain) -> Chain {
    Chain::from_bits(a.bits() | b.bits()).unwrap_or(a)
}

/// Reads an HTTP head, up to [`ws::MAX_HANDSHAKE_BYTES`]; returns it and
/// whatever came after it.
fn read_head(stream: &mut TcpStream) -> Option<(Vec<u8>, Vec<u8>)> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = ws::head_end(&bytes) {
            let rest = bytes.split_off(end);
            return Some((bytes, rest));
        }
        if bytes.len() > ws::MAX_HANDSHAKE_BYTES || started.elapsed() > HANDSHAKE_TIMEOUT {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
        }
    }
}

/// Writes `text` as a JSON string.
fn json_string(text: &str) -> String {
    let mut json = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            control if u32::from(control) < 0x20 => {
                json.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => json.push(other),
        }
    }
    json.push('"');
    json
}

fn summary(instrument: &SfzInstrument) -> String {
    let warnings: Vec<String> = instrument
        .warnings()
        .iter()
        .map(|warning| json_string(&warning.to_string()))
        .collect();
    format!(
        "{{\"regions\":{},\"samples\":{},\"sample_bytes\":{},\"warnings\":[{}]}}",
        instrument.num_regions(),
        instrument.num_samples(),
        instrument.sample_bytes(),
        warnings.join(",")
    )
}

/// A load under way: its token, the instrument being built, and the first
/// error a file gave.
type Load = (u64, Option<SfzLoad>, Option<(i32, String)>);

/// The loading thread: parses, decodes and measures SFZ instruments.
fn load_worker(jobs: &Receiver<Job>, done: &Sender<Done>) {
    let mut loads: HashMap<u16, Load> = HashMap::new();
    while let Ok(job) = jobs.recv() {
        let answer = match job {
            Job::Begin {
                part,
                token,
                text,
                request,
            } => match SfzLoad::new(&text) {
                Ok(load) => {
                    let files = load.missing_json();
                    loads.insert(part, (token, Some(load), None));
                    Done::Reply(request, ffi::status::OK, files.into_bytes())
                }
                Err(error) => {
                    loads.remove(&part);
                    Done::Reply(
                        request,
                        ffi::status::BAD_SFZ,
                        error.to_string().into_bytes(),
                    )
                }
            },
            Job::File {
                part,
                token,
                path,
                bytes,
                request,
            } => match loads.get_mut(&part) {
                Some((current, Some(load), failed)) if *current == token => {
                    match load.add_file(&path, bytes) {
                        Ok(_) => Done::Reply(request, ffi::status::OK, Vec::new()),
                        Err(error) => {
                            let text = format!("{path}: {error}");
                            *failed = Some((ffi::status::BAD_SAMPLE, text.clone()));
                            Done::Reply(request, ffi::status::BAD_SAMPLE, text.into_bytes())
                        }
                    }
                }
                _ => Done::Reply(request, status::STALE, Vec::new()),
            },
            Job::Finish {
                part,
                token,
                kit,
                config,
                request,
            } => match loads.remove(&part) {
                Some((current, Some(load), failed)) if current == token => match failed {
                    Some((code, text)) => Done::Reply(request, code, text.into_bytes()),
                    None => match load.finish() {
                        Err(error) => Done::Reply(
                            request,
                            ffi::status::BAD_SAMPLE,
                            error.to_string().into_bytes(),
                        ),
                        Ok(instrument) => {
                            let summary = summary(&instrument);
                            match DetachedSfz::measured(&config, instrument, kit) {
                                Ok(sfz) => Done::Loaded {
                                    part,
                                    token,
                                    request,
                                    sfz: Box::new(sfz),
                                    summary,
                                },
                                Err(_) => {
                                    Done::Reply(request, ffi::status::INVALID_CONFIG, Vec::new())
                                }
                            }
                        }
                    },
                },
                other => {
                    if let Some(other) = other {
                        loads.insert(part, other);
                    }
                    Done::Reply(request, status::STALE, Vec::new())
                }
            },
            Job::Abandon { part } => {
                loads.remove(&part);
                continue;
            }
        };
        if done.send(answer).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\u000ad\"");
    }
}

//! A small blocking client of the bridge, in Rust: what the tests drive
//! the plugin with over a real socket, and a starting point for a native
//! program that plays through the plugin. The browser's client is
//! `crates/manguebyte-web/js/bridge.js`.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use manguebyte_web::{Chain, InstrumentKind, ffi};

use crate::base64;
use crate::protocol::{
    self, BeatRecord, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, SUBPROTOCOL, Status, Transport, Welcome,
    page, plugin,
};
use crate::ws::{self, Decoder, Message, opcode};

/// How long a request waits for its reply.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a client could not do what it was asked.
#[derive(Debug)]
pub enum ClientError {
    /// The socket failed.
    Io(io::Error),
    /// The server refused the handshake, with this HTTP status and text.
    Refused(u16, String),
    /// The server closed the connection, with this code if it gave one.
    Closed(Option<u16>),
    /// The reply did not come in time.
    Timeout,
    /// The server sent what the protocol does not allow.
    Protocol(String),
}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl core::fmt::Display for ClientError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Refused(status, text) => write!(f, "refused with {status}: {text}"),
            Self::Closed(code) => write!(f, "closed ({code:?})"),
            Self::Timeout => f.write_str("no reply in time"),
            Self::Protocol(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for ClientError {}

/// A reply: its status and what came with it.
pub type Reply = (i32, Vec<u8>);

/// A connection to a bridge plugin.
#[derive(Debug)]
pub struct Client {
    stream: TcpStream,
    decoder: Decoder,
    next_request: u32,
    state: u64,
    /// Messages that arrived while a reply was awaited.
    inbox: VecDeque<Vec<u8>>,
    /// The latest transport received.
    pub transport: Option<Transport>,
    /// The latest counters received.
    pub status: Option<Status>,
}

impl Client {
    /// Connects to the plugin on `port` as a page from `origin` would.
    ///
    /// # Errors
    ///
    /// [`ClientError::Refused`] if the plugin refuses the handshake.
    pub fn connect(port: u16, origin: &str) -> Result<Self, ClientError> {
        Self::connect_offering(port, origin, SUBPROTOCOL, &format!("127.0.0.1:{port}"))
    }

    /// Connects offering `protocol`, with `host` as the `Host` header.
    ///
    /// # Errors
    ///
    /// As [`connect`](Self::connect)'s.
    pub fn connect_offering(
        port: u16,
        origin: &str,
        protocol: &str,
        host: &str,
    ) -> Result<Self, ClientError> {
        let mut stream = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut state = seed();
        let key_bytes: Vec<u8> = (0..16).map(|_| next(&mut state) as u8).collect();
        let key = base64::encode(&key_bytes);
        stream.write_all(&ws::client_request(host, origin, &key, protocol))?;
        let mut head = Vec::new();
        let mut chunk = [0u8; 1024];
        let end = loop {
            if let Some(end) = ws::head_end(&head) {
                break end;
            }
            let count = stream.read(&mut chunk)?;
            if count == 0 {
                return Err(ClientError::Refused(
                    0,
                    String::from_utf8_lossy(&head).into_owned(),
                ));
            }
            head.extend_from_slice(&chunk[..count]);
        };
        let rest = head.split_off(end);
        if let Err((status, text)) = ws::check_response(&head, &key, protocol) {
            let mut text = text;
            text.push_str(&String::from_utf8_lossy(&rest));
            return Err(ClientError::Refused(status, text));
        }
        let mut decoder = Decoder::new(false, MAX_MESSAGE_BYTES);
        decoder.push(&rest);
        stream.set_read_timeout(Some(Duration::from_millis(20)))?;
        Ok(Self {
            stream,
            decoder,
            next_request: 1,
            state,
            inbox: VecDeque::new(),
            transport: None,
            status: None,
        })
    }

    /// Sends one frame as it is given, masked with a fresh key.
    ///
    /// # Errors
    ///
    /// If the socket fails.
    pub fn send_frame(&mut self, fin: bool, kind: u8, payload: &[u8]) -> Result<(), ClientError> {
        let mask = (next(&mut self.state) as u32).to_le_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 14);
        ws::encode_frame(&mut frame, fin, kind, payload, Some(mask));
        self.stream.write_all(&frame)?;
        Ok(())
    }

    /// Sends raw bytes, whatever they are.
    ///
    /// # Errors
    ///
    /// If the socket fails.
    pub fn send_raw(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        self.stream.write_all(bytes)?;
        Ok(())
    }

    /// Sends a message of type `kind` and returns its request id.
    ///
    /// # Errors
    ///
    /// If the socket fails.
    pub fn send(&mut self, kind: u8, body: &[u8]) -> Result<u32, ClientError> {
        let request = self.next_request;
        self.next_request += 1;
        self.send_frame(
            true,
            opcode::BINARY,
            &protocol::message(kind, request, body),
        )?;
        Ok(request)
    }

    /// The next message or control frame, waiting up to `timeout`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Closed`] once the server closes, and protocol
    /// failures.
    pub fn next_message(&mut self, timeout: Duration) -> Result<Option<Message>, ClientError> {
        let started = Instant::now();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match self.decoder.next_message() {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => {}
                Err(failure) => return Err(ClientError::Protocol(failure.reason.to_owned())),
            }
            if started.elapsed() >= timeout {
                return Ok(None);
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ClientError::Closed(None)),
                Ok(count) => self.decoder.push(&chunk[..count]),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Reads one binary message, keeping track of transport and status
    /// messages, answering pings.
    fn pump(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, ClientError> {
        match self.next_message(timeout)? {
            None => Ok(None),
            Some(Message::Binary(bytes)) => {
                if let Some((header, body)) = protocol::split(&bytes) {
                    match header.kind {
                        plugin::TRANSPORT => self.transport = Transport::from_bytes(body),
                        plugin::STATUS => self.status = Status::from_bytes(body),
                        _ => {}
                    }
                }
                Ok(Some(bytes))
            }
            Some(Message::Ping(data)) => {
                self.send_frame(true, opcode::PONG, &data)?;
                Ok(None)
            }
            Some(Message::Close(code, _)) => Err(ClientError::Closed(code)),
            Some(_) => Ok(None),
        }
    }

    /// Waits for the message of type `kind` answering `request`.
    fn await_answer(&mut self, kind: u8, request: u32) -> Result<Vec<u8>, ClientError> {
        let started = Instant::now();
        while started.elapsed() < REPLY_TIMEOUT {
            if let Some(bytes) = self.pump(Duration::from_millis(50))? {
                match protocol::split(&bytes) {
                    Some((header, _)) if header.kind == kind && header.request == request => {
                        return Ok(bytes);
                    }
                    _ => self.inbox.push_back(bytes),
                }
            }
        }
        Err(ClientError::Timeout)
    }

    /// Sends a request and waits for its reply.
    ///
    /// # Errors
    ///
    /// If the connection fails or the reply does not come.
    pub fn request(&mut self, kind: u8, body: &[u8]) -> Result<Reply, ClientError> {
        let request = self.send(kind, body)?;
        let bytes = self.await_answer(plugin::REPLY, request)?;
        let (_, body) =
            protocol::split(&bytes).ok_or_else(|| ClientError::Protocol("short".into()))?;
        let status =
            protocol::i32_at(body, 0).ok_or_else(|| ClientError::Protocol("short".into()))?;
        Ok((status, body[4..].to_vec()))
    }

    /// Says hello, as protocol `version`.
    ///
    /// # Errors
    ///
    /// If the plugin refuses.
    pub fn hello_as(&mut self, version: u32) -> Result<Welcome, ClientError> {
        let mut body = version.to_le_bytes().to_vec();
        body.extend_from_slice(&ffi::ABI_VERSION.to_le_bytes());
        body.extend_from_slice(b"aratu-bridge client");
        let request = self.send(page::HELLO, &body)?;
        let started = Instant::now();
        while started.elapsed() < REPLY_TIMEOUT {
            if let Some(bytes) = self.pump(Duration::from_millis(50))? {
                if let Some((header, body)) = protocol::split(&bytes) {
                    if header.request == request && header.kind == plugin::HELLO {
                        return Welcome::from_bytes(body)
                            .ok_or_else(|| ClientError::Protocol("a short hello".into()));
                    }
                    if header.request == request && header.kind == plugin::REPLY {
                        let text =
                            String::from_utf8_lossy(body.get(4..).unwrap_or(&[])).into_owned();
                        return Err(ClientError::Protocol(text));
                    }
                }
                self.inbox.push_back(bytes);
            }
        }
        Err(ClientError::Timeout)
    }

    /// Says hello.
    ///
    /// # Errors
    ///
    /// If the plugin refuses.
    pub fn hello(&mut self) -> Result<Welcome, ClientError> {
        self.hello_as(PROTOCOL_VERSION)
    }

    /// Sends events for `epoch`, in messages of at most 4 096 records.
    ///
    /// # Errors
    ///
    /// If the socket fails.
    pub fn events(&mut self, epoch: u32, records: &[BeatRecord]) -> Result<(), ClientError> {
        for chunk in records.chunks(4_096) {
            self.send(page::EVENTS, &protocol::events_body(epoch, chunk))?;
        }
        Ok(())
    }

    /// Makes a part; returns the status.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn create_part(
        &mut self,
        part: u16,
        kind: InstrumentKind,
        chain: Chain,
    ) -> Result<i32, ClientError> {
        let mut body = part.to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&kind.code().to_le_bytes());
        body.extend_from_slice(&chain.bits().to_le_bytes());
        Ok(self.request(page::CREATE_PART, &body)?.0)
    }

    /// Gives a part another instrument; returns the status.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn set_instrument(&mut self, part: u16, kind: InstrumentKind) -> Result<i32, ClientError> {
        let mut body = part.to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&kind.code().to_le_bytes());
        Ok(self.request(page::SET_INSTRUMENT, &body)?.0)
    }

    /// Sets a part's chain; returns the status.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn set_chain(&mut self, part: u16, chain: Chain) -> Result<i32, ClientError> {
        let mut body = part.to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&chain.bits().to_le_bytes());
        Ok(self.request(page::SET_CHAIN, &body)?.0)
    }

    /// Removes a part; returns the status.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn remove_part(&mut self, part: u16) -> Result<i32, ClientError> {
        Ok(self.request(page::REMOVE_PART, &part.to_le_bytes())?.0)
    }

    /// Loads an SFZ instrument into a sampler part, file by file, and
    /// returns the plugin's summary once the part plays it.
    ///
    /// # Errors
    ///
    /// The status and text of a refused step.
    pub fn load_sfz(
        &mut self,
        part: u16,
        text: &str,
        files: &[(String, Vec<u8>)],
    ) -> Result<Result<String, Reply>, ClientError> {
        let mut body = part.to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(text.as_bytes());
        let (status, listed) = self.request(page::SFZ_BEGIN, &body)?;
        if status != 0 {
            return Ok(Err((status, listed)));
        }
        for (path, bytes) in files {
            let mut body = part.to_le_bytes().to_vec();
            body.extend_from_slice(&(path.len() as u16).to_le_bytes());
            body.extend_from_slice(path.as_bytes());
            body.extend_from_slice(bytes);
            let reply = self.request(page::SFZ_FILE, &body)?;
            if reply.0 != 0 {
                return Ok(Err(reply));
            }
        }
        let (status, summary) = self.request(page::SFZ_FINISH, &part.to_le_bytes())?;
        if status != 0 {
            return Ok(Err((status, summary)));
        }
        Ok(Ok(String::from_utf8_lossy(&summary).into_owned()))
    }

    /// Waits until every message sent so far has reached the audio thread's
    /// queue.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn sync(&mut self) -> Result<(), ClientError> {
        let (status, _) = self.request(page::SYNC, &[])?;
        if status == 0 {
            Ok(())
        } else {
            Err(ClientError::Protocol(format!("sync: {status}")))
        }
    }

    /// Asks a query.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn query(&mut self, what: u32, argument: u32) -> Result<Reply, ClientError> {
        let mut body = what.to_le_bytes().to_vec();
        body.extend_from_slice(&argument.to_le_bytes());
        self.request(page::QUERY, &body)
    }

    /// Reads messages for `duration`, keeping the latest transport and
    /// status.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn listen(&mut self, duration: Duration) -> Result<(), ClientError> {
        let started = Instant::now();
        while started.elapsed() < duration {
            if let Some(bytes) = self.pump(Duration::from_millis(10))? {
                self.inbox.push_back(bytes);
            }
        }
        Ok(())
    }

    /// Waits up to `timeout` for a transport that `wanted` accepts.
    ///
    /// # Errors
    ///
    /// [`ClientError::Timeout`] if none comes.
    pub fn wait_transport(
        &mut self,
        timeout: Duration,
        wanted: impl Fn(&Transport) -> bool,
    ) -> Result<Transport, ClientError> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if let Some(transport) = self.transport.filter(|transport| wanted(transport)) {
                return Ok(transport);
            }
            if let Some(bytes) = self.pump(Duration::from_millis(10))? {
                self.inbox.push_back(bytes);
            }
        }
        Err(ClientError::Timeout)
    }

    /// The messages that arrived while replies were awaited, oldest first.
    pub fn take_inbox(&mut self) -> Vec<Vec<u8>> {
        self.inbox.drain(..).collect()
    }

    /// Closes the connection with `code`.
    ///
    /// # Errors
    ///
    /// If the socket fails.
    pub fn close(mut self, code: u16) -> Result<(), ClientError> {
        self.send_frame(true, opcode::CLOSE, &ws::close_payload(code, "bye"))?;
        let _ = self.next_message(Duration::from_millis(200));
        Ok(())
    }
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos() as u64);
    let local = &nanos as *const u64 as u64;
    (nanos ^ local.rotate_left(32)) | 1
}

/// xorshift64*: masks need not be secret from the server, only varied.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

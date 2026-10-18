//! The WebSocket protocol, RFC 6455, as much of it as the bridge needs: the
//! opening handshake on both sides, and frames and messages, masked from a
//! client and unmasked from a server, fragmented or whole, with pings,
//! pongs and the closing handshake. No extensions are offered or accepted.
//!
//! Nothing here touches a socket: the handshake works on the bytes of the
//! request and the response, and [`Decoder`] is fed whatever bytes
//! arrive, in any pieces, so every rule is tested without a network.

use crate::{base64, sha1};

/// The GUID RFC 6455 section 1.3 appends to the key.
pub const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The largest opening handshake read, in bytes; a longer one is refused.
pub const MAX_HANDSHAKE_BYTES: usize = 8 * 1024;

/// The payload of a control frame is at most this long (section 5.5).
pub const MAX_CONTROL_PAYLOAD: usize = 125;

/// Close codes (section 7.4.1).
pub mod close {
    /// The purpose of the connection has been fulfilled.
    pub const NORMAL: u16 = 1000;
    /// The endpoint is going away (the plugin is being unloaded).
    pub const GOING_AWAY: u16 = 1001;
    /// A protocol error.
    pub const PROTOCOL: u16 = 1002;
    /// A type of data the endpoint cannot accept (text, for the bridge).
    pub const UNSUPPORTED: u16 = 1003;
    /// Data inconsistent with the type of the message (bad UTF-8).
    pub const INVALID_DATA: u16 = 1007;
    /// A message that violates the endpoint's policy.
    pub const POLICY: u16 = 1008;
    /// A message too big to process.
    pub const TOO_BIG: u16 = 1009;
}

/// Frame opcodes (section 5.2).
pub mod opcode {
    /// The rest of a fragmented message.
    pub const CONTINUATION: u8 = 0x0;
    /// A text message.
    pub const TEXT: u8 = 0x1;
    /// A binary message.
    pub const BINARY: u8 = 0x2;
    /// The closing handshake.
    pub const CLOSE: u8 = 0x8;
    /// A ping, answered by a pong.
    pub const PING: u8 = 0x9;
    /// A pong.
    pub const PONG: u8 = 0xA;
}

/// The value of `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`
/// (section 4.2.2): the base 64 of the SHA-1 of the key and [`GUID`].
pub fn accept_key(key: &str) -> String {
    let mut joined = key.trim().as_bytes().to_vec();
    joined.extend_from_slice(GUID.as_bytes());
    base64::encode(&sha1::digest(&joined))
}

/// An HTTP request line and its headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method, such as `GET`.
    pub method: String,
    /// The request target, such as `/`.
    pub target: String,
    /// The version, such as `HTTP/1.1`.
    pub version: String,
    /// The headers in order, names in lower case, values trimmed.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// The value of header `name` (in lower case); several headers of the
    /// same name are joined with commas, as HTTP allows.
    pub fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self
            .headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    /// Whether header `name` lists `token` among its comma-separated
    /// values, ignoring case.
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.header(name).is_some_and(|value| {
            value
                .split(',')
                .any(|item| item.trim().eq_ignore_ascii_case(token))
        })
    }
}

/// Where the end of an HTTP head is in `bytes`: just past its blank line.
pub fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

/// Parses an HTTP head (up to and including its blank line) as a request.
/// `None` if it is not one.
pub fn parse_request(head: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target, version) = (first.next()?, first.next()?, first.next()?);
    if first.next().is_some() || method.is_empty() || target.is_empty() {
        return None;
    }
    Some(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        version: version.to_owned(),
        headers: parse_headers(lines)?,
    })
}

/// The header lines of an HTTP head, names in lower case.
fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Vec<(String, String)>> {
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None;
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    Some(headers)
}

/// Why an opening handshake was refused, with the HTTP status sent back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The HTTP status code: 400, 403, 404, 409 or 426.
    pub status: u16,
    /// What is wrong, sent as the body.
    pub reason: String,
}

impl Refusal {
    fn new(status: u16, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
        }
    }

    /// The HTTP response that refuses the handshake. A 426 names the
    /// version this side speaks (section 4.4).
    pub fn response(&self) -> Vec<u8> {
        let phrase = match self.status {
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            426 => "Upgrade Required",
            _ => "Error",
        };
        let version = if self.status == 426 {
            "Sec-WebSocket-Version: 13\r\n"
        } else {
            ""
        };
        format!(
            "HTTP/1.1 {} {phrase}\r\n{version}Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status,
            self.reason.len(),
            self.reason
        )
        .into_bytes()
    }
}

/// What the server checks a handshake against.
pub struct Policy<'a> {
    /// The port the server listens on, which the `Host` header must name.
    pub port: u16,
    /// The subprotocol the client must offer.
    pub protocol: &'a str,
    /// Whether an `Origin` is allowed.
    pub origin_allowed: &'a dyn Fn(&str) -> bool,
}

/// Checks a client's opening handshake (section 4.2.1) and returns the
/// response that accepts it, or why it is refused.
///
/// Besides what RFC 6455 asks, the `Host` header must be the loopback
/// address or `localhost` with the server's port (which keeps a page that
/// rebinds a name of its own to 127.0.0.1 out), the `Origin` header must
/// be present and allowed, and the client must offer the bridge's
/// subprotocol.
pub fn accept(request: &Request, policy: &Policy<'_>) -> Result<Vec<u8>, Refusal> {
    if request.method != "GET" || request.version != "HTTP/1.1" {
        return Err(Refusal::new(
            400,
            "a WebSocket handshake is an HTTP/1.1 GET",
        ));
    }
    if request.target != "/" && !request.target.starts_with("/?") {
        return Err(Refusal::new(404, "the bridge listens on /"));
    }
    if !request.has_token("upgrade", "websocket") || !request.has_token("connection", "upgrade") {
        return Err(Refusal::new(400, "not a WebSocket upgrade"));
    }
    if request.header("sec-websocket-version").as_deref() != Some("13") {
        return Err(Refusal::new(426, "only WebSocket version 13 is spoken"));
    }
    let key = request
        .header("sec-websocket-key")
        .filter(|key| base64::decode(key).is_some_and(|bytes| bytes.len() == 16))
        .ok_or_else(|| Refusal::new(400, "Sec-WebSocket-Key is not 16 bytes in base 64"))?;
    let host_ok = request.header("host").is_some_and(|host| {
        let port = policy.port.to_string();
        ["127.0.0.1", "localhost", "[::1]"]
            .iter()
            .any(|name| host.eq_ignore_ascii_case(&format!("{name}:{port}")))
    });
    if !host_ok {
        return Err(Refusal::new(
            403,
            "the Host must be 127.0.0.1 or localhost with the port",
        ));
    }
    let origin = request
        .header("origin")
        .ok_or_else(|| Refusal::new(403, "a page must send its Origin"))?;
    if !(policy.origin_allowed)(&origin) {
        return Err(Refusal::new(
            403,
            format!("the origin {origin} is not allowed"),
        ));
    }
    if !request.has_token("sec-websocket-protocol", policy.protocol) {
        return Err(Refusal::new(
            400,
            format!("the client must offer the subprotocol {}", policy.protocol),
        ));
    }
    Ok(format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: {}\r\n\r\n",
        accept_key(&key),
        policy.protocol
    )
    .into_bytes())
}

/// A client's opening handshake: what [`accept`] checks.
pub fn client_request(host: &str, origin: &str, key: &str, protocol: &str) -> Vec<u8> {
    format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
    )
    .into_bytes()
}

/// Checks a server's response to [`client_request`] (section 4.1): the
/// status line, the upgrade, the accept key and the subprotocol. Returns
/// the status code and the body of a refusal as the error.
pub fn check_response(head: &[u8], key: &str, protocol: &str) -> Result<(), (u16, String)> {
    let text = String::from_utf8_lossy(head);
    let status: u16 = text
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let headers = std::str::from_utf8(head)
        .ok()
        .and_then(|text| parse_headers(text.split("\r\n").skip(1)))
        .ok_or((status, "not an HTTP response".to_owned()))?;
    let request = Request {
        method: String::new(),
        target: String::new(),
        version: String::new(),
        headers,
    };
    if status != 101 {
        return Err((status, text.into_owned()));
    }
    let fine = request.has_token("upgrade", "websocket")
        && request.has_token("connection", "upgrade")
        && request.header("sec-websocket-accept").as_deref() == Some(accept_key(key).as_str())
        && request.header("sec-websocket-protocol").as_deref() == Some(protocol);
    if fine {
        Ok(())
    } else {
        Err((
            status,
            "the response does not accept the handshake".to_owned(),
        ))
    }
}

/// Writes one frame (section 5.2): `fin` set on the last frame of a
/// message, `mask` for a frame from a client.
pub fn encode_frame(
    out: &mut Vec<u8>,
    fin: bool,
    opcode: u8,
    payload: &[u8],
    mask: Option<[u8; 4]>,
) {
    out.push(if fin { 0x80 } else { 0 } | (opcode & 0x0f));
    let masked = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        length @ 0..=125 => out.push(masked | length as u8),
        length @ 126..=0xffff => {
            out.push(masked | 126);
            out.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            out.push(masked | 127);
            out.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            out.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(index, byte)| byte ^ key[index % 4]),
            );
        }
        None => out.extend_from_slice(payload),
    }
}

/// A whole message, or a control frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A binary message.
    Binary(Vec<u8>),
    /// A text message, valid UTF-8.
    Text(String),
    /// A ping, with its application data.
    Ping(Vec<u8>),
    /// A pong.
    Pong(Vec<u8>),
    /// The other side closes, with its code and reason if it gave them.
    Close(Option<u16>, String),
}

/// Why the connection must be failed (section 7.1.7), with the close code
/// to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The close code.
    pub code: u16,
    /// What went wrong.
    pub reason: &'static str,
}

fn fail(code: u16, reason: &'static str) -> Failure {
    Failure { code, reason }
}

fn valid_close_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

/// Reads frames out of the bytes that arrive and puts messages together.
///
/// A server's decoder insists on masked frames and a client's on unmasked
/// ones (section 5.1). A frame or message longer than the limit fails the
/// connection with [`close::TOO_BIG`] as soon as its header says so,
/// before its payload is read.
#[derive(Debug)]
pub struct Decoder {
    buffer: Vec<u8>,
    start: usize,
    masked: bool,
    max_message: usize,
    /// The opcode and the bytes of a fragmented message under way.
    partial: Option<(u8, Vec<u8>)>,
}

impl Decoder {
    /// A decoder for a server (`masked`, frames from a client) or a client,
    /// taking messages up to `max_message` bytes.
    pub fn new(masked: bool, max_message: usize) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            masked,
            max_message,
            partial: None,
        }
    }

    /// Adds bytes that arrived.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Bytes received and not yet read as frames.
    pub fn buffered(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// The next message or control frame, `None` if more bytes are needed.
    ///
    /// # Errors
    ///
    /// A [`Failure`] once the bytes break the protocol; the connection is
    /// then to be closed with its code.
    pub fn next_message(&mut self) -> Result<Option<Message>, Failure> {
        loop {
            let Some((fin, opcode, payload)) = self.frame()? else {
                return Ok(None);
            };
            if opcode >= 0x8 {
                return self.control(opcode, payload).map(Some);
            }
            match (opcode, self.partial.as_mut()) {
                (opcode::CONTINUATION, None) => {
                    return Err(fail(
                        close::PROTOCOL,
                        "a continuation with nothing to continue",
                    ));
                }
                (opcode::CONTINUATION, Some((_, bytes))) => {
                    if bytes.len() + payload.len() > self.max_message {
                        return Err(fail(close::TOO_BIG, "the message is too big"));
                    }
                    bytes.extend_from_slice(&payload);
                }
                (_, Some(_)) => {
                    return Err(fail(
                        close::PROTOCOL,
                        "a new message inside a fragmented one",
                    ));
                }
                (opcode::TEXT | opcode::BINARY, None) => self.partial = Some((opcode, payload)),
                _ => return Err(fail(close::PROTOCOL, "an unknown opcode")),
            }
            if fin {
                let Some((opcode, bytes)) = self.partial.take() else {
                    continue;
                };
                return if opcode == opcode::TEXT {
                    String::from_utf8(bytes)
                        .map(|text| Some(Message::Text(text)))
                        .map_err(|_| fail(close::INVALID_DATA, "text that is not UTF-8"))
                } else {
                    Ok(Some(Message::Binary(bytes)))
                };
            }
        }
    }

    fn control(&self, opcode: u8, payload: Vec<u8>) -> Result<Message, Failure> {
        match opcode {
            opcode::PING => Ok(Message::Ping(payload)),
            opcode::PONG => Ok(Message::Pong(payload)),
            opcode::CLOSE => match payload.len() {
                0 => Ok(Message::Close(None, String::new())),
                1 => Err(fail(close::PROTOCOL, "a close payload of one byte")),
                _ => {
                    let code = u16::from_be_bytes([payload[0], payload[1]]);
                    if !valid_close_code(code) {
                        return Err(fail(close::PROTOCOL, "an invalid close code"));
                    }
                    let reason = String::from_utf8(payload[2..].to_vec()).map_err(|_| {
                        fail(close::INVALID_DATA, "a close reason that is not UTF-8")
                    })?;
                    Ok(Message::Close(Some(code), reason))
                }
            },
            _ => Err(fail(close::PROTOCOL, "an unknown control opcode")),
        }
    }

    /// The next whole frame, unmasked.
    fn frame(&mut self) -> Result<Option<(bool, u8, Vec<u8>)>, Failure> {
        let bytes = &self.buffer[self.start..];
        if bytes.len() < 2 {
            return Ok(None);
        }
        let (first, second) = (bytes[0], bytes[1]);
        if first & 0x70 != 0 {
            return Err(fail(close::PROTOCOL, "reserved bits are set"));
        }
        let fin = first & 0x80 != 0;
        let opcode = first & 0x0f;
        let masked = second & 0x80 != 0;
        if masked != self.masked {
            return Err(fail(
                close::PROTOCOL,
                if self.masked {
                    "a client's frame must be masked"
                } else {
                    "a server's frame must not be masked"
                },
            ));
        }
        let (length, mut at) = match second & 0x7f {
            126 => {
                if bytes.len() < 4 {
                    return Ok(None);
                }
                (u64::from(u16::from_be_bytes([bytes[2], bytes[3]])), 4)
            }
            127 => {
                if bytes.len() < 10 {
                    return Ok(None);
                }
                let mut word = [0u8; 8];
                word.copy_from_slice(&bytes[2..10]);
                let length = u64::from_be_bytes(word);
                if length >> 63 != 0 {
                    return Err(fail(close::PROTOCOL, "the length's top bit is set"));
                }
                (length, 10)
            }
            length => (u64::from(length), 2),
        };
        if opcode >= 0x8 {
            if !fin {
                return Err(fail(close::PROTOCOL, "a fragmented control frame"));
            }
            if length > MAX_CONTROL_PAYLOAD as u64 {
                return Err(fail(close::PROTOCOL, "a control frame over 125 bytes"));
            }
        }
        let already = self.partial.as_ref().map_or(0, |(_, bytes)| bytes.len());
        if length > (self.max_message - already.min(self.max_message)) as u64 {
            return Err(fail(close::TOO_BIG, "the message is too big"));
        }
        let length = length as usize;
        let key = if masked {
            if bytes.len() < at + 4 {
                return Ok(None);
            }
            let key = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
            at += 4;
            Some(key)
        } else {
            None
        };
        if bytes.len() < at + length {
            return Ok(None);
        }
        let mut payload = bytes[at..at + length].to_vec();
        if let Some(key) = key {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= key[index % 4];
            }
        }
        self.start += at + length;
        Ok(Some((fin, opcode, payload)))
    }
}

/// The payload of a close frame: `code` and `reason`, cut to fit 125
/// bytes.
pub fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    let mut end = reason.len().min(MAX_CONTROL_PAYLOAD - 2);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..end]);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

    fn server() -> Decoder {
        Decoder::new(true, 1 << 16)
    }

    fn masked(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        encode_frame(&mut out, fin, opcode, payload, Some(MASK));
        out
    }

    #[test]
    fn the_handshake_example_of_rfc_6455() {
        // Section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn the_masked_hello_of_rfc_6455() {
        // Section 5.7: "Hello", masked with 37 fa 21 3d.
        let frame = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        assert_eq!(masked(true, opcode::TEXT, b"Hello"), frame);
        let mut decoder = server();
        decoder.push(&frame);
        assert_eq!(
            decoder.next_message(),
            Ok(Some(Message::Text("Hello".into())))
        );
        // Unmasked, from a server.
        let mut out = Vec::new();
        encode_frame(&mut out, true, opcode::TEXT, b"Hello", None);
        assert_eq!(out, [0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]);
        let mut client = Decoder::new(false, 1 << 16);
        client.push(&out);
        assert_eq!(
            client.next_message(),
            Ok(Some(Message::Text("Hello".into())))
        );
    }

    #[test]
    fn frames_arrive_in_any_pieces_and_with_every_length_form() {
        for length in [0, 1, 125, 126, 127, 65_535, 65_536, 70_000] {
            let payload: Vec<u8> = (0..length).map(|n| (n * 7 + 3) as u8).collect();
            let frame = masked(true, opcode::BINARY, &payload);
            let mut decoder = Decoder::new(true, 1 << 20);
            // One byte at a time for the header, then the rest.
            let split = 14.min(frame.len());
            for byte in &frame[..split] {
                assert_eq!(decoder.next_message(), Ok(None));
                decoder.push(std::slice::from_ref(byte));
            }
            decoder.push(&frame[split..]);
            assert_eq!(
                decoder.next_message(),
                Ok(Some(Message::Binary(payload))),
                "{length}"
            );
            assert_eq!(decoder.next_message(), Ok(None));
            assert_eq!(decoder.buffered(), 0);
        }
    }

    #[test]
    fn a_fragmented_message_is_put_together_around_control_frames() {
        let mut decoder = server();
        let mut bytes = masked(false, opcode::BINARY, b"one ");
        bytes.extend(masked(true, opcode::PING, b"are you there"));
        bytes.extend(masked(false, opcode::CONTINUATION, b"two "));
        bytes.extend(masked(true, opcode::CONTINUATION, b"three"));
        decoder.push(&bytes);
        assert_eq!(
            decoder.next_message(),
            Ok(Some(Message::Ping(b"are you there".to_vec())))
        );
        assert_eq!(
            decoder.next_message(),
            Ok(Some(Message::Binary(b"one two three".to_vec())))
        );
        assert_eq!(decoder.next_message(), Ok(None));
    }

    #[test]
    fn pings_pongs_and_closes() {
        let mut decoder = server();
        decoder.push(&masked(true, opcode::PONG, b""));
        decoder.push(&masked(true, opcode::CLOSE, &close_payload(1000, "bye")));
        decoder.push(&masked(true, opcode::CLOSE, b""));
        assert_eq!(decoder.next_message(), Ok(Some(Message::Pong(Vec::new()))));
        assert_eq!(
            decoder.next_message(),
            Ok(Some(Message::Close(Some(1000), "bye".into())))
        );
        assert_eq!(
            decoder.next_message(),
            Ok(Some(Message::Close(None, String::new())))
        );
        // A reason too long for a control frame is cut, on a character.
        let long = "é".repeat(100);
        let payload = close_payload(4000, &long);
        assert!(payload.len() <= MAX_CONTROL_PAYLOAD);
        assert!(std::str::from_utf8(&payload[2..]).is_ok());
    }

    #[test]
    fn what_breaks_the_protocol_fails_the_connection() {
        let mut long_ping = masked(true, opcode::PING, &[0; 126]);
        let cases: Vec<(Vec<u8>, u16)> = vec![
            // Unmasked from a client.
            (
                {
                    let mut out = Vec::new();
                    encode_frame(&mut out, true, opcode::BINARY, b"x", None);
                    out
                },
                close::PROTOCOL,
            ),
            // Reserved bits.
            (
                {
                    let mut frame = masked(true, opcode::BINARY, b"x");
                    frame[0] |= 0x40;
                    frame
                },
                close::PROTOCOL,
            ),
            (masked(true, 0x3, b"x"), close::PROTOCOL),
            (masked(true, 0xB, b""), close::PROTOCOL),
            (masked(false, opcode::PING, b""), close::PROTOCOL),
            (std::mem::take(&mut long_ping), close::PROTOCOL),
            (masked(true, opcode::CONTINUATION, b"x"), close::PROTOCOL),
            (masked(true, opcode::CLOSE, &[0x03]), close::PROTOCOL),
            (
                masked(true, opcode::CLOSE, &close_payload(1005, "")),
                close::PROTOCOL,
            ),
            (
                masked(true, opcode::CLOSE, &close_payload(2000, "")),
                close::PROTOCOL,
            ),
            (
                masked(true, opcode::TEXT, &[0xff, 0xfe]),
                close::INVALID_DATA,
            ),
            (
                {
                    let mut bytes = masked(false, opcode::TEXT, b"a");
                    bytes.extend(masked(true, opcode::BINARY, b"b"));
                    bytes
                },
                close::PROTOCOL,
            ),
            // A 64-bit length with its top bit set.
            (
                {
                    let mut frame = vec![0x82, 0xff];
                    frame.extend_from_slice(&(1u64 << 63).to_be_bytes());
                    frame.extend_from_slice(&MASK);
                    frame
                },
                close::PROTOCOL,
            ),
        ];
        for (index, (bytes, code)) in cases.into_iter().enumerate() {
            let mut decoder = server();
            decoder.push(&bytes);
            let result = loop {
                match decoder.next_message() {
                    Ok(Some(_)) => continue,
                    other => break other,
                }
            };
            assert_eq!(
                result.map_err(|failure| failure.code),
                Err(code),
                "case {index}"
            );
        }
    }

    #[test]
    fn an_oversized_frame_or_message_is_refused_from_its_header() {
        let mut decoder = Decoder::new(true, 1_000);
        // Only the header of a 1 MiB frame: refused before its payload.
        let frame = masked(true, opcode::BINARY, &vec![0; 1 << 20]);
        decoder.push(&frame[..14]);
        assert_eq!(
            decoder.next_message().map_err(|f| f.code),
            Err(close::TOO_BIG)
        );
        // Fragments that add up to too much.
        let mut decoder = Decoder::new(true, 1_000);
        decoder.push(&masked(false, opcode::BINARY, &[1; 600]));
        decoder.push(&masked(true, opcode::CONTINUATION, &[2; 600]));
        assert_eq!(
            decoder.next_message().map_err(|f| f.code),
            Err(close::TOO_BIG)
        );
        // Exactly the limit is fine.
        let mut decoder = Decoder::new(true, 1_000);
        decoder.push(&masked(false, opcode::BINARY, &[1; 400]));
        decoder.push(&masked(true, opcode::CONTINUATION, &[2; 600]));
        assert!(
            matches!(decoder.next_message(), Ok(Some(Message::Binary(bytes))) if bytes.len() == 1_000)
        );
    }

    fn request(extra: &[(&str, &str)], skip: &[&str]) -> Request {
        let mut headers = vec![
            ("host", "127.0.0.1:47870"),
            ("upgrade", "websocket"),
            ("connection", "keep-alive, Upgrade"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ("sec-websocket-version", "13"),
            ("origin", "https://aratu.live"),
            ("sec-websocket-protocol", "chat, aratu-bridge.1"),
        ];
        headers.retain(|(name, _)| !skip.contains(name));
        let mut text = String::from("GET / HTTP/1.1\r\n");
        for (name, value) in headers.iter().chain(extra) {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("\r\n");
        parse_request(text.as_bytes()).unwrap()
    }

    #[test]
    fn the_server_handshake_accepts_and_refuses() {
        let allowed = |origin: &str| origin == "https://aratu.live";
        let policy = Policy {
            port: 47_870,
            protocol: "aratu-bridge.1",
            origin_allowed: &allowed,
        };
        let response = accept(&request(&[], &[]), &policy).unwrap();
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(check_response(&response, key, "aratu-bridge.1"), Ok(()));
        assert!(String::from_utf8_lossy(&response).contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));

        let refused = |request: Request| accept(&request, &policy).unwrap_err().status;
        assert_eq!(refused(request(&[], &["origin"])), 403);
        assert_eq!(
            refused(request(&[("origin", "https://evil.example")], &["origin"])),
            403
        );
        assert_eq!(
            refused(request(&[("host", "evil.example:47870")], &["host"])),
            403
        );
        assert_eq!(refused(request(&[("host", "127.0.0.1:1")], &["host"])), 403);
        assert_eq!(refused(request(&[], &["sec-websocket-protocol"])), 400);
        assert_eq!(
            refused(request(
                &[("sec-websocket-version", "8")],
                &["sec-websocket-version"]
            )),
            426
        );
        assert_eq!(
            refused(request(
                &[("sec-websocket-key", "c2hvcnQ=")],
                &["sec-websocket-key"]
            )),
            400
        );
        assert_eq!(refused(request(&[], &["upgrade"])), 400);
        let refusal = accept(&request(&[], &["origin"]), &policy).unwrap_err();
        let text = String::from_utf8(refusal.response()).unwrap();
        assert!(text.starts_with("HTTP/1.1 403 Forbidden\r\n"));
        assert!(
            String::from_utf8(Refusal::new(426, "x").response())
                .unwrap()
                .contains("Sec-WebSocket-Version: 13")
        );
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n\r\nrest"), Some(18));
        assert_eq!(parse_request(b"nonsense"), None);
    }
}

//! Sending and receiving over TCP.
//!
//! A TCP connection delivers every byte, in order, with no packet
//! boundaries. The boundaries are put back with the stream framing of OSC
//! 1.1, which is in [`slip`](crate::slip).
//!
//! OSC 1.0 framed packets on a stream differently, with a 32-bit size
//! before each one, and some programs still do. That framing is not
//! implemented here.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::decode::OscPacketRef;
use crate::encode::{encode_message, encoded_message_len};
use crate::error::OscError;
use crate::slip::{self, SlipDecoder};
use crate::types::{OscArgRef, OscPacket};

/// How much is read from the connection at a time.
const READ_BUFFER_LEN: usize = 4_096;

/// Sends packets over a TCP connection, framed as OSC 1.1 says (see
/// [`slip`](crate::slip)).
///
/// The receiver has to expect that framing. Programs that follow OSC 1.0
/// on TCP put a size before each packet instead, and will not understand
/// this sender.
///
/// ```no_run
/// use manguebyte_osc::{OscMessage, TcpSender};
///
/// let mut sender = TcpSender::connect("127.0.0.1:9000")?;
/// sender.send(&OscMessage::new("/session/save").with("take-3").into())?;
/// # Ok::<(), manguebyte_osc::OscError>(())
/// ```
///
/// Sending may block until the other side has read what was sent before,
/// so it does not belong on the audio thread.
#[derive(Debug)]
pub struct TcpSender {
    stream: TcpStream,
    /// The packet being sent, before framing.
    packet: Vec<u8>,
    /// The same packet, framed.
    framed: Vec<u8>,
}

impl TcpSender {
    /// Connects to a receiver that is listening on `address`.
    ///
    /// Each packet is sent at once instead of being held back to be joined
    /// with the next (`TCP_NODELAY`), because control messages are small
    /// and late ones are of little use.
    pub fn connect(address: impl ToSocketAddrs) -> io::Result<Self> {
        let stream = TcpStream::connect(address)?;
        stream.set_nodelay(true)?;
        Ok(Self::from_stream(stream))
    }

    /// A sender on a connection the caller has set up.
    pub fn from_stream(stream: TcpStream) -> Self {
        Self {
            stream,
            packet: Vec::new(),
            framed: Vec::new(),
        }
    }

    /// The connection, for setting options this type has no method for.
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    /// Sends one packet.
    pub fn send(&mut self, packet: &OscPacket) -> Result<(), OscError> {
        self.packet.clear();
        packet.encode_to_vec(&mut self.packet)?;
        self.send_framed()
    }

    /// Sends one message built from borrowed arguments. Once the sender
    /// has sent a packet this large, this does not allocate.
    pub fn send_message(&mut self, address: &str, args: &[OscArgRef<'_>]) -> Result<(), OscError> {
        self.packet.clear();
        self.packet.resize(encoded_message_len(address, args), 0);
        let len = encode_message(&mut self.packet, address, args)?;
        self.packet.truncate(len);
        self.send_framed()
    }

    /// Sends bytes that are already an encoded packet.
    pub fn send_bytes(&mut self, packet: &[u8]) -> Result<(), OscError> {
        self.packet.clear();
        self.packet.extend_from_slice(packet);
        self.send_framed()
    }

    fn send_framed(&mut self) -> Result<(), OscError> {
        self.framed.clear();
        slip::encode_to_vec(&self.packet, &mut self.framed);
        self.stream.write_all(&self.framed)?;
        Ok(())
    }
}

/// Receives packets from a TCP connection, framed as OSC 1.1 says (see
/// [`slip`](crate::slip)).
///
/// ```no_run
/// use std::net::TcpListener;
/// use manguebyte_osc::TcpReceiver;
///
/// let listener = TcpListener::bind("0.0.0.0:9000")?;
/// let (mut receiver, from) = TcpReceiver::accept(&listener)?;
/// while let Some(packet) = receiver.recv()? {
///     println!("{from}: {packet:?}");
/// }
/// # Ok::<(), manguebyte_osc::OscError>(())
/// ```
#[derive(Debug)]
pub struct TcpReceiver {
    stream: TcpStream,
    decoder: SlipDecoder,
    buffer: Box<[u8]>,
    /// The part of `buffer` that has been read from the connection and not
    /// yet decoded.
    unread: core::ops::Range<usize>,
}

impl TcpReceiver {
    /// The largest packet a receiver accepts unless told otherwise: the
    /// largest that fits in a UDP datagram, so that anything that can be
    /// sent over UDP can be sent over TCP.
    pub const DEFAULT_MAX_PACKET_LEN: usize = 65_536;

    /// Waits for a sender to connect to `listener` and returns a receiver
    /// for that connection, with the sender's address.
    pub fn accept(listener: &TcpListener) -> io::Result<(Self, SocketAddr)> {
        let (stream, from) = listener.accept()?;
        Ok((Self::from_stream(stream), from))
    }

    /// A receiver on a connection the caller has set up.
    pub fn from_stream(stream: TcpStream) -> Self {
        Self::with_max_packet_len(stream, Self::DEFAULT_MAX_PACKET_LEN)
    }

    /// A receiver that accepts packets of up to `max_packet_len` bytes.
    /// That much memory is allocated now, and no more later. A longer
    /// packet is dropped and reported as an error.
    pub fn with_max_packet_len(stream: TcpStream, max_packet_len: usize) -> Self {
        Self {
            stream,
            decoder: SlipDecoder::new(max_packet_len),
            buffer: vec![0; READ_BUFFER_LEN].into_boxed_slice(),
            unread: 0..0,
        }
    }

    /// The connection, for setting options this type has no method for.
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    /// How long a receive waits for more data before it gives up with an
    /// I/O error of kind `WouldBlock` or `TimedOut` (which of the two
    /// depends on the system). `None`, the default, waits for ever.
    ///
    /// Giving up loses nothing: a packet that has arrived in part is kept
    /// and completed by a later call.
    pub fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// Waits for a packet and returns its bytes, not yet decoded. Returns
    /// `None` once the sender has closed the connection. Does not allocate.
    ///
    /// # Errors
    ///
    /// [`OscError::Frame`] when the stream is not framed correctly or a
    /// packet is too long. The damaged packet is dropped and the next call
    /// carries on with the one after it. [`OscError::Io`] for a failure of
    /// the connection, including a timeout.
    pub fn recv_bytes(&mut self) -> Result<Option<&[u8]>, OscError> {
        loop {
            if self.unread.is_empty() {
                let read = match self.stream.read(&mut self.buffer) {
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                if read == 0 {
                    return Ok(None);
                }
                self.unread = 0..read;
            }
            let input = self.buffer.get(self.unread.clone()).unwrap_or(&[]);
            let (used, outcome) = self.decoder.feed(input);
            self.unread.start += used;
            match outcome {
                Some(Ok(())) => break,
                Some(Err(error)) => return Err(error.into()),
                None => {}
            }
        }
        Ok(Some(self.decoder.frame()))
    }

    /// Waits for a packet and returns a checked view of it. Returns `None`
    /// once the sender has closed the connection. Does not allocate.
    pub fn recv_ref(&mut self) -> Result<Option<OscPacketRef<'_>>, OscError> {
        match self.recv_bytes()? {
            Some(bytes) => Ok(Some(OscPacketRef::parse(bytes)?)),
            None => Ok(None),
        }
    }

    /// Waits for a packet and returns it decoded into owned values.
    /// Returns `None` once the sender has closed the connection.
    pub fn recv(&mut self) -> Result<Option<OscPacket>, OscError> {
        Ok(self.recv_ref()?.map(|packet| packet.to_packet()))
    }
}

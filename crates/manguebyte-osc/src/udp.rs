//! Sending and receiving over UDP.
//!
//! One datagram carries one packet. UDP does not promise delivery or order,
//! which is the usual trade for control data: a lost fader movement is
//! replaced by the next one. Use bundles with time tags when order and
//! timing matter, and [TCP](crate::TcpSender) when nothing may be lost.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use crate::decode::OscPacketRef;
use crate::encode::{encode_message, encoded_message_len};
use crate::error::OscError;
use crate::types::{OscArgRef, OscPacket};

/// One more than the largest payload a UDP datagram can carry (65,507
/// bytes over IPv4), so that no datagram is ever cut short on arrival.
const DATAGRAM_BUFFER_LEN: usize = 65_536;

fn first_address(addresses: impl ToSocketAddrs) -> io::Result<SocketAddr> {
    addresses.to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the name resolved to no address",
        )
    })
}

/// Sends packets to one destination over UDP, one packet per datagram.
///
/// UDP does not promise that a datagram arrives, or that two arrive in
/// the order they were sent. That is the usual trade for control data: a
/// lost fader movement is replaced by the next one. Put messages that
/// belong together in one bundle, give the bundle a time tag when timing
/// matters, and use [`TcpSender`](crate::TcpSender) when nothing may be
/// lost.
///
/// ```no_run
/// use manguebyte_osc::{OscArgRef, OscMessage, UdpSender};
///
/// let mut sender = UdpSender::new("127.0.0.1:9000")?;
/// sender.send(&OscMessage::new("/transport/play").into())?;
/// // The same without building an owned message:
/// sender.send_message("/mixer/channel/3/gain", &[OscArgRef::Float(-6.0)])?;
/// # Ok::<(), manguebyte_osc::OscError>(())
/// ```
///
/// Sending talks to the operating system and may block, so it does not
/// belong on the audio thread.
#[derive(Debug)]
pub struct UdpSender {
    socket: UdpSocket,
    target: SocketAddr,
    /// Where packets are encoded before they are sent. Grows to the largest
    /// packet sent so far and is then reused.
    scratch: Vec<u8>,
}

impl UdpSender {
    /// A sender to `target`, from a port the system picks.
    ///
    /// `target` may be a multicast group; no membership is needed to send
    /// to one. The time-to-live and loopback of multicast packets can be
    /// set on [`socket`](Self::socket). For a broadcast address use
    /// [`broadcast`](Self::broadcast).
    pub fn new(target: impl ToSocketAddrs) -> io::Result<Self> {
        let target = first_address(target)?;
        let local: SocketAddr = match target {
            SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        Ok(Self::from_socket(UdpSocket::bind(local)?, target))
    }

    /// A sender to a broadcast address such as `255.255.255.255:9000` or
    /// `192.168.1.255:9000`. The system refuses to send to those unless the
    /// socket has asked for it, which this does.
    pub fn broadcast(target: impl ToSocketAddrs) -> io::Result<Self> {
        let sender = Self::new(target)?;
        sender.socket.set_broadcast(true)?;
        Ok(sender)
    }

    /// A sender that uses a socket the caller has set up.
    pub fn from_socket(socket: UdpSocket, target: SocketAddr) -> Self {
        Self {
            socket,
            target,
            scratch: Vec::new(),
        }
    }

    /// The socket, for setting options this type has no method for.
    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    /// Where packets are sent.
    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// Sends one packet.
    pub fn send(&mut self, packet: &OscPacket) -> Result<(), OscError> {
        self.scratch.clear();
        packet.encode_to_vec(&mut self.scratch)?;
        Ok(self.send_bytes(&self.scratch)?)
    }

    /// Sends one message built from borrowed arguments. Once the sender
    /// has sent a packet this large, this does not allocate.
    pub fn send_message(&mut self, address: &str, args: &[OscArgRef<'_>]) -> Result<(), OscError> {
        self.scratch.clear();
        self.scratch.resize(encoded_message_len(address, args), 0);
        let len = encode_message(&mut self.scratch, address, args)?;
        Ok(self.send_bytes(self.scratch.get(..len).unwrap_or(&[]))?)
    }

    /// Sends bytes that are already an encoded packet.
    pub fn send_bytes(&self, packet: &[u8]) -> io::Result<()> {
        let sent = self.socket.send_to(packet, self.target)?;
        if sent == packet.len() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the datagram was sent only in part",
            ))
        }
    }
}

/// Receives packets over UDP.
///
/// ```no_run
/// use manguebyte_osc::UdpReceiver;
///
/// let mut receiver = UdpReceiver::bind("0.0.0.0:9000")?;
/// let (packet, from) = receiver.recv()?;
/// println!("{from}: {packet:?}");
/// # Ok::<(), manguebyte_osc::OscError>(())
/// ```
///
/// To hand what arrives to the audio thread, give the receiver to
/// [`BridgeThread::udp`](crate::BridgeThread::udp).
#[derive(Debug)]
pub struct UdpReceiver {
    socket: UdpSocket,
    buffer: Box<[u8]>,
}

impl UdpReceiver {
    /// Listens on `address`. Port 0 lets the system pick a port, which
    /// [`local_addr`](Self::local_addr) then reports.
    ///
    /// A socket bound to `0.0.0.0` also receives broadcasts sent to its
    /// port.
    pub fn bind(address: impl ToSocketAddrs) -> io::Result<Self> {
        Ok(Self::from_socket(UdpSocket::bind(first_address(address)?)?))
    }

    /// Listens for packets sent to an IPv4 multicast group on `port`.
    ///
    /// `interface` is the address of the network interface to join on;
    /// `Ipv4Addr::UNSPECIFIED` lets the system choose.
    ///
    /// The standard library cannot mark a socket as shared, so on most
    /// systems only one program at a time can listen on a given port this
    /// way.
    pub fn multicast_v4(group: Ipv4Addr, port: u16, interface: Ipv4Addr) -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
        socket.join_multicast_v4(&group, &interface)?;
        Ok(Self::from_socket(socket))
    }

    /// Listens for packets sent to an IPv6 multicast group on `port`.
    ///
    /// `interface` is the index of the network interface to join on; 0 lets
    /// the system choose. The limit described on
    /// [`multicast_v4`](Self::multicast_v4) applies.
    pub fn multicast_v6(group: Ipv6Addr, port: u16, interface: u32) -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, port))?;
        socket.join_multicast_v6(&group, interface)?;
        Ok(Self::from_socket(socket))
    }

    /// A receiver that uses a socket the caller has set up.
    pub fn from_socket(socket: UdpSocket) -> Self {
        Self {
            socket,
            buffer: vec![0; DATAGRAM_BUFFER_LEN].into_boxed_slice(),
        }
    }

    /// The socket, for setting options this type has no method for.
    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    /// The address and port being listened on.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// How long a receive waits before it gives up with an error of kind
    /// `WouldBlock` or `TimedOut` (which of the two depends on the
    /// system). `None`, the default, waits for ever.
    pub fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    /// Waits for a datagram and returns its bytes, not yet decoded, and
    /// where it came from. Does not allocate.
    pub fn recv_bytes(&mut self) -> io::Result<(&[u8], SocketAddr)> {
        let (len, from) = self.socket.recv_from(&mut self.buffer)?;
        Ok((self.buffer.get(..len).unwrap_or(&[]), from))
    }

    /// Waits for a datagram and returns a checked view of the packet in
    /// it, and where it came from. Does not allocate.
    pub fn recv_ref(&mut self) -> Result<(OscPacketRef<'_>, SocketAddr), OscError> {
        let (bytes, from) = self.recv_bytes()?;
        Ok((OscPacketRef::parse(bytes)?, from))
    }

    /// Waits for a datagram and returns the packet in it, decoded into
    /// owned values, and where it came from.
    pub fn recv(&mut self) -> Result<(OscPacket, SocketAddr), OscError> {
        let (packet, from) = self.recv_ref()?;
        Ok((packet.to_packet(), from))
    }
}

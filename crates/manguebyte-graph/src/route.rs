//! How an audio connection reaches its node: which input, which channels,
//! and after how long.

/// One of the audio inputs of a node.
///
/// Every node has a main input, the audio it processes, and a sidechain,
/// which controls the processing without being heard: the kick drum that
/// makes a compressor duck a bass line. A node receives its sidechain as
/// [`ProcessContext::sidechain`](manguebyte_dsp::ProcessContext::sidechain),
/// the way a plugin receives its second input bus, so the framework's
/// dynamics processors and hosted plugins with a sidechain bus take it as
/// they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Bus {
    /// The audio the node processes in place.
    #[default]
    Main,
    /// The node's sidechain. It has as many channels as the graph.
    Sidechain,
}

/// Where an audio connection goes and what it carries: the bus of the node
/// it feeds, the channels it takes and where it puts them, and an optional
/// delay.
///
/// Built from [`Route::MAIN`] or [`Route::SIDECHAIN`]:
///
/// ```
/// use manguebyte_graph::{Bus, Route};
///
/// // The left channel of a stereo source into the sidechain's right.
/// let route = Route::SIDECHAIN.channels(0, 1, 1);
/// assert_eq!(route.bus(), Bus::Sidechain);
/// assert_eq!((route.source_channel(), route.target_channel()), (0, 1));
///
/// // A loop back into a node's main input, 64 frames later.
/// let feedback = Route::MAIN.delayed(64);
/// assert_eq!(feedback.delay(), 64);
/// ```
///
/// # Delayed connections and feedback
///
/// A connection with a [delay](Self::delayed) carries the source's output
/// exactly that many frames later, whatever the size of the blocks. It is
/// the only kind of connection that may close a loop: every loop in a graph
/// must pass through at least one, or the graph refuses the connection
/// that would close it with [`GraphError::Cycle`](crate::GraphError::Cycle).
///
/// The delay is counted in frames, not in blocks, so that a loop sounds the
/// same however the host cuts the audio into blocks. A delay of at least
/// the largest block the graph is prepared for costs nothing extra. A
/// shorter one is allowed, down to a single frame, and makes the engine run
/// the whole graph in pieces no longer than the shortest delay in it, so
/// that every frame a loop needs has been computed before it is read.
///
/// A delayed connection is not part of latency compensation: it delays by
/// exactly what it says, so a loop's timing does not move when a latency
/// elsewhere changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Route {
    bus: Bus,
    source_channel: usize,
    target_channel: usize,
    channel_count: usize,
    delay: usize,
}

impl Route {
    /// Every channel of the source into the same channel of the target's
    /// main input, with no delay. What [`Graph::connect`](crate::Graph::connect)
    /// makes.
    pub const MAIN: Self = Self {
        bus: Bus::Main,
        source_channel: 0,
        target_channel: 0,
        channel_count: usize::MAX,
        delay: 0,
    };

    /// Every channel of the source into the same channel of the target's
    /// sidechain, with no delay. What
    /// [`Graph::connect_sidechain`](crate::Graph::connect_sidechain) makes.
    pub const SIDECHAIN: Self = Self {
        bus: Bus::Sidechain,
        ..Self::MAIN
    };

    /// The same route into another bus.
    #[must_use]
    pub const fn to_bus(mut self, bus: Bus) -> Self {
        self.bus = bus;
        self
    }

    /// The same route carrying `count` channels, from the source's channel
    /// `from` on into the target's channel `to` on. Channels that either
    /// side does not have are left out.
    #[must_use]
    pub const fn channels(mut self, from: usize, to: usize, count: usize) -> Self {
        self.source_channel = from;
        self.target_channel = to;
        self.channel_count = count;
        self
    }

    /// The same route, delayed by `frames`. A delay of zero is an ordinary
    /// connection; anything more may close a loop (see the
    /// [type's documentation](Self#delayed-connections-and-feedback)).
    #[must_use]
    pub const fn delayed(mut self, frames: usize) -> Self {
        self.delay = frames;
        self
    }

    /// The bus the route feeds.
    pub const fn bus(&self) -> Bus {
        self.bus
    }

    /// The first channel taken from the source.
    pub const fn source_channel(&self) -> usize {
        self.source_channel
    }

    /// The first channel of the bus that receives it.
    pub const fn target_channel(&self) -> usize {
        self.target_channel
    }

    /// How many channels the route carries at most. `usize::MAX` means all
    /// of them.
    pub const fn channel_count(&self) -> usize {
        self.channel_count
    }

    /// The delay, in frames. Zero for an ordinary connection.
    pub const fn delay(&self) -> usize {
        self.delay
    }
}

impl Default for Route {
    fn default() -> Self {
        Self::MAIN
    }
}

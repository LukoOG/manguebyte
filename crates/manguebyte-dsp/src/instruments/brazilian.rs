//! Brazilian percussion by physical modelling, for the pieces of a
//! [`DrumKit`](super::DrumKit): the agogô and the gonguê (bells), the
//! triangle, the cuíca (a friction drum), the surdo, zabumba, alfaia,
//! tamborim and pandeiro (drums with heads, the pandeiro with its jingles),
//! the cajón (a box), the ganzá (a shaker) and the hand clap.
//!
//! Each is built from the physics of its parts, after N. H. Fletcher and
//! T. D. Rossing, "The Physics of Musical Instruments", 2nd ed., Springer
//! 1998 (cited below as Fletcher and Rossing):
//!
//! - **Bells** (agogô, gonguê): iron shells struck with a stick. Their
//!   lowest modes bend the mouth as a ring bends, with `m` nodal meridians,
//!   at frequencies going as `m (m² − 1) / √(m² + 1)` (section 2.20): 1,
//!   2.83, 5.42, 8.77, 12.86 times the (2,0) mode. A second family with a
//!   nodal circle lies above them (the (m,1) modes of a bell, chapter 21),
//!   which this model puts at 1.62 times the first, an assumption tuned
//!   by ear rather than a measurement. A bell welded from sheet iron is not
//!   quite round, so every mode is a doublet, two orientations a little
//!   apart that beat (chapter 21); where the stick lands shares the strike
//!   between them.
//! - **The triangle**: a steel rod, whose modes are near those of a free
//!   bar's bending, `(β_n / β_1)²` with `β_n L` = 4.730, 7.853, 10.996 and
//!   then `(n + ½)π` (section 2.17), split into the rod's motion in the
//!   triangle's plane and across it, which its bends set a little apart.
//!   A thin rod radiates its low modes poorly, so its sound is the dense
//!   high ones, without a definite pitch.
//! - **Drums with heads** (surdo, zabumba, alfaia, tamborim, pandeiro) are
//!   [`Heads`]: the modes of a circular membrane (section 3.5), coupled to
//!   the second head across the air (section 18.4), starting sharp when
//!   struck hard and settling, as a head stretched by its own swing does
//!   (tension modulation: section 18.2, and T. D. Rossing, "Science of
//!   Percussion Instruments", World Scientific 2000, chapter 4). The
//!   surdo's felt mallet and wide swing make its fall in pitch the deepest.
//!   A muted stroke is a hand or the mallet pressed on the head: every
//!   mode dies many times faster.
//! - **The cuíca** is a membrane with a rod fixed at its centre, inside the
//!   drum, rubbed with a damp cloth: the cloth sticks and slips, once a
//!   period, as a bow on a string does (Fletcher and Rossing, chapter 10),
//!   and the slip pulses drive the membrane at its centre, where only the
//!   modes without nodal diameters move: their frequencies go as the zeros
//!   of `J_0`, 1, 2.30, 3.60, 4.90 (section 3.5). A finger pressing the head
//!   from outside raises its tension, and the pitch with it: the high
//!   squeak rises into its note and the low groan falls.
//! - **The pandeiro's jingles** (platinelas) are pairs of small free
//!   plates, whose modes go 1, 1.73, 2.33, 3.91, 4.11, 6.30 (a circular
//!   plate with a free edge, section 3.6), set rattling by the frame's
//!   motion. Their collisions, and the ganzá's beads, are stochastic events
//!   whose rate and strength follow the energy left in the shaken system:
//!   P. R. Cook's physically informed stochastic event modelling
//!   ("Physically Informed Sonic Modeling (PhISM): Synthesis of Percussive
//!   Sounds", Computer Music Journal 21(3), 1997). The ganzá's metal tube
//!   colours them with two resonances.
//! - **The cajón**: its front plate (the tapa) vibrates as a thin
//!   rectangular plate supported at its edges, `f_mn ∝ (m / L_x)² + (n /
//!   L_y)²` (chapter 3), and the air in the box, through its sound hole, is
//!   a Helmholtz resonator, `c / 2π · √(A / (V l))`, driven by the net
//!   volume the plate's modes sweep: the bass stroke, in the middle of the
//!   plate, rings the air; the slap, at its top edge, where the plate is
//!   loose against the frame, rings the high modes and rattles the edge.
//! - **The hand clap**: a pocket of air between the palms is squeezed out
//!   as they meet, a short burst whose spectrum the shape of the hands sets
//!   (B. H. Repp, "The sound of two hands clapping: an exploratory study",
//!   JASA 81(4), 1987); here three clappers a few milliseconds apart, each
//!   with hands of their own, and the room's short reply.

use core::f64::consts::{PI, TAU};

use super::acoustic::{Burst, HeadKind, Heads, Hit, filter};
use super::common::{Noise, SILENCE, flush, t60_factor};
use super::modal::{BESSEL_ZEROS, MAX_PHASORS, Phasors, Pulse, bessel_j};
use crate::{Processor, StateVariableFilter, SvfMode};

/// A stroke of a Brazilian percussion instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Stroke {
    Clap,
    AgogoHigh,
    AgogoLow,
    Shaker,
    CuicaHigh,
    CuicaLow,
    TriangleMuted,
    TriangleOpen,
    SurdoMuted,
    SurdoOpen,
    PandeiroThumb,
    PandeiroFingers,
    PandeiroSlap,
    PandeiroMuted,
    ZabumbaOpen,
    ZabumbaMuted,
    ZabumbaStick,
    TamborimOpen,
    TamborimMuted,
    AlfaiaOpen,
    AlfaiaMuted,
    GongueOpen,
    GongueMuted,
    CajonBass,
    CajonSlap,
}

/// What a stroke rings besides a drum's heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    /// Only the heads.
    Heads,
    Bell(Bell),
    Triangle,
    Cuica,
    Jingles,
    Plate,
    Shaker,
    Clap,
}

/// The bells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bell {
    AgogoHigh,
    AgogoLow,
    Gongue,
}

/// How a stroke on a drum with heads is played: which heads, where (over
/// the piece's own `tone`), how much of every decay is left, how much
/// softer or louder and harder, and how much the jingles take of it.
#[derive(Debug, Clone, Copy)]
struct HandOnHead {
    kind: HeadKind,
    tone: f64,
    decay: f64,
    level: f64,
    hardness: f64,
    jingles: f64,
}

impl Stroke {
    fn source(self) -> Source {
        use Stroke as S;
        match self {
            S::Clap => Source::Clap,
            S::AgogoHigh => Source::Bell(Bell::AgogoHigh),
            S::AgogoLow => Source::Bell(Bell::AgogoLow),
            S::GongueOpen | S::GongueMuted => Source::Bell(Bell::Gongue),
            S::Shaker => Source::Shaker,
            S::CuicaHigh | S::CuicaLow => Source::Cuica,
            S::TriangleMuted | S::TriangleOpen => Source::Triangle,
            S::PandeiroThumb | S::PandeiroFingers | S::PandeiroSlap | S::PandeiroMuted => {
                Source::Jingles
            }
            S::CajonBass | S::CajonSlap => Source::Plate,
            S::SurdoMuted
            | S::SurdoOpen
            | S::ZabumbaOpen
            | S::ZabumbaMuted
            | S::ZabumbaStick
            | S::TamborimOpen
            | S::TamborimMuted
            | S::AlfaiaOpen
            | S::AlfaiaMuted => Source::Heads,
        }
    }

    /// Whether a hand or the mallet damps it.
    fn muted(self) -> bool {
        use Stroke as S;
        matches!(
            self,
            S::TriangleMuted
                | S::SurdoMuted
                | S::ZabumbaMuted
                | S::TamborimMuted
                | S::AlfaiaMuted
                | S::GongueMuted
                | S::PandeiroMuted
        )
    }

    fn on_head(self) -> Option<HandOnHead> {
        use Stroke as S;
        let open = |kind| HandOnHead {
            kind,
            tone: 0.5,
            decay: 1.0,
            level: 1.0,
            hardness: 1.0,
            jingles: 0.0,
        };
        Some(match self {
            S::SurdoOpen => open(HeadKind::Surdo),
            // The other hand lies on the head as the mallet strikes.
            S::SurdoMuted => HandOnHead {
                tone: 0.6,
                decay: 0.1,
                ..open(HeadKind::Surdo)
            },
            S::ZabumbaOpen => open(HeadKind::Zabumba),
            // The mallet is pressed into the head after the strike.
            S::ZabumbaMuted => HandOnHead {
                decay: 0.14,
                level: 0.9,
                ..open(HeadKind::Zabumba)
            },
            S::ZabumbaStick => open(HeadKind::ZabumbaStick),
            S::AlfaiaOpen => open(HeadKind::Alfaia),
            S::AlfaiaMuted => HandOnHead {
                decay: 0.16,
                ..open(HeadKind::Alfaia)
            },
            S::TamborimOpen => open(HeadKind::Tamborim),
            // A finger of the hand holding it presses the head from below.
            S::TamborimMuted => HandOnHead {
                decay: 0.2,
                level: 0.9,
                ..open(HeadKind::Tamborim)
            },
            // The thumb near the rim: the open, low tone.
            S::PandeiroThumb => HandOnHead {
                kind: HeadKind::Pandeiro,
                tone: 0.85,
                hardness: 0.6,
                level: 1.4,
                jingles: 0.5,
                ..open(HeadKind::Pandeiro)
            },
            // The fingertips, lightly near the rim: mostly the jingles.
            S::PandeiroFingers => HandOnHead {
                tone: 0.75,
                level: 0.24,
                jingles: 1.0,
                ..open(HeadKind::Pandeiro)
            },
            // The open palm slaps the middle and stays.
            S::PandeiroSlap => HandOnHead {
                tone: 0.3,
                decay: 0.35,
                level: 1.2,
                hardness: 1.5,
                jingles: 0.8,
                ..open(HeadKind::Pandeiro)
            },
            // A finger of the holding hand presses the head from inside.
            S::PandeiroMuted => HandOnHead {
                tone: 0.6,
                decay: 0.15,
                level: 0.8,
                jingles: 0.25,
                ..open(HeadKind::Pandeiro)
            },
            _ => return None,
        })
    }
}

/// A bell: the (2,0) mode's frequency and decay time, how the decay falls
/// with frequency, `t60 · (f / f20)^-slope`, how far apart a doublet's two
/// modes are (a fraction, growing with `m`), the level of the second
/// family, the stick's contact for the softest and the hardest stroke, and
/// the level of its tick and of the bell.
#[derive(Debug, Clone, Copy)]
struct BellSpec {
    hertz: f64,
    t60: f64,
    slope: f64,
    split: f64,
    upper: f64,
    contact: (f64, f64),
    tick: f64,
    level: f64,
}

/// The two bells of an agogô, a fourth or so apart, and the gonguê of
/// maracatu: larger, lower, ringing longer, cruder and so more split.
const AGOGO_HIGH: BellSpec = BellSpec {
    hertz: 1_180.0,
    t60: 1.3,
    slope: 0.7,
    split: 0.012,
    upper: 0.35,
    contact: (0.0004, 0.00015),
    tick: 0.25,
    level: 0.142,
};
const AGOGO_LOW: BellSpec = BellSpec {
    hertz: 860.0,
    t60: 1.5,
    ..AGOGO_HIGH
};
const GONGUE: BellSpec = BellSpec {
    hertz: 520.0,
    t60: 2.0,
    slope: 0.6,
    split: 0.025,
    upper: 0.5,
    contact: (0.0006, 0.0002),
    tick: 0.3,
    level: 0.159,
};

/// The second family of a bell's modes, over the first.
const BELL_UPPER_RATIO: f64 = 1.62;

/// How much of a bell's or a triangle's ring a hand holding it leaves.
const HELD: f64 = 0.06;
/// How much louder a held bell's and a held triangle's strike is set, so
/// that a line of them sits under the open one by about as much as a
/// player's does (measured on lines of eighth notes).
const HELD_MAKE_UP: (f64, f64) = (2.0, 2.2);

/// A ring's bending mode with `m` nodal meridians, unscaled.
fn ring(m: f64) -> f64 {
    m * (m * m - 1.0) / (m * m + 1.0).sqrt()
}

/// The triangle's lowest bending mode, its decay at 1 kHz open, the
/// second family over the first, and its level.
const TRIANGLE_HERTZ: f64 = 95.0;
const TRIANGLE_T60: f64 = 5.0;
const TRIANGLE_FAMILY: f64 = 1.035;
const TRIANGLE_LEVEL: f64 = 0.031;
/// Where the beater strikes along the rod, as a fraction of its length.
const TRIANGLE_STRIKE: f64 = 0.17;

/// `β_n L` of a free bar's bending modes (Fletcher and Rossing, section
/// 2.17); above these, `(n + ½)π`.
const BAR_BETA: [f64; 5] = [4.730_041, 7.853_205, 10.995_608, 14.137_165, 17.278_760];

fn bar_beta(n: usize) -> f64 {
    BAR_BETA.get(n).copied().unwrap_or((n as f64 + 1.5) * PI)
}

/// The free circular plate's modes over its (2,0) mode (Fletcher and
/// Rossing, section 3.6): a jingle's.
const JINGLE_RATIOS: [f64; 6] = [1.0, 1.73, 2.33, 3.91, 4.11, 6.30];
/// How many pairs of jingles a pandeiro has, the (2,0) mode of the middle
/// pair, how far apart the pairs are, and how long their modes ring.
const JINGLE_PAIRS: usize = 5;
const JINGLE_HERTZ: f64 = 2_700.0;
const JINGLE_SPREAD: f64 = 0.05;
const JINGLE_T60: f64 = 0.15;
/// How long the jingles go on rattling after a stroke (the 60 dB fall of
/// their energy), how many collisions a second they make at an energy of
/// 1, and how loud each is.
const JINGLE_SETTLE: f64 = 0.18;
const JINGLE_COLLISIONS: f64 = 900.0;
const JINGLE_LEVEL: f64 = 0.1;

/// The cajón's front plate, width and height in metres, its (1,1) mode,
/// how long that rings, and the box: its volume, and the sound hole's
/// radius and the thickness of the back it is cut in.
const PLATE_WIDTH: f64 = 0.30;
const PLATE_HEIGHT: f64 = 0.46;
const PLATE_HERTZ: f64 = 150.0;
const PLATE_T60: f64 = 0.16;
const BOX_VOLUME: f64 = 0.30 * 0.46 * 0.30;
const HOLE_RADIUS: f64 = 0.055;
const BACK_THICKNESS: f64 = 0.015;
const SPEED_OF_SOUND: f64 = 343.0;
/// How long the box's air rings.
const AIR_T60: f64 = 0.25;
/// Where a slap or a bass stroke lands, and where the loose top corner is.
const BASS_AT: (f64, f64) = (0.5, 0.62);
const SLAP_AT: (f64, f64) = (0.5, 0.93);
const CORNER: (f64, f64) = (0.12, 0.97);

/// The box's Helmholtz resonance: the hole's neck lengthened by its end
/// corrections, `1.7 a` for a flanged hole.
fn helmholtz_hertz() -> f64 {
    let area = PI * HOLE_RADIUS * HOLE_RADIUS;
    let neck = BACK_THICKNESS + 1.7 * HOLE_RADIUS;
    SPEED_OF_SOUND / TAU * (area / (BOX_VOLUME * neck)).sqrt()
}

/// A stroke of the cuíca: the pitch it starts at, rises or falls to by
/// `turn` seconds (as an exponential of `rise` seconds), and goes on to
/// (over `fall` seconds); how long the cloth rubs; its level.
#[derive(Debug, Clone, Copy)]
struct Rub {
    start: f64,
    peak: f64,
    end: f64,
    rise: f64,
    turn: f64,
    fall: f64,
    length: f64,
    level: f64,
}

const CUICA_HIGH: Rub = Rub {
    start: 620.0,
    peak: 880.0,
    end: 760.0,
    rise: 0.025,
    turn: 0.06,
    fall: 0.08,
    length: 0.16,
    level: 0.0178,
};
const CUICA_LOW: Rub = Rub {
    start: 330.0,
    peak: 345.0,
    end: 215.0,
    rise: 0.01,
    turn: 0.03,
    fall: 0.09,
    length: 0.24,
    level: 0.047,
};

/// How long the cuíca's membrane rings, held by the cloth and the finger,
/// how fast the cloth grips and lets go, and how long a slip lasts, as a
/// fraction of the period, struck softly and hard.
const CUICA_T60: f64 = 0.05;
const RUB_GRIP: f64 = 0.012;
const RUB_LET_GO: f64 = 0.05;
const SLIP: (f64, f64) = (0.35, 0.2);

/// The ganzá: how long a shake goes on pushing, how long the beads tumble
/// (the 60 dB fall of their energy), how many collisions a second they
/// make at an energy of 1, how long each one's click lasts, the tube's two
/// resonances and their sharpness, and the level.
const SHAKE_PUSH: f64 = 0.03;
const SHAKE_TUMBLE: f64 = 0.14;
const SHAKE_COLLISIONS: f64 = 3_000.0;
const SHAKE_CLICK: f64 = 0.005;
const SHAKER_TUBE: [(f64, f64, f64); 2] = [(5_200.0, 2.5, 0.6), (8_800.0, 3.0, 0.4)];
const SHAKER_LEVEL: f64 = 0.72;

/// The clappers: when each claps after the first, how loud, where the air
/// between their palms resonates, and how loud the room replies.
const CLAPPERS: [(f64, f64, f64); 3] = [
    (0.0, 1.0, 1_250.0),
    (0.011, 0.8, 1_050.0),
    (0.019, 0.65, 1_500.0),
];
const CLAP_DECAY: f64 = 0.011;
const CLAP_ROOM: f64 = 0.12;
const CLAP_LEVEL: f64 = 3.0;

/// How many samples apart a piece checks whether it has fallen silent.
const CHECK: u32 = 64;
/// How many samples apart the cuíca's modes follow its pitch.
const CUICA_HOP: u32 = 16;

/// What a mode was made from, to strike it.
#[derive(Debug, Clone, Copy, Default)]
struct Shape {
    /// A bell's nodal meridians and orientation (0 or 1); a bar's `β`; a
    /// plate's `(m, n)`; a jingle's pair.
    a: f64,
    b: f64,
}

/// A piece of Brazilian percussion, one stroke at a time; a drum's heads
/// are the kit's own [`Heads`], handed in.
#[derive(Debug, Clone)]
pub(crate) struct Percussion {
    rate: f64,
    stroke: Stroke,
    configured: Option<(Source, bool, f64, f64, f64)>,
    body: Phasors,
    shapes: [Shape; MAX_PHASORS],
    pulse: Pulse,
    /// The stick's or the slip's tick, the jingles' sizzle, the cajón's
    /// rattle, the cloth's rub.
    tick: Burst,
    claps: [Burst; 3],
    clap_wait: [u32; 3],
    clap_pending: [f64; 3],
    room: Burst,
    tube: [StateVariableFilter<f64>; 2],
    rub: StateVariableFilter<f64>,
    noise: Noise,
    random: Noise,
    /// The ganzá's or the jingles' energy, the push of a shake, the click
    /// of the last collisions, how fast each falls, how loud a collision
    /// is, and the level of the last stroke.
    energy: f64,
    energy_fall: f64,
    push: f64,
    push_fall: f64,
    click: f64,
    click_fall: f64,
    collision: f64,
    shake: f64,
    /// The cuíca: the rub's envelope, its pitch and the offset it glides
    /// out of, the pitch its modes are at, where the period is, the slip,
    /// how many samples it has rubbed and for how long it rubs.
    rub_spec: Rub,
    rub_level: f64,
    envelope: f64,
    grip: f64,
    let_go: f64,
    pitch_target: f64,
    pitch_offset: f64,
    pitch_fall: f64,
    turned: bool,
    modes_at: f64,
    phase: f64,
    slip: f64,
    rubbed: u32,
    rub_samples: u32,
    turn_samples: u32,
    hop_left: u32,
    /// The level of the stroke, and whether the kit's heads are its drum.
    level: f64,
    uses_heads: bool,
    check_left: u32,
    alive: bool,
}

impl Percussion {
    /// A piece at rest.
    pub(crate) fn new(rate: f64) -> Self {
        let band = |hertz, q| filter(SvfMode::Bandpass, hertz, q, rate);
        Self {
            rate,
            stroke: Stroke::Clap,
            configured: None,
            body: Phasors::default(),
            shapes: [Shape::default(); MAX_PHASORS],
            pulse: Pulse::default(),
            tick: Burst::new(rate),
            claps: core::array::from_fn(|_| {
                let mut clap = Burst::new(rate);
                clap.band = band(1_250.0, 2.2);
                clap
            }),
            clap_wait: [0; 3],
            clap_pending: [0.0; 3],
            room: Burst::new(rate),
            tube: [
                band(SHAKER_TUBE[0].0, SHAKER_TUBE[0].1),
                band(SHAKER_TUBE[1].0, SHAKER_TUBE[1].1),
            ],
            rub: band(2_200.0, 1.5),
            noise: Noise::for_note(0, 0.0, 31),
            random: Noise::for_note(0, 0.0, 32),
            energy: 0.0,
            energy_fall: 0.0,
            push: 0.0,
            push_fall: 0.0,
            click: 0.0,
            click_fall: 0.0,
            collision: 0.0,
            shake: 0.0,
            rub_spec: CUICA_LOW,
            rub_level: 0.0,
            envelope: 0.0,
            grip: 0.0,
            let_go: 0.0,
            pitch_target: 0.0,
            pitch_offset: 0.0,
            pitch_fall: 0.0,
            turned: false,
            modes_at: 1.0,
            phase: 0.0,
            slip: 0.3,
            rubbed: 0,
            rub_samples: 0,
            turn_samples: 0,
            hop_left: 0,
            level: 0.0,
            uses_heads: false,
            check_left: CHECK,
            alive: false,
        }
    }

    /// Sets the modes for `stroke` as `hit` tunes and sets it, unless they
    /// are set so already.
    fn configure(&mut self, stroke: Stroke, hit: &Hit) {
        let source = stroke.source();
        let key = (source, stroke.muted(), hit.tune, hit.decay, hit.texture);
        if self.configured == Some(key) {
            return;
        }
        // Another piece, or a hand on this one: what rang is stopped.
        self.silence();
        self.configured = Some(key);
        let rate = self.rate;
        // How freely it rings.
        let ring_for = (0.7 + 0.6 * hit.texture) * hit.decay;
        let held = if stroke.muted() { HELD } else { 1.0 };
        self.body.clear();
        match source {
            Source::Bell(bell) => {
                let spec = bell_spec(bell);
                let base = spec.hertz * hit.tune;
                for (family, level) in [(1.0, 1.0), (BELL_UPPER_RATIO, spec.upper)] {
                    let top = if family > 1.0 { 5 } else { 7 };
                    for m in 2..=top {
                        let m = f64::from(m);
                        let hertz = base * family * ring(m) / ring(2.0);
                        let split = spec.split * (1.0 + 0.3 * (m - 2.0));
                        let t60 = (spec.t60 * (base / hertz).powf(spec.slope) * ring_for * held)
                            .clamp(0.01, 30.0);
                        for orientation in [0.0, 1.0] {
                            let hertz = hertz * (1.0 + split * (orientation - 0.5));
                            // Heard from a little round the bell from the
                            // seam; struck where the stroke lands.
                            let listen = m * 1.1;
                            let heard = if orientation == 0.0 {
                                listen.cos()
                            } else {
                                listen.sin()
                            };
                            self.shapes[self.body.len().min(MAX_PHASORS - 1)] = Shape {
                                a: m,
                                b: orientation,
                            };
                            self.body
                                .add(hertz, t60, level * heard / (m - 1.0), 0.0, rate);
                        }
                    }
                }
            }
            Source::Triangle => {
                let base = TRIANGLE_HERTZ * hit.tune;
                // The bends set every mode a little off the bar's, the same
                // on every strike.
                let mut bends = Noise::for_note(0, 0.0, 0x7249);
                for (family, level) in [(1.0, 1.0), (TRIANGLE_FAMILY, 0.8)] {
                    for n in 0..40 {
                        let beta = bar_beta(n);
                        let hertz = base
                            * family
                            * (beta / BAR_BETA[0]).powi(2)
                            * (1.0 + 0.006 * bends.next());
                        if hertz > 18_000.0 || hertz >= 0.45 * rate {
                            break;
                        }
                        let open = TRIANGLE_T60 * (1_000.0 / hertz).powf(0.45);
                        let t60 = if stroke.muted() {
                            open.min(0.2 * (1_000.0 / hertz).powf(0.2))
                        } else {
                            open
                        };
                        let t60 = (t60 * ring_for).clamp(0.01, 30.0);
                        // A thin rod radiates its low modes poorly.
                        let heard = level * (hertz / 3_000.0).powi(2).min(1.0);
                        self.shapes[self.body.len().min(MAX_PHASORS - 1)] =
                            Shape { a: beta, b: 0.0 };
                        self.body.add(hertz, t60, heard, 0.0, rate);
                    }
                }
            }
            Source::Plate => {
                let base = PLATE_HERTZ * hit.tune;
                let unit = 1.0 / (PLATE_WIDTH * PLATE_WIDTH) + 1.0 / (PLATE_HEIGHT * PLATE_HEIGHT);
                // The box's air first: it is the mode the bass is.
                let air = helmholtz_hertz() * hit.tune.sqrt();
                self.shapes[0] = Shape { a: 0.0, b: 0.0 };
                self.body
                    .add(air, (AIR_T60 * ring_for).clamp(0.01, 5.0), 1.3, 0.0, rate);
                for m in 1..=12 {
                    for n in 1..=12 {
                        let (mf, nf) = (f64::from(m), f64::from(n));
                        let shape = (mf / PLATE_WIDTH).powi(2) + (nf / PLATE_HEIGHT).powi(2);
                        let hertz = base * shape / unit;
                        if hertz > 3_500.0 {
                            continue;
                        }
                        let t60 = (PLATE_T60 * (base / hertz).sqrt() * ring_for).clamp(0.01, 5.0);
                        // The modes that sweep a net volume radiate most.
                        let sweeps = if m % 2 == 1 && n % 2 == 1 { 1.0 } else { 0.35 };
                        let heard = sweeps * (base / hertz).powf(0.3);
                        let corner = (mf * PI * CORNER.0).sin() * (nf * PI * CORNER.1).sin();
                        self.shapes[self.body.len().min(MAX_PHASORS - 1)] = Shape { a: mf, b: nf };
                        self.body.add(hertz, t60, heard, corner, rate);
                    }
                }
            }
            Source::Cuica => {
                // The axisymmetric modes, at a pitch of 1 Hz: they follow
                // the rub's pitch as it moves.
                let j01 = BESSEL_ZEROS[0].2;
                self.modes_at = 1.0;
                for &(m, _, j) in BESSEL_ZEROS.iter().filter(|zero| zero.0 == 0).take(6) {
                    let ratio = j / j01;
                    // Heard as the volume it sweeps, `J_1(j) / j`.
                    let heard =
                        (bessel_j(u32::from(m) + 1, j) / j).abs() / (bessel_j(1, j01) / j01).abs();
                    let t60 = (CUICA_T60 / ratio.sqrt() * ring_for).clamp(0.005, 2.0);
                    self.shapes[self.body.len().min(MAX_PHASORS - 1)] = Shape { a: ratio, b: 0.0 };
                    // Added at a pitch the bank accepts; moved to the rub's
                    // at its strike.
                    self.body.add(ratio, t60, heard, 0.0, rate.max(1.0));
                }
            }
            Source::Jingles => {
                for pair in 0..JINGLE_PAIRS {
                    let spread = 1.0 + JINGLE_SPREAD * (pair as f64 - 2.0);
                    let base = JINGLE_HERTZ * spread * hit.tune.sqrt();
                    for (k, ratio) in JINGLE_RATIOS.into_iter().enumerate() {
                        let hertz = base * ratio;
                        let t60 = (JINGLE_T60 * (1.0 / ratio).sqrt() * ring_for).clamp(0.005, 2.0);
                        self.shapes[self.body.len().min(MAX_PHASORS - 1)] = Shape {
                            a: pair as f64,
                            b: k as f64,
                        };
                        self.body
                            .add(hertz, t60, 1.0 / (k as f64 + 1.0).sqrt(), 0.0, rate);
                    }
                }
            }
            Source::Heads | Source::Shaker | Source::Clap => {}
        }
    }

    /// Strikes the piece, adding to whatever it still rings with; a drum's
    /// heads are `heads`.
    pub(crate) fn strike(&mut self, stroke: Stroke, hit: &Hit, heads: &mut Heads) {
        self.configure(stroke, hit);
        self.stroke = stroke;
        let rate = self.rate;
        let mut random = Noise::for_note(0, 0.0, hit.seed);
        let amplitude = hit.amplitude.max(0.0);
        let hard = hit.hardness.clamp(0.0, 1.5);
        let tone = hit.tone.clamp(0.0, 1.0);
        let snap = hit.snap.clamp(0.0, 1.0);
        self.noise = Noise::for_note(0, 0.0, hit.seed ^ 0x51);
        self.random = Noise::for_note(0, 0.0, hit.seed ^ 0x52);
        self.uses_heads = false;
        if let Some(hand) = stroke.on_head() {
            let head_hit = Hit {
                amplitude: amplitude * hand.level,
                hardness: (hard * hand.hardness).min(1.5),
                tone: (hand.tone + tone - 0.5).clamp(0.0, 1.0),
                decay: hit.decay * hand.decay,
                ..*hit
            };
            heads.strike(hand.kind, &head_hit);
            self.uses_heads = true;
            if hand.jingles > 0.0 {
                // The frame's jolt sets the jingles rattling.
                self.energy += hand.jingles * (0.5 + 0.5 * hard.min(1.0));
                self.shake = amplitude;
                self.energy_fall = t60_factor(JINGLE_SETTLE * hit.decay.min(2.0), rate);
                self.click_fall = t60_factor(0.008, rate);
                self.collision = JINGLE_LEVEL * (0.5 + snap);
                self.level = 1.0;
                self.alive = true;
            }
            self.check_left = CHECK;
            return;
        }
        match stroke.source() {
            Source::Bell(bell) => {
                let spec = bell_spec(bell);
                // Where the stick lands, round the bell from the seam.
                let angle = 0.3 + 0.25 * random.unit() + 0.5 * tone;
                for (input, shape) in self.body.inputs_mut().iter_mut().zip(&self.shapes) {
                    let turn = shape.a * angle;
                    *input = if shape.b == 0.0 {
                        turn.cos()
                    } else {
                        turn.sin()
                    };
                }
                let contact =
                    spec.contact.0 * (spec.contact.1 / spec.contact.0).powf(hard.min(1.3));
                self.pulse.start(contact, amplitude, rate);
                self.tick
                    .band
                    .set_frequency((4_000.0 * (0.8 + 0.4 * hard)).min(0.45 * rate));
                self.tick.add(
                    amplitude * spec.tick * (0.4 + 1.2 * snap),
                    contact,
                    0.006,
                    rate,
                );
                // Held, a bell or a triangle sounds shorter, not softer: its
                // strike is made up for.
                self.level = spec.level * if stroke.muted() { HELD_MAKE_UP.0 } else { 1.0 };
            }
            Source::Triangle => {
                let at = TRIANGLE_STRIKE * (0.8 + 0.4 * tone);
                for (input, shape) in self.body.inputs_mut().iter_mut().zip(&self.shapes) {
                    let x = shape.a * at;
                    // A free bar's shape near an end, its far end's share
                    // left out.
                    *input = 0.5 * (x.cos() - x.sin() + (-x).exp());
                }
                let contact = 0.000_15 * (0.4f64).powf(hard.min(1.3));
                self.pulse.start(contact, amplitude, rate);
                self.tick.band.set_frequency((7_000.0f64).min(0.45 * rate));
                self.tick
                    .add(amplitude * 0.08 * (0.4 + 1.2 * snap), contact, 0.003, rate);
                self.level = TRIANGLE_LEVEL * if stroke.muted() { HELD_MAKE_UP.1 } else { 1.0 };
            }
            Source::Plate => {
                let (at, contact, rattle) = if stroke == Stroke::CajonBass {
                    (BASS_AT, 0.004 * (0.55f64).powf(hard.min(1.3)), 0.1)
                } else {
                    (SLAP_AT, 0.0008 * (0.5f64).powf(hard.min(1.3)), 1.2)
                };
                let at = (at.0, (at.1 + 0.2 * (tone - 0.5)).clamp(0.05, 0.97));
                let mut volume = 0.0;
                let inputs = self.body.inputs_mut();
                let count = inputs.len();
                for (input, shape) in inputs.iter_mut().zip(&self.shapes).skip(1) {
                    let (m, n) = (shape.a, shape.b);
                    *input = (m * PI * at.0).sin() * (n * PI * at.1).sin();
                    if m as u32 % 2 == 1 && n as u32 % 2 == 1 {
                        volume += *input * 4.0 / (PI * PI * m * n);
                    }
                }
                if count > 0 {
                    // The air takes the volume the plate's modes sweep.
                    inputs[0] = 1.5 * volume;
                }
                self.pulse.start(contact, amplitude, rate);
                self.tick.band.set_frequency((3_500.0f64).min(0.45 * rate));
                self.tick
                    .add(amplitude * rattle * (0.4 + 1.2 * snap), 0.001, 0.05, rate);
                self.level = if stroke == Stroke::CajonBass {
                    0.3
                } else {
                    0.25
                };
            }
            Source::Cuica => {
                let rub = if stroke == Stroke::CuicaHigh {
                    CUICA_HIGH
                } else {
                    CUICA_LOW
                };
                // Pulled harder, the cloth slips shorter and the finger
                // presses a little harder.
                let lift = 1.0 + 0.03 * (hard - 0.5) + 0.12 * (tone - 0.5);
                let rub = Rub {
                    start: rub.start * hit.tune * lift,
                    peak: rub.peak * hit.tune * lift,
                    end: rub.end * hit.tune * lift,
                    length: rub.length * hit.decay.sqrt(),
                    ..rub
                };
                self.rub_spec = rub;
                self.rub_level = amplitude * rub.level;
                self.slip = SLIP.0 + (SLIP.1 - SLIP.0) * hard.min(1.0);
                self.grip = 1.0 - (-1.0 / (RUB_GRIP * rate)).exp();
                self.let_go = t60_factor(RUB_LET_GO, rate);
                self.pitch_target = rub.peak;
                self.pitch_offset = rub.start - rub.peak;
                self.pitch_fall = (-1.0 / (rub.rise * rate)).exp();
                self.turned = false;
                self.rubbed = 0;
                self.rub_samples = (rub.length * rate) as u32;
                self.turn_samples = (rub.turn * rate) as u32;
                self.hop_left = 0;
                let scale = rub.start / self.modes_at;
                self.body.scale_frequencies(scale);
                self.modes_at = rub.start;
                for input in self.body.inputs_mut() {
                    *input = 1.0;
                }
                self.tick.band.set_frequency((2_200.0f64).min(0.45 * rate));
                self.level = 1.0;
            }
            Source::Shaker => {
                self.push += 0.6 + 0.4 * hard.min(1.0);
                self.shake = amplitude;
                self.push_fall = t60_factor(SHAKE_PUSH, rate);
                self.energy_fall =
                    t60_factor(SHAKE_TUMBLE * hit.decay * (0.7 + 0.6 * hit.texture), rate);
                self.click_fall = t60_factor(SHAKE_CLICK, rate);
                self.collision = 0.5 + snap;
                for (filter, &(hertz, _, _)) in self.tube.iter_mut().zip(&SHAKER_TUBE) {
                    filter.set_frequency(
                        (hertz * hit.tune.sqrt() * (0.85 + 0.3 * tone)).min(0.45 * rate),
                    );
                }
                self.level = SHAKER_LEVEL;
            }
            Source::Clap => {
                for (index, &(at, level, hertz)) in CLAPPERS.iter().enumerate() {
                    let at = at + 0.002 * random.next();
                    self.clap_wait[index] = (at.max(0.0) * rate) as u32 + 1;
                    self.clap_pending[index] += amplitude * level * (0.85 + 0.3 * random.unit());
                    let hertz = hertz * (0.9 + 0.2 * random.unit()) * (0.8 + 0.4 * tone) * hit.tune;
                    self.claps[index].band.set_frequency(hertz.min(0.45 * rate));
                }
                self.room.band.set_frequency((1_600.0f64).min(0.45 * rate));
                self.room.add(
                    amplitude * CLAP_ROOM * (0.5 + hit.texture),
                    0.004,
                    0.12 * hit.decay,
                    rate,
                );
                self.level = CLAP_LEVEL;
            }
            Source::Heads | Source::Jingles => {}
        }
        self.check_left = CHECK;
        self.alive = true;
    }

    /// One sample, the heads' among it.
    #[inline]
    pub(crate) fn next(&mut self, heads: &mut Heads) -> f64 {
        let mut out = 0.0;
        if self.uses_heads && heads.is_sounding() {
            out += heads.next();
        }
        if !self.alive {
            return out;
        }
        let rate = self.rate;
        let sound = match self.stroke.source() {
            Source::Bell(_) | Source::Triangle => {
                let (ring, _) = self.body.next(self.pulse.next());
                ring + self.tick.next(self.noise.next())
            }
            Source::Plate => {
                let (ring, corner) = self.body.next(self.pulse.next());
                // The loose corner slaps the frame as the plate swings
                // away from it.
                let gate = if corner > 0.0 { 1.0 } else { 0.15 };
                ring + self.tick.next(self.noise.next() * gate)
            }
            Source::Jingles => self.jingles(),
            Source::Cuica => self.cuica(),
            Source::Shaker => self.shaker(),
            Source::Clap => {
                let mut sum = 0.0;
                for index in 0..CLAPPERS.len() {
                    if self.clap_wait[index] > 0 {
                        self.clap_wait[index] -= 1;
                        if self.clap_wait[index] == 0 {
                            let level = core::mem::take(&mut self.clap_pending[index]);
                            self.claps[index].add(level, 0.0004, CLAP_DECAY, rate);
                        }
                    }
                    sum += self.claps[index].next(self.noise.next());
                }
                sum + self.room.next(self.noise.next())
            }
            Source::Heads => 0.0,
        };
        self.check_left -= 1;
        if self.check_left == 0 {
            self.check_left = CHECK;
            let level = self.body.level()
                + self.tick.level()
                + self.claps.iter().map(Burst::level).sum::<f64>()
                + self.clap_pending.iter().sum::<f64>()
                + self.room.level()
                + self.energy
                + self.push
                + self.click
                + self.envelope
                + if self.rubbed < self.rub_samples {
                    self.rub_level
                } else {
                    0.0
                };
            let busy = self.pulse.is_running() || self.clap_wait.iter().any(|&wait| wait > 0);
            if level < 0.1 * SILENCE && !busy {
                self.rest();
            } else {
                self.body.flush_quiet();
            }
        }
        out + sound * self.level
    }

    /// The jingles: collisions at a rate and strength that follow their
    /// energy, each ringing one pair's modes and a sizzle of noise.
    #[inline]
    fn jingles(&mut self) -> f64 {
        self.energy = flush(self.energy * self.energy_fall);
        let strength = self.energy.sqrt();
        let chance = (JINGLE_COLLISIONS * strength / self.rate).min(0.5);
        let mut force = 0.0;
        if self.random.unit() < chance {
            let pair = ((self.random.unit() * JINGLE_PAIRS as f64) as usize).min(JINGLE_PAIRS - 1);
            let random = &mut self.random;
            for (input, shape) in self.body.inputs_mut().iter_mut().zip(&self.shapes) {
                *input = if shape.a as usize == pair {
                    random.next()
                } else {
                    0.0
                };
            }
            force = self.collision * self.shake * strength * (0.5 + 0.5 * self.random.unit());
            self.tick
                .band
                .set_frequency((8_000.0f64).min(0.45 * self.rate));
            self.tick.add(0.3 * force, 0.0002, 0.008, self.rate);
        }
        let (ring, _) = self.body.next(force);
        ring + self.tick.next(self.noise.next())
    }

    /// The ganzá: the shake pushes energy in, the beads collide at a rate
    /// that follows it, each collision a click of noise through the tube.
    #[inline]
    fn shaker(&mut self) -> f64 {
        let pushed = self.push * (1.0 - self.push_fall);
        self.push = flush(self.push * self.push_fall);
        self.energy = flush(self.energy * self.energy_fall + pushed);
        let strength = self.energy.sqrt();
        let chance = (SHAKE_COLLISIONS * strength / self.rate).min(0.5);
        if self.random.unit() < chance {
            self.click += self.collision * self.shake * strength * self.random.unit();
        }
        self.click = flush(self.click * self.click_fall);
        let grains = self.click * self.noise.next();
        let mut out = 0.0;
        for (filter, &(_, _, weight)) in self.tube.iter_mut().zip(&SHAKER_TUBE) {
            out += weight * filter.process_sample(0, grains);
        }
        out
    }

    /// The cuíca: the cloth grips and lets go, the pitch glides as the
    /// finger presses, a slip every period drives the membrane at its
    /// centre, and the cloth's own rub is heard faintly.
    #[inline]
    fn cuica(&mut self) -> f64 {
        let rub = self.rub_spec;
        if self.rubbed < self.rub_samples {
            self.rubbed += 1;
            self.envelope += (1.0 - self.envelope) * self.grip;
            if !self.turned && self.rubbed >= self.turn_samples {
                // The finger has pressed in: the pitch goes on to its end.
                self.turned = true;
                let now = self.pitch_target + self.pitch_offset;
                self.pitch_target = rub.end;
                self.pitch_offset = now - rub.end;
                self.pitch_fall = (-1.0 / (rub.fall * self.rate)).exp();
            }
        } else {
            self.envelope = flush(self.envelope * self.let_go);
        }
        self.pitch_offset *= self.pitch_fall;
        let pitch = (self.pitch_target + self.pitch_offset).max(20.0);
        if self.hop_left == 0 {
            self.hop_left = CUICA_HOP;
            self.body.scale_frequencies(pitch / self.modes_at);
            self.modes_at = pitch;
        }
        self.hop_left -= 1;
        self.phase += pitch / self.rate;
        if self.phase >= 1.0 && self.envelope > 0.0 {
            self.phase -= 1.0;
            // The slips are a little irregular, as friction is.
            let jitter = 1.0 + 0.004 * self.random.next();
            self.pulse.start(
                self.slip * jitter / pitch,
                self.rub_level * self.envelope,
                self.rate,
            );
        } else if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        let (ring, _) = self.body.next(self.pulse.next());
        let cloth = self
            .rub
            .process_sample(0, self.noise.next() * self.envelope * self.rub_level);
        ring + 0.03 * cloth
    }

    /// Whether anything of the piece but the heads still sounds.
    pub(crate) fn is_sounding(&self) -> bool {
        self.alive
    }

    /// Everything at rest, the modes kept.
    fn rest(&mut self) {
        self.alive = false;
        self.body.rest();
        self.pulse.stop();
        self.tick.silence();
        for clap in &mut self.claps {
            clap.silence();
        }
        self.clap_wait = [0; 3];
        self.clap_pending = [0.0; 3];
        self.room.silence();
        for filter in &mut self.tube {
            Processor::<f64>::reset(filter);
        }
        Processor::<f64>::reset(&mut self.rub);
        self.energy = 0.0;
        self.push = 0.0;
        self.click = 0.0;
        self.envelope = 0.0;
        self.rubbed = 0;
        self.rub_samples = 0;
        self.phase = 0.0;
        self.check_left = CHECK;
    }

    /// At rest, the modes kept; the heads are the caller's to silence.
    pub(crate) fn silence(&mut self) {
        self.rest();
        self.uses_heads = false;
    }

    /// The frequencies of the modes, for the tests.
    #[cfg(test)]
    pub(crate) fn modes(&self) -> Vec<f64> {
        (0..self.body.len())
            .map(|i| self.body.omega(i) * self.rate / TAU)
            .collect()
    }
}

fn bell_spec(bell: Bell) -> BellSpec {
    match bell {
        Bell::AgogoHigh => AGOGO_HIGH,
        Bell::AgogoLow => AGOGO_LOW,
        Bell::Gongue => GONGUE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::{Complex, RealFft};
    use crate::instruments::measure::{
        self, RATE, accented, amplitude_at, between, decibels, frequency_near, off, on, peak, play,
        synth_of,
    };
    use crate::instruments::{DrumKit, DrumKitVoice, percussion};
    use crate::{ProcessSpec, Synth};

    fn kit() -> DrumKit {
        DrumKit::brazilian()
    }

    /// One stroke of `key` at `velocity`, rendered for `seconds`.
    fn stroke(key: u8, velocity: f32, seconds: f64) -> Vec<f64> {
        measure::note(
            DrumKitVoice::voices(2, kit()),
            on(key, velocity),
            0.05,
            seconds,
        )
    }

    /// The peaks of the spectrum of `signal` (Hann window, zero padded to
    /// 2^16) between `low` and `high` hertz, louder than every bin within
    /// `reach`: `(hertz, decibels)`, loudest first.
    fn peaks(signal: &[f64], low: f64, high: f64, reach: usize) -> Vec<(f64, f64)> {
        let len = 1 << 16;
        let count = signal.len().min(len);
        let mut input = vec![0.0; len];
        for (n, (slot, &x)) in input.iter_mut().zip(signal).enumerate() {
            *slot = x * (0.5 - 0.5 * (TAU * n as f64 / (count - 1) as f64).cos());
        }
        let mut fft = RealFft::<f64>::new(len);
        let mut out = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
        fft.forward(&input, &mut out);
        let magnitudes: Vec<f64> = out.iter().map(|value| value.norm()).collect();
        let mut found = Vec::new();
        for bin in reach..magnitudes.len() - reach {
            let hertz = bin as f64 * RATE / len as f64;
            if hertz < low || hertz > high {
                continue;
            }
            if magnitudes[bin - reach..=bin + reach]
                .iter()
                .all(|&m| m <= magnitudes[bin])
            {
                found.push((hertz, decibels(magnitudes[bin])));
            }
        }
        found.sort_by(|a, b| b.1.total_cmp(&a.1));
        found
    }

    /// Whether one of the `top` loudest of `peaks` is within `tolerance`
    /// (a fraction) of `hertz`.
    fn near(peaks: &[(f64, f64)], top: usize, hertz: f64, tolerance: f64) -> bool {
        peaks
            .iter()
            .take(top)
            .any(|&(f, _)| (f / hertz - 1.0).abs() < tolerance)
    }

    /// The energy of `signal` between `low` and `high` hertz.
    fn band(signal: &[f64], low: f64, high: f64) -> f64 {
        measure::band_passed(signal, low, high)
            .iter()
            .map(|x| x * x)
            .sum()
    }

    /// How long a stroke of `key` sounds, at `rate`, in seconds: when its
    /// voice falls silent; `None` if it still sounds after `most`.
    fn sounds_for(key: u8, rate: f64, most: f64) -> (Option<f64>, Vec<f64>) {
        let mut synth = Synth::new(DrumKitVoice::voices(2, kit()));
        synth.prepare(&ProcessSpec {
            sample_rate: rate,
            max_block_size: 512,
            num_channels: 1,
        });
        let mut signal = Vec::new();
        let mut buffer = manguebyte_core::AudioBuffer::<f64>::new(1, 480);
        synth.note_on(crate::event::NoteRef::new(9, key), 1.0);
        synth.note_off(crate::event::NoteRef::new(9, key), 0.0);
        while (signal.len() as f64) < most * rate {
            buffer.clear();
            synth.process(&mut buffer);
            signal.extend_from_slice(buffer.channel(0));
            if synth.active_voices() == 0 {
                return (Some(signal.len() as f64 / rate), signal);
            }
        }
        (None, signal)
    }

    #[test]
    fn every_piece_plays_at_a_sensible_level_and_falls_silent() {
        let mut ends = Vec::new();
        for key in percussion::ALL {
            let model = kit().piece(key).unwrap().model;
            let (end, signal) = sounds_for(key, RATE, 15.0);
            let loudest = peak(&signal);
            println!(
                "{key:3} {model:?}: peak {:6.1} dB, silent after {end:.2?} s",
                decibels(loudest)
            );
            // Between -24 and -5 dB at full velocity, as every kit's pieces.
            assert!((0.063..0.56).contains(&loudest), "{model:?}: {loudest}");
            let end = end.unwrap_or_else(|| panic!("{model:?} still sounds"));
            ends.push((key, end));
        }
        let end = |key| ends.iter().find(|&&(k, _)| k == key).unwrap().1;
        use percussion::*;
        for (muted, open) in [
            (MUTE_TRIANGLE, OPEN_TRIANGLE),
            (MUTE_SURDO, OPEN_SURDO),
            (ZABUMBA_MUTED, ZABUMBA_OPEN),
            (TAMBORIM_MUTED, TAMBORIM_OPEN),
            (ALFAIA_MUTED, ALFAIA_OPEN),
            (GONGUE_MUTED, GONGUE_OPEN),
            (PANDEIRO_MUTED, PANDEIRO_THUMB),
        ] {
            assert!(end(muted) < 0.6 * end(open), "{muted} against {open}");
        }
    }

    #[test]
    fn every_piece_works_and_falls_silent_at_every_sample_rate() {
        for rate in [3_000.0, 22_050.0, 96_000.0] {
            for key in percussion::ALL {
                let (end, signal) = sounds_for(key, rate, 15.0);
                let label = format!("{key} at {rate} Hz");
                assert!(measure::finite_within(&signal, 1.0), "{label}");
                assert!(peak(&signal) > 1e-3, "{label}");
                assert!(end.is_some(), "{label}: still sounding");
            }
        }
    }

    #[test]
    fn the_output_does_not_depend_on_the_block_size() {
        use percussion::*;
        let mut script = Vec::new();
        for (index, &key) in percussion::ALL.iter().enumerate() {
            script.push((index * 1_500, on(key, 0.5 + 0.02 * index as f32)));
        }
        // Struck again while they ring, and muted while open.
        for (at, key) in [
            (40_000, OPEN_SURDO),
            (40_500, OPEN_SURDO),
            (41_000, MUTE_SURDO),
            (42_000, PANDEIRO_FINGERS),
            (42_300, PANDEIRO_FINGERS),
            (43_000, MUTE_CUICA),
            (43_900, MUTE_CUICA),
            (44_000, SHAKER),
            (44_200, SHAKER),
        ] {
            script.push((at, accented(key, 0.9)));
        }
        script.push((45_000, off(OPEN_SURDO)));
        let run = |block| {
            play(
                &mut synth_of(DrumKitVoice::voices(6, kit())),
                &script,
                60_000,
                block,
            )
        };
        let reference = run(256);
        assert!(peak(&reference) > 0.05);
        for block in [1, 7, 64, 1_000] {
            assert!(run(block) == reference, "blocks of {block}");
        }
    }

    #[test]
    fn the_bells_ring_their_ring_modes_in_doublets() {
        for (key, spec) in [
            (percussion::HIGH_AGOGO, AGOGO_HIGH),
            (percussion::LOW_AGOGO, AGOGO_LOW),
            (percussion::GONGUE_OPEN, GONGUE),
        ] {
            let signal = stroke(key, 0.8, 1.0);
            let found = peaks(between(&signal, 0.03, 0.9), 200.0, 12_000.0, 4);
            let f0 = spec.hertz;
            // The (2,0) doublet is the loudest.
            assert!(
                (found[0].0 / f0 - 1.0).abs() < 0.5 * spec.split + 0.004,
                "{key}: {:?}",
                &found[..4]
            );
            // The (3,0) and (4,0) modes, each a doublet split about its ring
            // frequency, and the second family above the first.
            for m in [3.0, 4.0] {
                let ratio = ring(m) / ring(2.0);
                let split = spec.split * (1.0 + 0.3 * (m - 2.0));
                for side in [-0.5, 0.5] {
                    let hertz = f0 * ratio * (1.0 + split * side);
                    assert!(
                        near(&found, 30, hertz, 0.003),
                        "{key}: {hertz} in {found:?}"
                    );
                }
            }
            assert!(
                near(&found, 10, f0 * BELL_UPPER_RATIO, 0.5 * spec.split + 0.004),
                "{key}"
            );
            // The configured modes are those ratios.
            let mut bell = Percussion::new(RATE);
            let hit = Hit {
                amplitude: 1.0,
                hardness: 0.8,
                tune: 1.0,
                decay: 1.0,
                tone: 0.5,
                snap: 0.5,
                texture: 0.5,
                seed: 1,
            };
            let stroke = if key == percussion::GONGUE_OPEN {
                Stroke::GongueOpen
            } else if key == percussion::HIGH_AGOGO {
                Stroke::AgogoHigh
            } else {
                Stroke::AgogoLow
            };
            bell.strike(stroke, &hit, &mut Heads::new(RATE));
            let modes = bell.modes();
            assert!((modes[0] / modes[2] - 1.0 / ring(3.0) * ring(2.0)).abs() < 0.02);
        }
    }

    #[test]
    fn the_triangle_rings_a_free_bars_modes_and_hardly_its_lowest() {
        let signal = stroke(percussion::OPEN_TRIANGLE, 0.8, 1.6);
        let window = between(&signal, 0.1, 1.5);
        let found = peaks(window, 1_500.0, 12_000.0, 6);
        let predicted: Vec<f64> = [1.0, TRIANGLE_FAMILY]
            .into_iter()
            .flat_map(|family| {
                (0..40).map(move |n| TRIANGLE_HERTZ * family * (bar_beta(n) / BAR_BETA[0]).powi(2))
            })
            .collect();
        for &(hertz, level) in found.iter().take(12) {
            assert!(
                predicted.iter().any(|&p| (hertz / p - 1.0).abs() < 0.01),
                "a peak at {hertz:.0} Hz ({level:.0} dB) is no bar mode"
            );
        }
        // A thin rod radiates its low modes poorly.
        assert!(band(window, 2_500.0, 12_000.0) > 100.0 * band(window, 50.0, 1_000.0));
    }

    #[test]
    fn the_cuica_squeaks_up_and_groans_down_in_harmonics() {
        // The squeak rises into its note; the groan falls.
        let high = stroke(percussion::MUTE_CUICA, 0.8, 0.3);
        let start = frequency_near(between(&high, 0.005, 0.025), 650.0);
        let top = frequency_near(between(&high, 0.045, 0.065), 850.0);
        println!("squeak: {start:.0} Hz, then {top:.0} Hz");
        assert!(
            top > 1.15 * start && (780.0..900.0).contains(&top),
            "{start} {top}"
        );
        let low = stroke(percussion::OPEN_CUICA, 0.8, 0.4);
        let early = frequency_near(between(&low, 0.02, 0.06), 330.0);
        let late = frequency_near(between(&low, 0.17, 0.22), 230.0);
        println!("groan: {early:.0} Hz, then {late:.0} Hz");
        assert!(
            early > 1.2 * late && (200.0..260.0).contains(&late),
            "{early} {late}"
        );
        // A tone, its harmonics locked to the slips: the second and third
        // are where the fundamental's multiples are.
        let window = between(&low, 0.17, 0.22);
        for harmonic in [2.0, 3.0] {
            let at = frequency_near(window, harmonic * late);
            assert!(
                (at / (harmonic * late) - 1.0).abs() < 0.01,
                "{harmonic}: {at}"
            );
            let level = decibels(amplitude_at(window, at) / amplitude_at(window, late));
            assert!((-45.0..-3.0).contains(&level), "{harmonic}: {level} dB");
        }
    }

    #[test]
    fn the_drums_ring_their_heads_modes() {
        use percussion::*;
        // The (0,1) mode, shifted by the coupled head on a drum of two, and
        // the (1,1) at 1.593 times it.
        for (key, hertz, tolerance) in [
            (OPEN_SURDO, 58.0, 0.15),
            (ZABUMBA_OPEN, 92.0, 0.08),
            (ALFAIA_OPEN, 78.0, 0.08),
            (TAMBORIM_OPEN, 640.0, 0.03),
            (PANDEIRO_THUMB, 190.0, 0.03),
        ] {
            let signal = stroke(key, 0.6, 0.6);
            let found = peaks(between(&signal, 0.1, 0.5), 30.0, 2_000.0, 3);
            for ratio in [1.0, 1.593] {
                assert!(
                    near(&found, 6, hertz * ratio, tolerance),
                    "{key}: {} in {:?}",
                    hertz * ratio,
                    &found[..6]
                );
            }
        }
        // The surdo starts sharp and settles: a head stretched by its swing.
        let signal = stroke(OPEN_SURDO, 1.0, 1.4);
        let early = peaks(between(&signal, 0.0, 0.25), 80.0, 110.0, 3)[0].0;
        let late = peaks(between(&signal, 0.8, 1.3), 80.0, 110.0, 3)[0].0;
        println!("surdo's (1,1): {early:.2} Hz, then {late:.2} Hz");
        assert!(early > 1.005 * late, "{early} {late}");
    }

    #[test]
    fn the_cajon_bass_rings_the_box_and_the_slap_the_plate() {
        let bass = stroke(percussion::CAJON_BASS, 0.8, 0.6);
        let found = peaks(between(&bass, 0.02, 0.5), 40.0, 2_000.0, 3);
        assert!(
            (found[0].0 / helmholtz_hertz() - 1.0).abs() < 0.04,
            "{found:?}"
        );
        assert!(near(&found, 4, PLATE_HERTZ, 0.04), "{found:?}");
        let slap = stroke(percussion::CAJON_SLAP, 0.8, 0.6);
        let bright = |signal: &[f64]| band(signal, 1_000.0, 8_000.0) / band(signal, 40.0, 200.0);
        let gain = decibels(bright(&slap) / bright(&bass)) / 2.0;
        println!("the slap is {gain:.1} dB brighter");
        assert!(gain > 10.0, "{gain}");
    }

    #[test]
    fn the_pandeiros_jingles_follow_the_stroke() {
        use percussion::*;
        let jingly = |key| {
            let signal = stroke(key, 0.8, 0.5);
            band(&signal, 2_000.0, 16_000.0) / band(&signal, 100.0, 600.0)
        };
        let (thumb, fingers, muted) = (
            jingly(PANDEIRO_THUMB),
            jingly(PANDEIRO_FINGERS),
            jingly(PANDEIRO_MUTED),
        );
        println!("jingles over head: thumb {thumb:.2}, fingers {fingers:.2}, muted {muted:.2}");
        assert!(fingers > 3.0 * thumb, "{fingers} {thumb}");
        // The jingles' free-plate modes.
        let signal = stroke(PANDEIRO_FINGERS, 1.0, 0.5);
        let found = peaks(between(&signal, 0.0, 0.4), 2_000.0, 20_000.0, 8);
        for ratio in [1.0, 1.73, 2.33] {
            assert!(
                near(&found, 15, JINGLE_HERTZ * ratio, 1.6 * JINGLE_SPREAD),
                "{ratio}: {found:?}"
            );
        }
    }

    #[test]
    fn the_ganza_and_the_clap_sound_where_their_physics_puts_them() {
        // The tube's resonances colour the beads.
        let shaker = stroke(percussion::SHAKER, 0.8, 0.5);
        assert!(band(&shaker, 4_000.0, 11_000.0) > 10.0 * band(&shaker, 100.0, 2_500.0));
        // A shake grows as the beads gather speed, then they settle.
        let levels = measure::frame_levels(&shaker, 0.005);
        let loudest = measure::loudest_at(&levels, 0.005);
        assert!((0.005..0.08).contains(&loudest), "{loudest}");
        // The air between the palms: most of a clap is below 2.5 kHz, and
        // its three clappers are heard apart.
        let clap = stroke(percussion::HAND_CLAP, 0.8, 0.3);
        assert!(band(&clap, 700.0, 2_500.0) > 3.0 * band(&clap, 4_000.0, 12_000.0));
        let levels = measure::frame_levels(between(&clap, 0.0, 0.04), 0.001);
        let bursts = levels.windows(2).filter(|w| w[1] > w[0] + 4.0).count();
        assert!(bursts >= 2, "{levels:?}");
    }

    #[test]
    fn a_kit_takes_the_percussion_on_the_keys_it_has_free() {
        use crate::instruments::DrumModel;
        let brazilian = kit();
        for (base, own_clap) in [
            (DrumKit::style_909(), true),
            (DrumKit::style_cr78(), false),
            (DrumKit::acoustic(), true),
        ] {
            let with = base.with_brazilian_percussion(0.5);
            for key in percussion::ALL {
                let piece = with.piece(key).unwrap();
                if key == percussion::HAND_CLAP && own_clap {
                    assert_eq!(piece, base.piece(key).unwrap());
                    continue;
                }
                let model = brazilian.piece(key).unwrap();
                assert_eq!(piece.model, model.model);
                // The same level whatever the kit's own.
                assert!((piece.level * with.level - 0.5 * model.level).abs() < 1e-12);
            }
            assert_eq!(
                with.piece(percussion::HAND_CLAP).unwrap().model == DrumModel::HandClap,
                !own_clap
            );
        }
    }

    #[test]
    fn the_ring_and_bar_ratios_are_the_published_ones() {
        let ratios: Vec<f64> = (2..=6).map(|m| ring(f64::from(m)) / ring(2.0)).collect();
        for (got, want) in ratios.iter().zip([1.0, 2.828, 5.423, 8.770, 12.86]) {
            assert!((got - want).abs() < 0.01, "{got} against {want}");
        }
        let bar: Vec<f64> = (0..5)
            .map(|n| (bar_beta(n) / BAR_BETA[0]).powi(2))
            .collect();
        for (got, want) in bar.iter().zip([1.0, 2.756, 5.404, 8.933, 13.344]) {
            assert!((got - want).abs() < 0.01, "{got} against {want}");
        }
        // Past the table, the asymptote joins it smoothly.
        assert!((bar_beta(5) - 20.420_352).abs() < 1e-3);
        // A box of 41 litres with a hole of 11 cm rings near 80 Hz.
        assert!(
            (helmholtz_hertz() - 79.4).abs() < 1.0,
            "{}",
            helmholtz_hertz()
        );
    }
}

//! Acoustic drums by modal synthesis: drums with heads (bass drum, snare
//! with its rimshot and side stick, toms, congas and bongos) and cymbals
//! (crash, ride and its bell, splash, china and the hi-hat).
//!
//! The physics is that of N. H. Fletcher and T. D. Rossing, "The Physics
//! of Musical Instruments", 2nd ed., Springer 1998, chapters 18 (membranes
//! and drums) and 20 (cymbals, gongs and plates), and of T. D. Rossing,
//! "Science of Percussion Instruments", World Scientific 2000, chapters 2
//! to 4 (drums) and 9 (cymbals).

use super::common::{Noise, SILENCE, flush, t60_factor};
use super::modal::{
    BESSEL_ZEROS, MAX_PHASORS, PLATE_BANDS, Phasors, Plate, PlateSpec, PlateStrike, Pulse, bessel_j,
};
use crate::{ProcessSpec, Processor, StateVariableFilter, SvfMode};

/// How a piece is struck and set, as the kit hands it over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hit {
    /// The strike's level, as a gain: velocity and accent.
    pub(crate) amplitude: f64,
    /// How hard it is struck, 0 to about 1.3: velocity, and more with an
    /// accent. A harder strike is a shorter contact, so a brighter sound.
    pub(crate) hardness: f64,
    /// Tuning, as a ratio.
    pub(crate) tune: f64,
    /// A multiple of every decay time.
    pub(crate) decay: f64,
    /// The piece's three timbre settings, 0 to 1.
    pub(crate) tone: f64,
    pub(crate) snap: f64,
    pub(crate) texture: f64,
    /// What every random choice of the strike is drawn from.
    pub(crate) seed: u64,
}

/// A mode given by its frequency, decay and level.
#[derive(Debug, Clone, Copy)]
struct Partial {
    hertz: f64,
    t60: f64,
    level: f64,
}

const fn partial(hertz: f64, t60: f64, level: f64) -> Partial {
    Partial { hertz, t60, level }
}

/// A filter for one channel, prepared at `rate`.
fn filter(mode: SvfMode, hertz: f64, q: f64, rate: f64) -> StateVariableFilter<f64> {
    let mut filter = StateVariableFilter::new(mode, hertz.min(0.45 * rate), q);
    Processor::<f64>::prepare(
        &mut filter,
        &ProcessSpec {
            sample_rate: rate,
            max_block_size: 1,
            num_channels: 1,
        },
    );
    filter
}

/// A burst of filtered noise that rises in `attack` and falls in its decay
/// time, added to what is left of the last one: the noise of a stick on a
/// head, or of snare wires.
#[derive(Debug, Clone)]
struct Burst {
    slow: f64,
    fast: f64,
    slow_fall: f64,
    fast_fall: f64,
    band: StateVariableFilter<f64>,
    /// Whether the filter has been fed since it was last cleared.
    fed: bool,
}

impl Burst {
    fn new(rate: f64) -> Self {
        Self {
            slow: 0.0,
            fast: 0.0,
            slow_fall: 0.0,
            fast_fall: 0.0,
            band: filter(SvfMode::Bandpass, 3_000.0, 0.7, rate),
            fed: false,
        }
    }

    fn add(&mut self, level: f64, attack: f64, decay: f64, rate: f64) {
        self.slow += level;
        self.fast += level;
        self.slow_fall = t60_factor(decay, rate);
        self.fast_fall = t60_factor(attack, rate);
    }

    fn level(&self) -> f64 {
        (self.slow - self.fast).abs() + self.fast.abs()
    }

    #[inline]
    fn envelope(&mut self) -> f64 {
        let envelope = self.slow - self.fast;
        self.slow = flush(self.slow * self.slow_fall);
        self.fast = flush(self.fast * self.fast_fall);
        envelope
    }

    /// Whether the burst is over: its envelope has fallen to nothing.
    fn is_over(&self) -> bool {
        self.slow == 0.0 && self.fast == 0.0
    }

    #[inline]
    fn next(&mut self, noise: f64) -> f64 {
        if self.is_over() {
            // Over: the filter is cleared rather than left to ring down
            // into subnormal numbers, which are slow.
            if self.fed {
                Processor::<f64>::reset(&mut self.band);
                self.fed = false;
            }
            return 0.0;
        }
        self.fed = true;
        let envelope = self.envelope();
        self.band.process_sample(0, noise * envelope)
    }

    fn silence(&mut self) {
        self.slow = 0.0;
        self.fast = 0.0;
        self.fed = false;
        Processor::<f64>::reset(&mut self.band);
    }
}

/// The drums with heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HeadKind {
    Kick,
    Snare,
    Rimshot,
    SideStick,
    Tom,
    Conga,
    Bongo,
}

/// What makes one drum with heads.
#[derive(Debug, Clone, Copy)]
struct HeadSpec {
    /// The (0,1) mode of the batter head alone, in hertz.
    hertz: f64,
    /// How many of the batter's modes ring, from the lowest.
    modes: usize,
    /// The resonant head's modes over the batter's; zero for a drum with
    /// one head.
    far_ratio: f64,
    /// How stiffly the air inside couples the (0,1) modes of the two
    /// heads, and the modes with nodal diameters, as a fraction of the
    /// batter's stiffness in each mode. The (0,n) modes take the first
    /// over `n²`: the less a mode changes the volume inside, the less the
    /// air couples it.
    coupling: (f64, f64),
    /// The decay time of the batter's (0,1) mode, and how it falls with
    /// frequency: `t60 · (f01 / f)^t60_slope`.
    t60: f64,
    t60_slope: f64,
    /// A factor on the decay of the modes with nodal diameters, which
    /// radiate less than the (0,n) modes and so lose less.
    diameters: f64,
    /// A factor on the decay of the upper mode of the (0,1) pair, in which
    /// both heads move out together, compressing the air and radiating as
    /// a source.
    upper: f64,
    /// A factor on the decay of the resonant head's part of each pair: a
    /// snare head is damped by its wires.
    far_decay: f64,
    /// How much of the resonant head is heard beside the batter.
    far_heard: f64,
    /// Where the drum is heard from, as a radius over the head's and an
    /// angle from the strike, and how much brighter higher modes are heard
    /// (the radiation of a head rises with frequency).
    listen: (f64, f64),
    tilt: f64,
    /// Where it is struck at `tone` 0 and at 1, as a fraction of the
    /// radius from the centre.
    strike: (f64, f64),
    /// The contact time of the softest and the hardest strike, in seconds.
    contact: (f64, f64),
    /// How far a strike at full level stretches the head sharp, as a
    /// fraction of its frequencies.
    glide: f64,
    /// How much louder the heads ring while they swing wide after a full
    /// strike, as a gain added to 1, and how fast that falls away (60 dB,
    /// in seconds): a head swinging wide loses energy faster, to the air
    /// it pumps and to the wires it throws, than it does once the swing
    /// is small, so a drum's first tens of milliseconds decay faster than
    /// its tail.
    swing: (f64, f64),
    /// How long the sound takes to build up after a strike, as a 60 dB
    /// time: the wave the stick sets off takes a few milliseconds to cross
    /// the head and come back from the rim (`2a / c`, with `c =
    /// sqrt(T / σ)` some 100 m/s on a snare) before the head swings as its
    /// modes.
    build: f64,
    /// The noise of the stick or beater on the head: level, the band's
    /// centre for the softest and hardest strike, and the decay.
    crack: (f64, f64, f64, f64),
    /// The snare wires.
    wires: Wires,
    /// The hoop and stick, and how hard they are struck.
    rim: &'static [Partial],
    rim_level: f64,
    /// The shell's modes, rung a moment after the strike, and how hard.
    shell: &'static [Partial],
    shell_level: f64,
    /// The level of the heads, and of the whole drum.
    head_level: f64,
    level: f64,
}

/// The snare wires of a [`HeadSpec`]: level, and in seconds their decay
/// and rise (60 dB times), their delay after the strike, and how deeply
/// the snare head's swing gates them.
#[derive(Debug, Clone, Copy)]
struct Wires {
    level: f64,
    decay: f64,
    rise: f64,
    delay: f64,
    depth: f64,
}

const NO_WIRES: Wires = Wires {
    level: 0.0,
    decay: 0.1,
    rise: 0.01,
    delay: 0.001,
    depth: 0.0,
};

/// The hoop of a 14-inch snare drum, a ring whose bending modes go as
/// `n (n² − 1) / sqrt(n² + 1)` (Fletcher and Rossing, section 2.20), and a
/// hickory stick, a free bar whose modes go 1, 2.756, 5.404, 8.933
/// (section 2.17).
const SNARE_RIM: [Partial; 8] = [
    partial(1_150.0, 0.16, 1.0),
    partial(3_250.0, 0.11, 0.7),
    partial(6_230.0, 0.08, 0.5),
    partial(10_080.0, 0.05, 0.3),
    partial(380.0, 0.05, 0.5),
    partial(1_047.0, 0.04, 0.45),
    partial(2_053.0, 0.03, 0.35),
    partial(3_395.0, 0.025, 0.25),
];

/// A side stick is mostly the stick, held, and the hoop.
const SIDE_STICK_RIM: [Partial; 8] = [
    partial(1_150.0, 0.09, 0.5),
    partial(3_250.0, 0.07, 0.35),
    partial(6_230.0, 0.05, 0.2),
    partial(10_080.0, 0.03, 0.1),
    partial(410.0, 0.06, 1.0),
    partial(1_130.0, 0.05, 0.9),
    partial(2_215.0, 0.035, 0.6),
    partial(3_660.0, 0.025, 0.35),
];

/// The shell of a wooden snare drum.
const SNARE_SHELL: [Partial; 3] = [
    partial(720.0, 0.25, 1.0),
    partial(1_210.0, 0.2, 0.6),
    partial(1_830.0, 0.15, 0.4),
];

/// The shell of a tom.
const TOM_SHELL: [Partial; 2] = [partial(610.0, 0.1, 1.0), partial(1_050.0, 0.07, 0.6)];

/// The shells of a conga and a bongo.
const CONGA_SHELL: [Partial; 1] = [partial(330.0, 0.15, 1.0)];
const BONGO_SHELL: [Partial; 1] = [partial(900.0, 0.08, 1.0)];

/// The hoop of a tom, barely rung by a strike on the head.
const TOM_RIM: [Partial; 2] = [partial(950.0, 0.12, 1.0), partial(2_700.0, 0.08, 0.6)];

/// The snare drum, calibrated against a real one recorded for the project
/// (see the tests): its strong partials crowd 160 to 360 Hz, in pairs a few
/// per cent apart; its body falls 60 dB in about a fifth of a second; its
/// wires ring for half a second and peak 30 to 50 ms after the body.
const SNARE: HeadSpec = HeadSpec {
    hertz: 155.0,
    modes: 39,
    far_ratio: 1.06,
    coupling: (0.15, 0.12),
    t60: 1.2,
    t60_slope: 0.0,
    diameters: 1.8,
    upper: 1.0,
    far_decay: 0.5,
    far_heard: 0.9,
    listen: (0.55, 0.7),
    tilt: -0.6,
    strike: (0.1, 0.72),
    contact: (0.0035, 0.0022),
    glide: 0.012,
    swing: (6.0, 0.12),
    build: 0.008,
    crack: (1.2, 3_500.0, 5_500.0, 0.005),
    wires: Wires {
        level: 1.25,
        decay: 0.5,
        rise: 0.2,
        delay: 0.0012,
        depth: 0.6,
    },
    rim: &SNARE_RIM,
    rim_level: 0.02,
    shell: &SNARE_SHELL,
    shell_level: 0.4,
    head_level: 1.0,
    level: 0.24,
};

const fn head_spec(kind: HeadKind) -> HeadSpec {
    match kind {
        HeadKind::Snare => SNARE,
        HeadKind::Rimshot => HeadSpec {
            strike: (0.75, 0.9),
            contact: (0.0006, 0.00025),
            crack: (0.5, 3_000.0, 5_500.0, 0.008),
            wires: Wires {
                level: 0.6,
                ..SNARE.wires
            },
            rim_level: 0.9,
            head_level: 1.3,
            level: 0.15,
            ..SNARE
        },
        HeadKind::SideStick => HeadSpec {
            strike: (0.35, 0.45),
            swing: (0.5, 0.05),
            build: 0.003,
            contact: (0.002, 0.0012),
            crack: (0.25, 1_800.0, 3_000.0, 0.0025),
            wires: Wires {
                level: 0.1,
                decay: 0.3,
                ..SNARE.wires
            },
            rim: &SIDE_STICK_RIM,
            rim_level: 1.0,
            shell_level: 1.0,
            head_level: 0.25,
            level: 0.098,
            ..SNARE
        },
        HeadKind::Tom => HeadSpec {
            hertz: 98.0,
            far_ratio: 1.06,
            coupling: (0.12, 0.015),
            t60: 0.9,
            t60_slope: 0.7,
            diameters: 1.2,
            upper: 0.6,
            far_decay: 1.0,
            far_heard: 0.5,
            listen: (0.5, 0.6),
            tilt: 0.25,
            strike: (0.12, 0.62),
            contact: (0.0015, 0.00045),
            glide: 0.04,
            swing: (1.5, 0.1),
            build: 0.015,
            crack: (0.2, 1_500.0, 3_200.0, 0.005),
            wires: NO_WIRES,
            rim: &TOM_RIM,
            rim_level: 0.015,
            shell: &TOM_SHELL,
            shell_level: 0.3,
            level: 0.5,
            ..SNARE
        },
        HeadKind::Kick => HeadSpec {
            hertz: 52.0,
            modes: 30,
            far_ratio: 1.1,
            coupling: (0.1, 0.01),
            t60: 0.4,
            t60_slope: 1.0,
            diameters: 1.0,
            upper: 0.6,
            far_decay: 1.0,
            far_heard: 0.6,
            listen: (0.3, 0.3),
            tilt: 0.2,
            strike: (0.04, 0.3),
            contact: (0.0035, 0.0012),
            glide: 0.07,
            swing: (1.0, 0.08),
            build: 0.02,
            crack: (0.3, 2_200.0, 4_000.0, 0.003),
            wires: NO_WIRES,
            rim: &[],
            rim_level: 0.0,
            shell: &[],
            shell_level: 0.0,
            head_level: 1.0,
            level: 1.26,
        },
        HeadKind::Conga => HeadSpec {
            hertz: 205.0,
            modes: 30,
            far_ratio: 0.0,
            t60: 0.7,
            t60_slope: 1.0,
            diameters: 1.3,
            listen: (0.5, 0.8),
            tilt: 0.3,
            strike: (0.55, 0.88),
            contact: (0.0028, 0.0012),
            glide: 0.02,
            swing: (1.0, 0.06),
            build: 0.006,
            crack: (0.3, 1_500.0, 3_000.0, 0.006),
            wires: NO_WIRES,
            rim: &[],
            rim_level: 0.0,
            shell: &CONGA_SHELL,
            shell_level: 0.15,
            level: 0.67,
            ..SNARE
        },
        HeadKind::Bongo => HeadSpec {
            hertz: 420.0,
            modes: 24,
            far_ratio: 0.0,
            t60: 0.32,
            t60_slope: 1.0,
            diameters: 1.3,
            listen: (0.5, 0.8),
            tilt: 0.3,
            strike: (0.6, 0.9),
            contact: (0.0018, 0.0008),
            glide: 0.025,
            swing: (1.0, 0.06),
            build: 0.005,
            crack: (0.25, 2_000.0, 3_800.0, 0.004),
            wires: NO_WIRES,
            rim: &[],
            rim_level: 0.0,
            shell: &BONGO_SHELL,
            shell_level: 0.1,
            level: 0.9,
            ..SNARE
        },
    }
}

/// How a mode of the heads takes a strike: `projection · J_m(j r)` for a
/// strike on the batter at radius `r`.
#[derive(Debug, Clone, Copy, Default)]
struct Shape {
    m: u32,
    j: f64,
    projection: f64,
}

/// The (m, n) mode of two heads coupled by the air between them: the
/// frequencies of the pair it becomes, over the batter's alone, and each
/// one's shares of the batter and of the resonant head, `(ratio, batter,
/// far)`, lower first.
///
/// In units of the batter's stiffness in that mode the two heads are `[[1 +
/// k, k], [k, r² + k]]`, `r` the resonant head's tuning over the
/// batter's and `k` the coupling; the pair are its eigenvalues and
/// eigenvectors. In the upper mode the heads move out together.
fn coupled_pair(ratio: f64, coupling: f64) -> [(f64, f64, f64); 2] {
    let (a, d, k) = (1.0 + coupling, ratio * ratio + coupling, coupling);
    let theta = 0.5 * (2.0 * k).atan2(a - d);
    let (sin, cos) = theta.sin_cos();
    let upper = a * cos * cos + 2.0 * k * sin * cos + d * sin * sin;
    let lower = a * sin * sin - 2.0 * k * sin * cos + d * cos * cos;
    [
        (lower.max(0.0).sqrt(), -sin, cos),
        (upper.max(0.0).sqrt(), cos, sin),
    ]
}

/// A drum with one or two heads.
///
/// The batter head rings in the modes of an ideal membrane, its
/// frequencies over the lowest those of [`BESSEL_ZEROS`]. Each mode takes
/// the strike in proportion to its shape where the stick lands, `J_m(j_mn
/// r)` (a strike at the centre rings only the modes without nodal
/// diameters: dull; nearer the rim it rings them all: brighter and more
/// ringing), and is heard in proportion to its shape where the listener is
/// and to its modal mass, `J_{m+1}(j_mn)²`, falling.
///
/// On a drum with two heads, each mode of the batter is coupled to the
/// same mode of the resonant head by the air between them and by the shell
/// (Fletcher and Rossing, section 18.4; Rossing, chapter 4): every mode
/// becomes a pair a few per cent apart, one mostly batter and one mostly
/// resonant head. The (0,1) modes, which change the volume inside most,
/// couple most: their pair is a lower mode in which the heads move the same
/// way and an upper one in which both move out together. See
/// [`coupled_pair`]. The shell rings a moment after the strike.
///
/// Every frequency starts sharp by an amount that grows as the square of
/// the strike's level and settles as the swing dies: a head struck hard is
/// stretched tighter. The stick's force is a pulse whose length falls with
/// how hard it is struck, which filters the modes it rings. The stick's
/// own noise on the head, too dense in modes to model one by one, is a
/// short burst of band-passed noise.
///
/// A snare's wires lie against the snare head and are thrown off and
/// slapped back by its motion (Rossing, chapter 4): noise gated by the
/// sign of the snare head's displacement, which buzzes at its frequency,
/// in an envelope that starts a moment after the strike, rises over tens
/// of milliseconds and decays more slowly than the heads, band-limited to
/// 2 to 10 kHz.
#[derive(Debug, Clone)]
pub(crate) struct Heads {
    rate: f64,
    head: Phasors,
    shell: Phasors,
    rim: Phasors,
    shapes: [Shape; MAX_PHASORS],
    shell_inputs: [f64; MAX_PHASORS],
    head_pulse: Pulse,
    shell_pulse: Pulse,
    rim_pulse: Pulse,
    /// Samples until the shell is struck, and how.
    shell_wait: u32,
    shell_strike: (f64, f64),
    crack: Burst,
    wires: Burst,
    wires_high: StateVariableFilter<f64>,
    wires_higher: StateVariableFilter<f64>,
    wire_wait: u32,
    wire_pending: f64,
    wire_rise: f64,
    wire_decay: f64,
    wire_depth: f64,
    crack_noise: Noise,
    wire_noise: Noise,
    /// The level of the heads over their modes' own: `1 + slow − fast`,
    /// from nothing at a strike through the wide swing and back to 1.
    swing: (f64, f64),
    swing_fall: (f64, f64),
    spec: HeadSpec,
    configured: Option<(HeadKind, f64, f64, f64, f64)>,
    check_left: u32,
    alive: bool,
}

/// How many samples apart a drum checks whether it has fallen silent.
const CHECK: u32 = 64;

impl Heads {
    /// A drum at rest. Allocates.
    pub(crate) fn new(rate: f64) -> Self {
        let mut wires = Burst::new(rate);
        wires.band = filter(SvfMode::Highpass, 1_500.0, 0.707, rate);
        Self {
            rate,
            head: Phasors::default(),
            shell: Phasors::default(),
            rim: Phasors::default(),
            shapes: [Shape::default(); MAX_PHASORS],
            shell_inputs: [0.0; MAX_PHASORS],
            head_pulse: Pulse::default(),
            shell_pulse: Pulse::default(),
            rim_pulse: Pulse::default(),
            shell_wait: 0,
            shell_strike: (0.0, 0.0),
            crack: Burst::new(rate),
            wires,
            wires_high: filter(SvfMode::Lowpass, 8_000.0, 0.707, rate),
            wires_higher: filter(SvfMode::Lowpass, 8_000.0, 0.707, rate),
            wire_wait: 0,
            wire_pending: 0.0,
            wire_rise: 0.0,
            wire_decay: 0.0,
            wire_depth: 0.0,
            crack_noise: Noise::for_note(0, 0.0, 21),
            wire_noise: Noise::for_note(0, 0.0, 22),
            swing: (0.0, 0.0),
            swing_fall: (0.0, 0.0),
            spec: SNARE,
            configured: None,
            check_left: 0,
            alive: false,
        }
    }

    /// Sets the modes for `kind` tuned and set as `hit` says, unless they
    /// are set so already.
    fn configure(&mut self, kind: HeadKind, hit: &Hit) {
        let key = (kind, hit.tune, hit.decay, hit.texture, self.rate);
        if self.configured == Some(key) {
            return;
        }
        if self.configured.is_some_and(|(old, ..)| old != kind) {
            self.silence();
        }
        self.configured = Some(key);
        let spec = head_spec(kind);
        self.spec = spec;
        let rate = self.rate;
        let f01 = spec.hertz * hit.tune;
        let j01 = BESSEL_ZEROS[0].2;
        // How long a head rings: the texture of a snare or tom is how
        // freely it rings; of a kick, how open its front head is.
        let ring = match kind {
            HeadKind::Kick => 0.55 + 0.9 * hit.texture,
            HeadKind::Snare | HeadKind::Rimshot | HeadKind::Tom => 0.6 + 0.8 * hit.texture,
            _ => 0.7 + 0.6 * hit.texture,
        };
        let t60 = |hertz: f64, m: u32| {
            let diameters = if m > 0 { spec.diameters } else { 1.0 };
            (spec.t60 * (f01 / hertz).powf(spec.t60_slope) * diameters * hit.decay * ring)
                .clamp(0.005, 30.0)
        };
        let (rho, theta) = spec.listen;
        let heard = |m: u32, j: f64| {
            let mass = if m == 0 { 1.0 } else { 2.0 } / bessel_j(m + 1, j).powi(2);
            bessel_j(m, j * rho) * (f64::from(m) * theta).cos() * mass * (j / j01).powf(spec.tilt)
                / (1.0 + 0.5 * f64::from(m))
        };
        // Every mode's frequency, decay, weights and shape, before they
        // are scaled together.
        let mut modes = [(0.0, 0.0, 0.0, 0.0, Shape::default()); MAX_PHASORS];
        let mut count = 0;
        for &(m, n, j) in BESSEL_ZEROS.iter().take(spec.modes) {
            let (m, n) = (u32::from(m), f64::from(n));
            let base = f01 * j / j01;
            let sound = heard(m, j);
            // The wires lie along a diameter: the modes symmetric about it
            // move them, the others hardly.
            let along = if m % 2 == 0 {
                1.0 / (1.0 + f64::from(m))
            } else {
                0.1
            };
            let pair = if spec.far_ratio > 0.0 {
                let coupling = if m == 0 {
                    spec.coupling.0 / (n * n)
                } else {
                    spec.coupling.1
                };
                coupled_pair(spec.far_ratio, coupling)
            } else {
                [(1.0, 1.0, 0.0), (0.0, 0.0, 0.0)]
            };
            for (index, (ratio, batter, far)) in pair.into_iter().enumerate() {
                if ratio == 0.0 || count == MAX_PHASORS {
                    continue;
                }
                let hertz = base * ratio;
                let mut decay = t60(hertz, m) * (batter * batter + far * far * spec.far_decay);
                if m == 0 && n == 1.0 && index == 1 {
                    decay *= spec.upper;
                }
                modes[count] = (
                    hertz,
                    decay,
                    sound * (batter + spec.far_heard * far),
                    far * along,
                    Shape {
                        m,
                        j,
                        projection: batter,
                    },
                );
                count += 1;
            }
        }
        // Scaled so that a strike between the centre and the rim of
        // level 1 rings the heads at about 1.
        let middle = 0.5 * (spec.strike.0 + spec.strike.1);
        let total: f64 = modes[..count]
            .iter()
            .map(|(_, _, sound, _, shape)| {
                (sound * shape.projection * bessel_j(shape.m, shape.j * middle)).abs()
            })
            .sum();
        let scale = 1.0 / total.max(1e-12);
        self.head.clear();
        for &(hertz, decay, sound, far, shape) in &modes[..count] {
            // Written where the mode will go: a mode the bank leaves out
            // is written over by the next.
            self.shapes[self.head.len().min(MAX_PHASORS - 1)] = shape;
            self.head.add(hertz, decay, sound * scale, far, rate);
        }
        // The shell is tuned only a little by the heads' tension.
        let shell_tune = hit.tune.sqrt();
        self.shell.clear();
        for p in spec.shell {
            self.shell_inputs[self.shell.len().min(MAX_PHASORS - 1)] = p.level;
            self.shell
                .add(p.hertz * shell_tune, p.t60 * hit.decay, 1.0, 0.0, rate);
        }
        self.rim.clear();
        for p in spec.rim {
            self.rim
                .add(p.hertz * shell_tune, p.t60 * hit.decay, 1.0, 0.0, rate);
        }
        for (input, p) in self.rim.inputs_mut().iter_mut().zip(spec.rim) {
            *input = p.level;
        }
    }

    /// Strikes the drum, adding to whatever it is still ringing with.
    pub(crate) fn strike(&mut self, kind: HeadKind, hit: &Hit) {
        self.configure(kind, hit);
        let spec = self.spec;
        let rate = self.rate;
        let mut random = Noise::for_note(0, 0.0, hit.seed);
        let amplitude = hit.amplitude.max(0.0);
        let hard = hit.hardness.clamp(0.0, 1.5);
        // Where the stick lands, a little differently each time. A kick's
        // tone is its beater instead.
        let tone = if kind == HeadKind::Kick {
            0.3
        } else {
            hit.tone
        };
        let radius =
            (spec.strike.0 + (spec.strike.1 - spec.strike.0) * tone + 0.03 * random.next())
                .clamp(0.0, 0.95);
        for (input, shape) in self.head.inputs_mut().iter_mut().zip(&self.shapes) {
            *input = shape.projection * bessel_j(shape.m, shape.j * radius);
        }
        // The contact time: a kick's tone is the beater, felt to plastic.
        let (soft, hardest) = if kind == HeadKind::Kick {
            let beater = 1.0 - 0.75 * hit.tone;
            (spec.contact.0 * beater, spec.contact.1 * beater)
        } else {
            spec.contact
        };
        let contact = soft * (hardest / soft).powf(hard.min(1.3));
        self.head_pulse
            .start(contact, amplitude * spec.head_level, rate);
        let settle = spec.t60 * hit.decay / 2.0;
        self.head
            .glide(spec.glide * (amplitude * hard).powi(2), settle, rate);
        let swing = spec.swing.0;
        self.swing = (self.swing.0 + swing, self.swing.1 + swing + 1.0);
        self.swing_fall = (t60_factor(spec.swing.1, rate), t60_factor(spec.build, rate));
        if self.shell.len() > 0 {
            let len = self.shell.len();
            self.shell
                .inputs_mut()
                .copy_from_slice(&self.shell_inputs[..len]);
            let body = if kind == HeadKind::SideStick {
                0.5 + hit.texture
            } else {
                1.0
            };
            self.shell_wait = (0.0005 * rate).max(1.0) as u32;
            self.shell_strike = (contact.min(0.0006), amplitude * spec.shell_level * body);
        }
        if !spec.rim.is_empty() {
            let rim = match kind {
                HeadKind::Rimshot => spec.rim_level * (0.5 + hit.texture),
                HeadKind::SideStick => spec.rim_level * (0.7 + 0.6 * hit.tone),
                _ => spec.rim_level,
            };
            self.rim_pulse.start(0.00025, amplitude * rim, rate);
        }
        // The stick's noise: louder and brighter the harder it is.
        let (level, low, high, seconds) = spec.crack;
        let crack = match kind {
            HeadKind::Kick | HeadKind::Tom | HeadKind::Conga | HeadKind::Bongo => {
                0.3 + 1.4 * hit.snap
            }
            _ => 1.0,
        };
        let centre = low * (high / low).powf(hard.min(1.2));
        self.crack.band.set_frequency(centre.min(0.45 * rate));
        // It rises over the contact, as the stick presses into the head.
        self.crack.add(
            amplitude * level * crack * (0.2 + 0.8 * hard),
            contact,
            seconds,
            rate,
        );
        // The wires: relatively louder for soft strikes, and shorter.
        let wires = spec.wires;
        if wires.level > 0.0 {
            self.wire_pending += amplitude.powf(0.85) * wires.level * 2.0 * hit.snap;
            self.wire_wait = (wires.delay * rate).max(1.0) as u32;
            self.wire_rise = wires.rise;
            self.wire_decay =
                wires.decay * hit.decay * (0.6 + 0.4 * hard.min(1.0)) * (0.7 + 0.6 * hit.snap);
            self.wire_depth = wires.depth;
        }
        self.check_left = CHECK;
        self.alive = true;
    }

    /// One sample.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        let rate = self.rate;
        let (head, motion) = self.head.next(self.head_pulse.next());
        let mut out = head * (1.0 + self.swing.0 - self.swing.1).max(0.0);
        self.swing = (
            flush(self.swing.0 * self.swing_fall.0),
            flush(self.swing.1 * self.swing_fall.1),
        );
        if self.shell_wait > 0 {
            self.shell_wait -= 1;
            if self.shell_wait == 0 {
                let (contact, level) = self.shell_strike;
                self.shell_pulse.start(contact, level, rate);
            }
        }
        if self.shell.len() > 0 {
            out += self.shell.next(self.shell_pulse.next()).0;
        }
        if self.rim.len() > 0 {
            out += self.rim.next(self.rim_pulse.next()).0;
        }
        out += self.crack.next(self.crack_noise.next());
        if self.wire_wait > 0 {
            self.wire_wait -= 1;
            if self.wire_wait == 0 {
                let pending = self.wire_pending;
                self.wire_pending = 0.0;
                self.wires
                    .add(pending, self.wire_rise, self.wire_decay, rate);
            }
        }
        if self.wires.slow != 0.0 {
            let gate = if motion > 0.0 {
                1.0
            } else {
                1.0 - self.wire_depth
            };
            let buzz = self.wires.next(self.wire_noise.next() * gate);
            let buzz = self.wires_high.process_sample(0, buzz);
            out += self.wires_higher.process_sample(0, buzz);
        }
        self.check_left -= 1;
        if self.check_left == 0 {
            self.check_left = CHECK;
            let level = self.head.level()
                + self.shell.level()
                + self.rim.level()
                + self.crack.level()
                + self.wires.level()
                + self.wire_pending;
            let busy = self.head_pulse.is_running()
                || self.shell_pulse.is_running()
                || self.rim_pulse.is_running()
                || self.shell_wait > 0
                || self.wire_wait > 0;
            if level < 0.1 * SILENCE && !busy {
                self.silence();
            } else {
                self.head.flush_quiet();
                self.shell.flush_quiet();
                self.rim.flush_quiet();
            }
        }
        out * self.spec.level
    }

    pub(crate) fn is_sounding(&self) -> bool {
        self.alive
    }

    /// At rest, the modes kept.
    pub(crate) fn silence(&mut self) {
        self.alive = false;
        self.head.rest();
        self.shell.rest();
        self.rim.rest();
        self.head_pulse.stop();
        self.shell_pulse.stop();
        self.rim_pulse.stop();
        self.shell_wait = 0;
        self.crack.silence();
        self.wires.silence();
        Processor::<f64>::reset(&mut self.wires_high);
        Processor::<f64>::reset(&mut self.wires_higher);
        self.wire_wait = 0;
        self.wire_pending = 0.0;
        self.crack_noise = Noise::for_note(0, 0.0, 21);
        self.wire_noise = Noise::for_note(0, 0.0, 22);
        self.swing = (0.0, 0.0);
        self.check_left = CHECK;
    }

    /// The frequencies of the heads' modes and their decay times, for the
    /// tests.
    #[cfg(test)]
    pub(crate) fn modes(&self) -> Vec<(f64, f64)> {
        (0..self.head.len())
            .map(|i| {
                let hertz = self.head.omega(i) * self.rate / core::f64::consts::TAU;
                (hertz, self.head.t60(i, self.rate))
            })
            .collect()
    }
}

/// How long the tick of a stick on a cymbal takes to rise, as a 60 dB
/// time.
const TICK_RISE: f64 = 0.0015;

/// The cymbals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CymbalKind {
    Crash,
    Ride,
    RideBell,
    Splash,
    China,
    ClosedHat,
    PedalHat,
    OpenHat,
}

/// What makes one cymbal.
#[derive(Debug, Clone, Copy)]
struct CymbalSpec {
    plate: PlateSpec,
    /// The contact time of a full strike with the shoulder of the stick
    /// (`tone` 0) and its tip (`tone` 1); a soft strike's is longer.
    contact: (f64, f64),
    /// The strike's tilt with the shoulder and with the tip.
    tilt: (f64, f64),
    low_cut: f64,
    bands: [f64; PLATE_BANDS],
    hold: [f64; PLATE_BANDS],
    /// The cascade's rates at full strength (`snap` 1 and full hardness).
    fill: [f64; PLATE_BANDS],
    gain: [f64; PLATE_BANDS],
    /// The tick of the stick on the metal: level, centre and decay.
    tick: (f64, f64, f64),
    /// Partials that stand out of the plate's dense modes, in hertz, and
    /// how hard a strike rings them: the bell of a ride, the strongest low
    /// modes of a crash.
    partials: &'static [Partial],
    partial_level: f64,
    level: f64,
}

/// The decay of a crash cymbal's modes by frequency, from a real 17-inch
/// crash recorded for the project (the octave bands' 60 dB decay times,
/// measured from their peaks): the low end dies within half a second, the
/// middle rings longest, 3.6 s at 2 to 4 kHz.
const CRASH_T60: [(f64, f64); 8] = [
    (89.0, 0.1),
    (177.0, 0.18),
    (354.0, 0.4),
    (707.0, 4.0),
    (1_414.0, 3.6),
    (2_828.0, 3.6),
    (5_657.0, 3.0),
    (11_314.0, 2.5),
];

/// The strongest low partials of that crash, in its spectrum from 50 ms
/// on: the lowest few die fast, the pairs near 530, 680 and 1 870 Hz ring.
const CRASH_PARTIALS: [Partial; 17] = [
    partial(45.4, 0.6, 0.1),
    partial(68.8, 0.1, 0.8),
    partial(99.6, 0.12, 0.7),
    partial(131.0, 0.15, 0.5),
    partial(168.0, 0.18, 0.45),
    partial(212.0, 0.22, 0.35),
    partial(290.0, 0.5, 0.08),
    partial(380.0, 0.55, 0.05),
    partial(528.8, 3.2, 0.043),
    partial(543.5, 3.2, 0.055),
    partial(670.9, 2.8, 0.031),
    partial(687.0, 2.8, 0.031),
    partial(1_820.8, 3.0, 0.026),
    partial(1_854.5, 3.0, 0.03),
    partial(1_869.1, 3.0, 0.041),
    partial(1_907.2, 3.0, 0.03),
    partial(1_923.3, 3.0, 0.039),
];

/// A 21-inch ride, thicker and bigger than the crash: everything rings
/// longer.
const RIDE_T60: [(f64, f64); 8] = [
    (89.0, 0.2),
    (177.0, 0.35),
    (354.0, 0.8),
    (707.0, 4.0),
    (1_414.0, 4.6),
    (2_828.0, 5.4),
    (5_657.0, 4.6),
    (11_314.0, 3.6),
];

/// The bell of a ride: few, clear partials, ringing long.
const RIDE_BELL: [Partial; 7] = [
    partial(740.0, 3.6, 1.0),
    partial(1_125.0, 3.0, 0.8),
    partial(1_606.0, 2.6, 0.7),
    partial(2_161.0, 2.2, 0.55),
    partial(2_768.0, 1.8, 0.45),
    partial(3_463.0, 1.5, 0.3),
    partial(4_225.0, 1.2, 0.2),
];

/// A 10-inch splash: an octave higher and short.
const SPLASH_T60: [(f64, f64); 7] = [
    (177.0, 0.08),
    (354.0, 0.15),
    (707.0, 0.6),
    (1_414.0, 1.2),
    (2_828.0, 1.5),
    (5_657.0, 1.4),
    (11_314.0, 1.1),
];

/// An 18-inch china: shorter than the crash, and its mid-range strong.
const CHINA_T60: [(f64, f64); 8] = [
    (89.0, 0.12),
    (177.0, 0.2),
    (354.0, 0.45),
    (707.0, 2.0),
    (1_414.0, 2.4),
    (2_828.0, 2.6),
    (5_657.0, 2.2),
    (11_314.0, 1.8),
];

/// A pair of 14-inch hi-hats just touching: the top cymbal rings, the
/// bottom one sizzling against it, much shorter than a crash.
const HAT_OPEN_T60: [(f64, f64); 7] = [
    (177.0, 0.1),
    (354.0, 0.25),
    (707.0, 0.8),
    (1_414.0, 1.1),
    (2_828.0, 1.3),
    (5_657.0, 1.2),
    (11_314.0, 1.0),
];

/// The same pair closed: pressed together, every mode is damped hard, the
/// low ones most.
const HAT_CLOSED_T60: [(f64, f64); 4] = [
    (354.0, 0.03),
    (1_414.0, 0.06),
    (5_657.0, 0.09),
    (11_314.0, 0.08),
];

const CRASH_PLATE: PlateSpec = PlateSpec {
    seed: 1_701,
    modes: 440,
    low_hz: 60.0,
    knee_hz: 500.0,
    high_hz: 16_000.0,
    split: 0.005,
    t60: &CRASH_T60,
    edges: [600.0, 2_000.0, 7_000.0],
};

const RIDE_PLATE: PlateSpec = PlateSpec {
    seed: 2_101,
    modes: 460,
    low_hz: 60.0,
    knee_hz: 700.0,
    high_hz: 19_500.0,
    split: 0.003,
    t60: &RIDE_T60,
    edges: [2_000.0, 4_000.0, 8_000.0],
};

const SPLASH_PLATE: PlateSpec = PlateSpec {
    seed: 1_001,
    modes: 280,
    low_hz: 200.0,
    knee_hz: 1_500.0,
    high_hz: 19_500.0,
    split: 0.006,
    t60: &SPLASH_T60,
    edges: [2_500.0, 5_000.0, 9_500.0],
};

const CHINA_PLATE: PlateSpec = PlateSpec {
    seed: 1_802,
    modes: 440,
    low_hz: 80.0,
    knee_hz: 700.0,
    high_hz: 17_000.0,
    split: 0.02,
    t60: &CHINA_T60,
    edges: [1_500.0, 3_000.0, 6_500.0],
};

const HAT_OPEN: PlateSpec = PlateSpec {
    seed: 1_401,
    modes: 320,
    low_hz: 200.0,
    knee_hz: 1_200.0,
    high_hz: 19_500.0,
    split: 0.008,
    t60: &HAT_OPEN_T60,
    edges: [2_500.0, 5_000.0, 9_000.0],
};

const HAT_CLOSED: PlateSpec = PlateSpec {
    t60: &HAT_CLOSED_T60,
    ..HAT_OPEN
};

/// The crash cymbal, calibrated against a real one recorded for the
/// project (see the tests): everything up to 2 kHz peaks at the strike;
/// 2 to 4 kHz peaks 0.17 s after it and 4 to 16 kHz about 0.22 s after,
/// as the cascade carries the energy up; the centroid climbs from 1.5 kHz
/// to 5.5 kHz in 0.4 s and sinks slowly.
const CRASH: CymbalSpec = CymbalSpec {
    plate: CRASH_PLATE,
    contact: (0.0004, 0.00015),
    tilt: (-0.6, -0.3),
    low_cut: 300.0,
    bands: [0.004, 0.006, 0.02, 0.05],
    hold: [0.3, 0.3, 0.35, 0.35],
    fill: [0.0, 40.0, 75.0, 60.0],
    gain: [0.0, 0.01, 0.45, 0.065],
    tick: (0.1, 7_000.0, 0.001),
    partials: &CRASH_PARTIALS,
    partial_level: 3.0,
    level: 0.052,
};

const fn cymbal_spec(kind: CymbalKind) -> CymbalSpec {
    match kind {
        CymbalKind::Crash => CRASH,
        CymbalKind::Ride | CymbalKind::RideBell => CymbalSpec {
            plate: RIDE_PLATE,
            contact: (0.0005, 0.0002),
            tilt: (0.3, 0.6),
            low_cut: 300.0,
            bands: [0.8, 0.6, 0.35, 0.15],
            hold: [0.6, 0.45, 0.45, 0.45],
            fill: [0.0, 25.0, 25.0, 20.0],
            gain: [0.0, 0.25, 0.3, 0.3],
            tick: (0.1, 5_000.0, 0.0008),
            partials: &RIDE_BELL,
            partial_level: 0.08,
            level: 0.186,
        },
        CymbalKind::Splash => CymbalSpec {
            plate: SPLASH_PLATE,
            contact: (0.0006, 0.00025),
            tilt: (0.2, 0.5),
            low_cut: 500.0,
            bands: [1.0, 0.35, 0.2, 0.1],
            hold: [0.25, 0.2, 0.2, 0.2],
            fill: [0.0, 120.0, 120.0, 110.0],
            gain: [0.0, 0.5, 0.5, 0.25],
            tick: (0.06, 7_000.0, 0.0008),
            partials: &[],
            partial_level: 0.0,
            level: 0.142,
        },
        CymbalKind::China => CymbalSpec {
            plate: CHINA_PLATE,
            contact: (0.0008, 0.0003),
            tilt: (0.0, 0.25),
            low_cut: 200.0,
            bands: [1.0, 0.5, 0.25, 0.1],
            hold: [0.4, 0.35, 0.35, 0.35],
            fill: [0.0, 140.0, 120.0, 100.0],
            gain: [0.0, 0.6, 0.45, 0.2],
            tick: (0.08, 4_000.0, 0.001),
            partials: &[],
            partial_level: 0.0,
            level: 0.093,
        },
        CymbalKind::ClosedHat => CymbalSpec {
            plate: HAT_CLOSED,
            contact: (0.0004, 0.00015),
            tilt: (0.6, 0.9),
            low_cut: 1_500.0,
            bands: [0.3, 0.6, 1.0, 1.0],
            hold: [0.08, 0.06, 0.05, 0.04],
            fill: [0.0, 60.0, 60.0, 60.0],
            gain: [0.0, 0.3, 0.4, 0.4],
            tick: (0.25, 8_000.0, 0.0012),
            partials: &[],
            partial_level: 0.0,
            level: 0.54,
        },
        CymbalKind::PedalHat => CymbalSpec {
            plate: HAT_CLOSED,
            contact: (0.0018, 0.0012),
            tilt: (0.2, 0.4),
            low_cut: 600.0,
            bands: [0.6, 1.0, 0.7, 0.5],
            hold: [0.06, 0.05, 0.05, 0.04],
            fill: [0.0, 60.0, 60.0, 60.0],
            gain: [0.0, 0.3, 0.4, 0.4],
            tick: (0.15, 2_500.0, 0.004),
            partials: &[],
            partial_level: 0.0,
            level: 0.56,
        },
        CymbalKind::OpenHat => CymbalSpec {
            plate: HAT_OPEN,
            contact: (0.0005, 0.0002),
            tilt: (0.4, 0.7),
            low_cut: 800.0,
            bands: [0.5, 0.6, 0.6, 0.5],
            hold: [1.0, 0.8, 0.8, 0.8],
            fill: [0.0, 40.0, 40.0, 40.0],
            gain: [0.0, 0.3, 0.4, 0.25],
            tick: (0.15, 7_000.0, 0.001),
            partials: &[],
            partial_level: 0.0,
            level: 0.077,
        },
    }
}

/// A cymbal or a hi-hat: a [`Plate`] of hundreds of modes, the partials
/// that stand out of them (a ride's bell), and the tick of the stick.
///
/// A strike rings the plate's modes through the force pulse of the stick,
/// with random shares (where a stick lands among hundreds of mode shapes
/// is as good as random), mostly in the low bands; the cascade of the
/// plate then carries energy up the bands over the next tenths of a
/// second, faster and further the harder the strike, which is what makes
/// a cymbal struck hard bloom into a bright wash and one struck softly
/// stay dark. Struck again while it rings, a cymbal adds the new strike to
/// its ringing, as the instrument does.
#[derive(Debug, Clone)]
pub(crate) struct Cymbal {
    rate: f64,
    plate: Plate,
    partials: Phasors,
    pulse: Pulse,
    tick: Burst,
    noise: Noise,
    spec: CymbalSpec,
    configured: Option<(CymbalKind, f64, f64, f64)>,
    check_left: u32,
    alive: bool,
}

impl Cymbal {
    /// A cymbal at rest. Allocates.
    pub(crate) fn new(rate: f64) -> Self {
        let mut tick = Burst::new(rate);
        tick.band = filter(SvfMode::Highpass, 4_000.0, 0.707, rate);
        Self {
            rate,
            plate: Plate::new(),
            partials: Phasors::default(),
            pulse: Pulse::default(),
            tick,
            noise: Noise::for_note(0, 0.0, 31),
            spec: CRASH,
            configured: None,
            check_left: 0,
            alive: false,
        }
    }

    fn configure(&mut self, kind: CymbalKind, hit: &Hit) {
        // The open hi-hat's texture is how far it is open: how long it
        // rings.
        let decay = match kind {
            CymbalKind::OpenHat => hit.decay * (0.35 + 1.3 * hit.texture),
            _ => hit.decay,
        };
        let key = (kind, hit.tune, decay, self.rate);
        if self.configured == Some(key) {
            return;
        }
        if self.configured.is_some_and(|(old, ..)| old != kind) {
            self.silence();
        }
        self.configured = Some(key);
        let spec = cymbal_spec(kind);
        self.spec = spec;
        self.plate
            .configure(&spec.plate, hit.tune, decay, self.rate);
        self.partials.clear();
        for p in spec.partials {
            self.partials
                .add(p.hertz * hit.tune, p.t60 * decay, 1.0, 0.0, self.rate);
        }
    }

    /// Strikes the cymbal, adding to whatever it is still ringing with.
    pub(crate) fn strike(&mut self, kind: CymbalKind, hit: &Hit) {
        self.configure(kind, hit);
        let spec = self.spec;
        let rate = self.rate;
        let mut random = Noise::for_note(0, 0.0, hit.seed);
        let amplitude = hit.amplitude.max(0.0);
        let hard = hit.hardness.clamp(0.0, 1.5);
        let tone = hit.tone.clamp(0.0, 1.0);
        let full = spec.contact.0 * (spec.contact.1 / spec.contact.0).powf(tone);
        // A soft strike stays on the metal longer.
        let contact = full * (1.8 - 0.8 * hard.min(1.0));
        let tilt = spec.tilt.0 + (spec.tilt.1 - spec.tilt.0) * tone;
        // How strongly the cascade runs: the shimmer setting, and how hard
        // the cymbal is struck (the coupling of its modes is nonlinear, so
        // it grows with the swing).
        let strength = 2.0 * hit.snap * (0.25 + 0.75 * hard.min(1.3));
        let mut fill = spec.fill;
        for rate in &mut fill {
            *rate *= strength;
        }
        let (plate_share, partial_share) = match kind {
            CymbalKind::RideBell => (0.45, 1.0),
            _ => (1.0, spec.partial_level * (0.5 + hit.texture)),
        };
        let how = PlateStrike {
            level: amplitude * plate_share,
            tilt,
            low_cut: spec.low_cut,
            bands: spec.bands,
            hold: spec.hold,
            fill,
            gain: spec.gain,
        };
        self.plate.strike(&how, &mut random, rate);
        let contact = match kind {
            CymbalKind::RideBell => contact.min(0.0003),
            _ => contact,
        };
        self.pulse.start(contact, 1.0, rate);
        for (input, p) in self.partials.inputs_mut().iter_mut().zip(spec.partials) {
            *input = amplitude * partial_share * p.level;
        }
        let (level, centre, seconds) = spec.tick;
        self.tick.band.set_frequency(centre.min(0.45 * rate));
        // It rises over the contact of the stick.
        self.tick.add(
            amplitude * level * (0.4 + 0.6 * hard),
            TICK_RISE,
            seconds,
            rate,
        );
        self.check_left = CHECK;
        self.alive = true;
    }

    /// One sample.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        let force = self.pulse.next();
        let mut out = self.plate.next(force);
        if self.partials.len() > 0 {
            out += self.partials.next(force).0;
        }
        out += self.tick.next(self.noise.next());
        self.check_left -= 1;
        if self.check_left == 0 {
            self.check_left = CHECK;
            if !self.plate.is_sounding()
                && self.partials.level() + self.tick.level() < 0.1 * SILENCE
                && !self.pulse.is_running()
            {
                self.silence();
            } else {
                self.partials.flush_quiet();
            }
        }
        out * self.spec.level
    }

    pub(crate) fn is_sounding(&self) -> bool {
        self.alive
    }

    /// At rest, the modes kept.
    pub(crate) fn silence(&mut self) {
        self.alive = false;
        self.plate.rest();
        self.partials.rest();
        self.pulse.stop();
        self.tick.silence();
        self.noise = Noise::for_note(0, 0.0, 31);
        self.check_left = CHECK;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments::measure::{RATE, amplitude_at, between, frequency_near};

    fn hit(amplitude: f64, decay: f64) -> Hit {
        Hit {
            amplitude,
            hardness: 0.8,
            tune: 1.0,
            decay,
            tone: 0.5,
            snap: 0.5,
            texture: 0.5,
            seed: 1,
        }
    }

    fn ring_heads(kind: HeadKind, hit: &Hit, seconds: f64) -> (Heads, Vec<f64>) {
        let mut heads = Heads::new(RATE);
        heads.strike(kind, hit);
        let signal = (0..(seconds * RATE) as usize)
            .map(|_| heads.next())
            .collect();
        (heads, signal)
    }

    #[test]
    fn the_heads_ring_at_the_frequencies_of_the_model() {
        // Struck softly, so that the glide is negligible, and left to ring
        // long, so that close modes can be told apart: every mode that
        // rings loud enough to measure and is far enough from the others
        // is within 0.5 % (it is far closer) of its frequency in the model.
        for (kind, fewest) in [
            (HeadKind::Tom, 10),
            (HeadKind::Snare, 10),
            (HeadKind::Kick, 4),
            (HeadKind::Conga, 5),
        ] {
            let (heads, signal) = ring_heads(kind, &hit(0.05, 8.0), 5.0);
            let modes = heads.modes();
            let window = between(&signal, 0.5, 4.5);
            let amplitudes: Vec<f64> = modes
                .iter()
                .map(|&(hertz, _)| amplitude_at(window, hertz))
                .collect();
            let loudest = amplitudes.iter().copied().fold(0.0, f64::max);
            let mut measured = 0;
            let mut worst = 0.0f64;
            for (i, &(hertz, _)) in modes.iter().enumerate() {
                let gap = modes
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k != i)
                    .map(|(_, &(f, _))| (f - hertz).abs())
                    .fold(f64::INFINITY, f64::min);
                if gap < 2.0 || amplitudes[i] < 1e-2 * loudest {
                    continue;
                }
                let found = frequency_near(window, hertz);
                worst = worst.max((found / hertz - 1.0).abs());
                measured += 1;
            }
            println!(
                "{kind:?}: {measured} of {} modes, worst {:.5} %",
                modes.len(),
                100.0 * worst
            );
            assert!(measured >= fewest, "{kind:?}: {measured}");
            assert!(worst < 0.005, "{kind:?}: {worst}");
        }
    }

    #[test]
    fn every_mode_decays_in_the_time_of_the_model() {
        // Once the wide swing is over, each mode loud enough to measure and
        // far enough from the others falls at the rate its decay time sets,
        // within 5 %: its level through windows of a second, a span
        // apart. On the snare, where the wires ring too, only modes well
        // apart are measured.
        for (kind, apart, span, fewest) in [
            (HeadKind::Tom, 8.0, 2.5, 3),
            (HeadKind::Snare, 15.0, 2.5, 2),
            (HeadKind::Kick, 4.0, 1.0, 2),
        ] {
            let (heads, signal) = ring_heads(kind, &hit(0.5, 4.0), 4.0);
            let modes = heads.modes();
            let early = between(&signal, 0.4, 1.4);
            let later = between(&signal, 0.4 + span, 1.4 + span);
            let amplitudes: Vec<f64> = modes
                .iter()
                .map(|&(hertz, _)| amplitude_at(early, hertz))
                .collect();
            let lasting: Vec<f64> = modes
                .iter()
                .map(|&(hertz, _)| amplitude_at(later, hertz))
                .collect();
            let loudest = amplitudes.iter().copied().fold(0.0, f64::max);
            let mut measured = 0;
            for (i, &(hertz, t60)) in modes.iter().enumerate() {
                let gap = modes
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k != i)
                    .map(|(_, &(f, _))| (f - hertz).abs())
                    .fold(f64::INFINITY, f64::min);
                // Nor may a louder mode ring nearby by the end, to leak in.
                let outlasted = modes
                    .iter()
                    .zip(&lasting)
                    .enumerate()
                    .any(|(k, (&(f, _), &a))| {
                        k != i && (f - hertz).abs() < 3.0 * apart && a > lasting[i]
                    });
                // The shell and the rim ring beside the heads.
                let spec = head_spec(kind);
                let beside = spec
                    .shell
                    .iter()
                    .chain(spec.rim)
                    .any(|p| (p.hertz - hertz).abs() < 20.0);
                if gap < apart || beside || outlasted || amplitudes[i] < 0.01 * loudest {
                    continue;
                }
                let fall = 20.0 * (amplitudes[i] / lasting[i]).log10();
                let decay = span * 60.0 / fall;
                assert!(
                    (decay / t60 - 1.0).abs() < 0.05,
                    "{kind:?} {hertz:.1} Hz: {decay} against {t60}"
                );
                measured += 1;
            }
            println!("{kind:?}: {measured} modes measured");
            assert!(measured >= fewest, "{kind:?}");
        }
    }

    #[test]
    fn a_head_struck_hard_starts_sharp_and_settles() {
        // The loudest mode of a conga (one head, so no close pairs to blur
        // a short measurement), over its first 30 ms and once settled.
        let pitch = |amplitude| {
            let (heads, signal) = ring_heads(HeadKind::Conga, &hit(amplitude, 2.0), 1.0);
            let modes = heads.modes();
            let (hertz, _) = modes
                .iter()
                .copied()
                .max_by(|a, b| {
                    amplitude_at(between(&signal, 0.3, 0.6), a.0)
                        .total_cmp(&amplitude_at(between(&signal, 0.3, 0.6), b.0))
                })
                .unwrap();
            let early = frequency_near(between(&signal, 0.002, 0.032), hertz);
            let late = frequency_near(between(&signal, 0.4, 0.9), hertz);
            (early / hertz, late / hertz)
        };
        // Measured the same way, struck softly it hardly glides: the
        // difference is the glide.
        let (hard_early, hard_late) = pitch(1.0);
        let (soft_early, soft_late) = pitch(0.1);
        println!("hard: {hard_early:.4} then {hard_late:.5}; soft {soft_early:.4}");
        assert!(
            hard_early - soft_early > 0.005,
            "{hard_early} against {soft_early}"
        );
        assert!((hard_late - 1.0).abs() < 0.002, "{hard_late}");
        assert!((soft_late - 1.0).abs() < 0.002, "{soft_late}");
    }

    #[test]
    fn a_strike_at_the_centre_rings_only_the_modes_without_diameters() {
        // The (1,1) pair against the lowest pair, struck near the centre
        // and near the rim.
        let share = |tone| {
            let mut heads = Heads::new(RATE);
            heads.strike(
                HeadKind::Tom,
                &Hit {
                    tone,
                    ..hit(0.05, 4.0)
                },
            );
            let signal: Vec<f64> = (0..(1.0 * RATE) as usize).map(|_| heads.next()).collect();
            let modes = heads.modes();
            let window = between(&signal, 0.2, 0.8);
            let level = |range: core::ops::Range<usize>| {
                modes[range]
                    .iter()
                    .map(|&(hertz, _)| amplitude_at(window, hertz))
                    .sum::<f64>()
            };
            // The pairs come in the order of the zeros: (0,1), then (1,1).
            level(2..4) / level(0..2)
        };
        let (centre, rim) = (share(0.0), share(1.0));
        println!("(1,1) over (0,1): {centre:.3} at the centre, {rim:.3} near the rim");
        assert!(rim > 3.0 * centre, "{rim} against {centre}");
    }

    /// The snare from 2 to 10 kHz, struck with the wires set to `snap`.
    fn wire_band(snap: f64) -> Vec<f64> {
        let mut heads = Heads::new(RATE);
        heads.strike(
            HeadKind::Snare,
            &Hit {
                snap,
                ..hit(0.5, 1.0)
            },
        );
        let signal: Vec<f64> = (0..(0.8 * RATE) as usize).map(|_| heads.next()).collect();
        crate::instruments::measure::band_passed(&signal, 2_000.0, 10_000.0)
    }

    fn energy(signal: &[f64], from: f64, to: f64) -> f64 {
        between(signal, from, to).iter().map(|x| x * x).sum()
    }

    #[test]
    fn the_snare_wires_buzz_after_the_strike_and_die_away() {
        // With the wires, the band of the wires holds far more energy after
        // the strike than without them.
        let (with, without) = (wire_band(0.5), wire_band(0.0));
        let gain = 10.0 * (energy(&with, 0.02, 0.3) / energy(&without, 0.02, 0.3)).log10();
        println!("the wires add {gain:.1} dB from 2 to 10 kHz");
        assert!(gain > 10.0, "{gain}");
        // The wires alone: they start a moment after the strike, rise,
        // then die away.
        let wires: Vec<f64> = with.iter().zip(&without).map(|(a, b)| a - b).collect();
        let (first, loudest, late) = (
            energy(&wires, 0.0, 0.001),
            energy(&wires, 0.03, 0.05) / 20.0,
            energy(&wires, 0.6, 0.62) / 20.0,
        );
        assert!(first < 1e-2 * loudest, "{first} against {loudest}");
        assert!(late < 1e-3 * loudest, "{late} against {loudest}");
    }
}

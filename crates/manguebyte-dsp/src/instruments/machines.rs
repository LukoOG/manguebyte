//! Drum machines in the manner of the 909, the 606 and the CR-78, modelled
//! on the published descriptions of their circuits and on what their
//! service notes say each control does, not on recordings of them.
//!
//! Every drum here is a recipe of a few parts, the parts those circuits are
//! made of:
//!
//! - **bodies**: an oscillator or a struck resonance whose pitch falls
//!   from above to where it settles. The 909's bass drum, snare and toms
//!   are oscillators whose triangle is rounded towards a sine by a
//!   waveshaper; the 606's and the CR-78's drums are bridged-T resonators
//!   struck by a trigger pulse, which ring as decaying sines.
//! - **a click**: the trigger pulse itself leaking to the output, through
//!   a low-pass.
//! - **noise paths**: white noise through a high-pass, a low-pass and a
//!   band-pass, in an envelope with a spike and a tail, or in a train of
//!   bursts (the hand clap's retriggered envelope; the guiro's scrape).
//! - **metal**: six square-wave oscillators at inharmonic frequencies,
//!   summed and ring-modulated in pairs, through band-passes: the hi-hats
//!   and cymbals of the analogue machines. The 909's own hi-hats and
//!   cymbals were six-bit samples of real cymbals played from ROM; here
//!   they are this metal, made denser with noise and quantised to six bits
//!   before the envelope, as its ROM held them, so the grain of the
//!   quantisation falls with the level as on the original. That is a
//!   model of their character, not of the recordings.
//!
//! Then an optional saturator (the antiderivative-anti-aliased `tanh` of
//! [`SoftClip`]) and an optional low-pass (the CR-78's soft top).

use core::f64::consts::TAU;

use super::common::{Noise, SILENCE, SoftClip, flush, t60_factor};
use crate::{Oscillator, ProcessSpec, Processor, StateVariableFilter, SvfMode, Waveform};

/// The drums of the machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MachineKind {
    Kick909,
    Snare909,
    Clap909,
    Tom909,
    Rim909,
    ClosedHat909,
    OpenHat909,
    Crash909,
    Ride909,
    Kick606,
    Snare606,
    Tom606,
    ClosedHat606,
    OpenHat606,
    Cymbal606,
    KickCr78,
    SnareCr78,
    RimCr78,
    HatCr78,
    OpenHatCr78,
    CymbalCr78,
    CowbellCr78,
    GuiroCr78,
    MaracasCr78,
    TambourineCr78,
    BongoCr78,
    CongaCr78,
    ClavesCr78,
}

/// The settings a machine drum is struck with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MachineHit {
    /// Level, as a gain, with velocity and accent.
    pub(crate) amplitude: f64,
    /// Tuning, as a ratio.
    pub(crate) tune: f64,
    pub(crate) decay: f64,
    pub(crate) tone: f64,
    /// Snap with velocity and accent in it.
    pub(crate) snap: f64,
    pub(crate) texture: f64,
    pub(crate) accent: bool,
}

/// The six square waves of the 909's metal here, in hertz: those of the
/// 808 (K. J. Werner, J. S. Abel and J. O. Smith, "The TR-808 cymbal: a
/// physically-informed, circuit-bendable, digital model", ICMC 2014)
/// raised by about a fifth, for a brighter, tighter metal.
const METAL_909: [f64; 6] = [307.9, 456.6, 554.4, 784.1, 810.0, 1_200.0];

/// The 606's six squares, nearer the 808's.
const METAL_606: [f64; 6] = [236.1, 350.1, 425.0, 601.1, 621.0, 920.0];

/// The CR-78's cowbell pair and tambourine jingles, in hertz.
const METAL_CR78: [f64; 6] = [545.0, 815.0, 3_150.0, 4_270.0, 5_380.0, 6_610.0];

/// The waveform of a body.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Wave {
    /// A struck resonance: a sine.
    #[default]
    Sine,
    /// A triangle rounded by `tanh(k·x) / tanh(k)`: the 909's oscillators.
    Rounded(f64),
}

/// A tonal part.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct BodyRecipe {
    hertz: f64,
    wave: Wave,
    /// Two pitch envelopes: how far above `hertz` each starts, as a
    /// fraction, and how long it takes to fall 60 dB.
    sweep: [(f64, f64); 2],
    level: f64,
    /// How long it holds its level, then how long it takes to fall 60 dB.
    hold: f64,
    t60: f64,
}

/// A noise path.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct NoiseRecipe {
    level: f64,
    attack: f64,
    /// A fast part and a slow part of the envelope: levels and decays.
    spike: (f64, f64),
    tail: (f64, f64),
    /// Bursts before the tail: how many, how far apart (seconds, in
    /// turn), and each one's decay; the tail starts with the last.
    bursts: u32,
    spacing: [f64; 4],
    burst_t60: f64,
    /// Filters: zero is none.
    highpass: f64,
    lowpass: f64,
    band: (f64, f64),
}

/// The metal path.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct MetalRecipe {
    hertz: [f64; 6],
    /// Which of the six squares sound, as bits.
    mask: u8,
    /// How much of it is the squares ring-modulated in pairs, and how much
    /// white noise is mixed in, before the filters.
    ring: f64,
    noise: f64,
    /// Two bands with decays of their own, and a high-pass after them.
    band: (f64, f64, f64, f64),
    band2: (f64, f64, f64, f64),
    highpass: f64,
    attack: f64,
    /// How many bits it is quantised to before its envelope; zero is not.
    bits: u32,
}

/// A whole drum.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Recipe {
    bodies: [BodyRecipe; 3],
    click: (f64, f64, f64),
    noise: [NoiseRecipe; 2],
    metal: Option<MetalRecipe>,
    drive: f64,
    lowpass: f64,
    level: f64,
}

fn body(hertz: f64, wave: Wave, level: f64, t60: f64) -> BodyRecipe {
    BodyRecipe {
        hertz,
        wave,
        sweep: [(0.0, 0.01); 2],
        level,
        hold: 0.0,
        t60,
    }
}

fn noise(level: f64, t60: f64) -> NoiseRecipe {
    NoiseRecipe {
        level,
        attack: 0.0002,
        spike: (0.0, 0.01),
        tail: (1.0, t60),
        bursts: 0,
        spacing: [0.0; 4],
        burst_t60: 0.01,
        highpass: 0.0,
        lowpass: 0.0,
        band: (0.0, 1.0),
    }
}

/// The recipe of `kind`, set as `hit` says.
fn recipe(kind: MachineKind, hit: &MachineHit) -> Recipe {
    let (tune, decay, tone, snap, texture) = (hit.tune, hit.decay, hit.tone, hit.snap, hit.texture);
    let mut r = Recipe {
        level: 1.0,
        ..Recipe::default()
    };
    match kind {
        MachineKind::Kick909 => {
            // The oscillator's pitch falls fast from about three and a half
            // times its rest, then a little more slowly: TUNE sets how far.
            let mut kick = body(50.0 * tune, Wave::Rounded(1.6), 1.0, 0.55 * decay);
            kick.sweep = [(1.5 + 3.0 * snap, 0.035), (0.25 + 0.4 * snap, 0.25)];
            kick.hold = 0.004;
            r.bodies[0] = kick;
            // ATTACK: the click and a breath of noise.
            r.click = (0.25 + 0.75 * tone, 0.001, 4_500.0);
            r.noise[0] = NoiseRecipe {
                lowpass: 5_000.0,
                highpass: 800.0,
                ..noise(0.12 + 0.35 * tone, 0.006)
            };
            // The rumble of techno: noise rung through a narrow band at the
            // kick's pitch, swelling after it and saturated with it.
            if texture > 0.0 {
                r.noise[1] = NoiseRecipe {
                    attack: 0.03,
                    band: (55.0 * tune, 3.0),
                    lowpass: 180.0,
                    ..noise(6.0 * texture, 0.9 * decay)
                };
            }
            r.drive = 1.5;
            r.level = 0.38;
        }
        MachineKind::Snare909 => {
            // Two oscillators, the lower louder; TONE moves the balance to
            // the upper and brightens the noise; SNAPPY is the noise.
            let mut low = body(
                185.0 * tune,
                Wave::Rounded(1.2),
                1.0 - 0.45 * tone,
                0.14 * decay,
            );
            low.sweep = [(0.35, 0.03), (0.08, 0.1)];
            let mut high = body(
                335.0 * tune,
                Wave::Rounded(1.2),
                0.35 + 0.45 * tone,
                0.09 * decay,
            );
            high.sweep = [(0.3, 0.025), (0.0, 0.1)];
            r.bodies = [low, high, BodyRecipe::default()];
            r.noise[0] = NoiseRecipe {
                spike: (0.6, 0.04),
                tail: (0.6, 0.28 * decay),
                lowpass: 5_000.0 + 6_000.0 * tone,
                highpass: 900.0,
                ..noise(1.4 * snap, 0.28)
            };
            r.click = (0.2, 0.0008, 6_000.0);
            r.drive = 1.2;
            r.level = 0.35;
        }
        MachineKind::Clap909 => {
            // The 909 retriggers its clap envelope three times about ten
            // milliseconds apart before the tail.
            let centre = 900.0 * (1.0 + 0.7 * tone) * tune;
            r.noise[0] = NoiseRecipe {
                spike: (0.0, 0.01),
                tail: (0.45 + 0.5 * (1.0 - snap), 0.26 * decay),
                bursts: 4,
                spacing: [0.0, 0.0085, 0.018, 0.0285],
                burst_t60: 0.016,
                band: (centre, 1.6),
                highpass: 450.0,
                ..noise(2.0 * (0.6 + 0.8 * snap), 0.26)
            };
            r.level = 0.28;
        }
        MachineKind::Tom909 => {
            let hertz = 130.0 * tune;
            let ring = (130.0 / hertz).clamp(0.5, 2.0).sqrt();
            let mut tom = body(hertz, Wave::Rounded(1.3), 1.0, 0.42 * decay * ring);
            tom.sweep = [(0.35 + 0.5 * snap, 0.12), (0.1, 0.4)];
            r.bodies[0] = tom;
            r.noise[0] = NoiseRecipe {
                lowpass: 3_000.0,
                highpass: 300.0,
                ..noise(0.25 * (0.5 + tone), 0.06)
            };
            r.click = (0.15, 0.0008, 5_000.0);
            r.drive = 1.2;
            r.level = 0.36;
        }
        MachineKind::Rim909 => {
            r.bodies = [
                body(220.0 * tune, Wave::Sine, 0.5, 0.03 * decay),
                body(500.0 * tune, Wave::Sine, 0.8, 0.035 * decay),
                body(1_700.0 * tune, Wave::Sine, 0.6 + 0.4 * tone, 0.025 * decay),
            ];
            r.click = (0.6, 0.0006, 7_000.0);
            r.noise[0] = NoiseRecipe {
                highpass: 2_000.0,
                ..noise(0.3, 0.01)
            };
            r.drive = 2.0 + 3.0 * snap;
            r.level = 0.28;
        }
        MachineKind::ClosedHat909 | MachineKind::OpenHat909 => {
            let open = kind == MachineKind::OpenHat909;
            let t60 = if open { 0.7 } else { 0.09 } * decay;
            r.metal = Some(MetalRecipe {
                hertz: METAL_909.map(|f| f * tune),
                mask: 0b11_1111,
                ring: 0.5,
                noise: 0.35,
                band: (8_500.0 * (1.0 + 0.3 * tone), 0.9, 1.0, t60),
                band2: (11_500.0, 1.2, 0.5 + 0.5 * snap, 0.7 * t60),
                highpass: 6_500.0,
                attack: 0.0001,
                bits: 6,
            });
            r.level = 0.35;
        }
        MachineKind::Crash909 => {
            r.metal = Some(MetalRecipe {
                hertz: METAL_909.map(|f| f * 0.9 * tune),
                mask: 0b11_1111,
                ring: 0.7,
                noise: 0.5,
                band: (5_500.0 * (1.0 + 0.3 * tone), 0.6, 1.0, 1.7 * decay),
                band2: (9_500.0, 0.9, 0.7 + 0.3 * snap, 1.1 * decay),
                highpass: 2_800.0,
                attack: 0.0004,
                bits: 6,
            });
            r.level = 0.36;
        }
        MachineKind::Ride909 => {
            r.metal = Some(MetalRecipe {
                hertz: METAL_909.map(|f| f * 1.1 * tune),
                mask: 0b11_1111,
                ring: 0.6,
                noise: 0.2,
                band: (4_300.0 * (1.0 + 0.3 * tone), 3.0, 0.8, 1.6 * decay),
                band2: (8_000.0, 1.0, 0.6 + 0.4 * snap, 2.2 * decay),
                highpass: 3_500.0,
                attack: 0.0001,
                bits: 6,
            });
            r.level = 0.575;
        }
        MachineKind::Kick606 => {
            let mut kick = body(62.0 * tune, Wave::Sine, 1.0, 0.2 * decay);
            kick.sweep = [(0.4 + 0.8 * snap, 0.015), (0.05, 0.08)];
            r.bodies[0] = kick;
            r.click = (0.4 + 0.5 * tone, 0.0008, 4_500.0);
            r.drive = 1.1;
            r.level = 0.42;
        }
        MachineKind::Snare606 => {
            let mut low = body(238.0 * tune, Wave::Sine, 0.9 - 0.3 * tone, 0.07 * decay);
            low.sweep = [(0.15, 0.02), (0.0, 0.1)];
            let high = body(476.0 * tune, Wave::Sine, 0.2 + 0.3 * tone, 0.045 * decay);
            r.bodies = [low, high, BodyRecipe::default()];
            r.noise[0] = NoiseRecipe {
                spike: (0.4, 0.03),
                tail: (0.7, 0.14 * decay),
                highpass: 1_600.0,
                lowpass: 10_000.0,
                ..noise(1.3 * snap, 0.14)
            };
            r.click = (0.25, 0.0006, 6_000.0);
            r.level = 0.26;
        }
        MachineKind::Tom606 => {
            let hertz = 135.0 * tune;
            let mut tom = body(hertz, Wave::Sine, 1.0, 0.24 * decay);
            tom.sweep = [(0.12 + 0.2 * snap, 0.06), (0.0, 0.1)];
            r.bodies[0] = tom;
            r.noise[0] = NoiseRecipe {
                lowpass: 2_500.0,
                ..noise(0.1 * (0.5 + tone), 0.03)
            };
            r.click = (0.15, 0.0008, 4_000.0);
            r.level = 0.37;
        }
        MachineKind::ClosedHat606 | MachineKind::OpenHat606 => {
            let open = kind == MachineKind::OpenHat606;
            let t60 = if open { 0.38 } else { 0.05 } * decay;
            r.metal = Some(MetalRecipe {
                hertz: METAL_606.map(|f| f * tune),
                mask: 0b11_1111,
                ring: 0.0,
                noise: 0.0,
                band: (8_000.0 * (1.0 + 0.3 * tone), 1.2, 1.0, t60),
                band2: (0.0, 1.0, 0.0, t60),
                highpass: 7_500.0,
                attack: 0.0001,
                bits: 0,
            });
            r.level = 1.0;
        }
        MachineKind::Cymbal606 => {
            r.metal = Some(MetalRecipe {
                hertz: METAL_606.map(|f| f * tune),
                mask: 0b11_1111,
                ring: 0.0,
                noise: 0.0,
                band: (3_500.0, 1.4, 1.0 - 0.5 * tone, 1.0 * decay),
                band2: (7_500.0, 1.4, 0.4 + 0.6 * tone, 0.55 * decay),
                highpass: 2_500.0,
                attack: 0.0002,
                bits: 0,
            });
            r.level = 1.1;
        }
        MachineKind::KickCr78 => {
            let mut kick = body(58.0 * tune, Wave::Sine, 1.0, 0.32 * decay);
            kick.sweep = [(0.08 + 0.2 * snap, 0.02), (0.0, 0.1)];
            r.bodies[0] = kick;
            r.click = (0.1 + 0.2 * tone, 0.0012, 1_500.0);
            r.lowpass = 2_500.0;
            r.level = 0.43;
        }
        MachineKind::SnareCr78 => {
            r.bodies[0] = body(210.0 * tune, Wave::Sine, 0.9, 0.08 * decay);
            r.bodies[1] = body(395.0 * tune, Wave::Sine, 0.25 * (0.5 + tone), 0.05 * decay);
            r.noise[0] = NoiseRecipe {
                attack: 0.0008,
                tail: (1.0, 0.14 * decay),
                highpass: 1_000.0,
                lowpass: 4_500.0 + 3_000.0 * tone,
                ..noise(0.9 * snap, 0.14)
            };
            r.click = (0.1, 0.0008, 3_000.0);
            r.lowpass = 9_000.0;
            r.level = 0.33;
        }
        MachineKind::RimCr78 => {
            r.bodies[0] = body(1_150.0 * tune, Wave::Sine, 1.0, 0.025 * decay);
            r.bodies[1] = body(2_300.0 * tune, Wave::Sine, 0.4 + 0.3 * tone, 0.018 * decay);
            r.click = (0.2, 0.0006, 5_000.0);
            r.drive = 1.5;
            r.lowpass = 9_000.0;
            r.level = 0.26;
        }
        MachineKind::HatCr78 | MachineKind::OpenHatCr78 => {
            let open = kind == MachineKind::OpenHatCr78;
            r.noise[0] = NoiseRecipe {
                attack: 0.0005,
                tail: (1.0, if open { 0.32 } else { 0.07 } * decay),
                highpass: 7_000.0,
                band: (9_000.0 * (1.0 + 0.2 * tone), 0.8),
                ..noise(2.2, 0.07)
            };
            r.lowpass = 14_000.0;
            r.level = 0.18;
        }
        MachineKind::CymbalCr78 => {
            r.noise[0] = NoiseRecipe {
                attack: 0.001,
                tail: (1.0, 1.3 * decay),
                highpass: 3_500.0,
                band: (6_000.0 * (1.0 + 0.3 * tone), 0.7),
                ..noise(2.0, 1.3)
            };
            r.lowpass = 13_000.0;
            r.level = 0.2;
        }
        MachineKind::CowbellCr78 => {
            r.metal = Some(MetalRecipe {
                hertz: METAL_CR78.map(|f| f * tune),
                mask: 0b00_0011,
                ring: 0.0,
                noise: 0.0,
                band: (
                    1_400.0 * (1.0 + 0.3 * tone),
                    1.2,
                    0.6 + 0.6 * snap,
                    0.05 * decay,
                ),
                band2: (1_400.0 * (1.0 + 0.3 * tone), 1.2, 0.4, 0.25 * decay),
                highpass: 0.0,
                attack: 0.0003,
                bits: 0,
            });
            r.lowpass = 7_000.0;
            r.level = 0.33;
        }
        MachineKind::GuiroCr78 => {
            // A scrape: a train of short bursts through a band, longer with
            // the texture.
            let bursts = if texture >= 0.5 { 4 } else { 3 };
            let gap = 0.022 / tune;
            r.noise[0] = NoiseRecipe {
                attack: 0.002,
                tail: (0.5, (0.06 + 0.1 * texture) * decay),
                bursts,
                spacing: [0.0, gap, 2.0 * gap, 3.0 * gap],
                burst_t60: 0.025,
                band: (3_000.0 * (1.0 + 0.3 * tone), 2.0),
                highpass: 1_200.0,
                ..noise(2.0, 0.1)
            };
            r.lowpass = 9_000.0;
            r.level = 0.22;
        }
        MachineKind::MaracasCr78 => {
            r.noise[0] = NoiseRecipe {
                attack: 0.003,
                tail: (1.0, 0.05 * decay),
                highpass: 5_000.0,
                ..noise(1.2, 0.05)
            };
            r.lowpass = 12_000.0;
            r.level = 0.29;
        }
        MachineKind::TambourineCr78 => {
            r.noise[0] = NoiseRecipe {
                attack: 0.001,
                tail: (0.7, 0.25 * decay),
                bursts: 3,
                spacing: [0.0, 0.012, 0.026, 0.0],
                burst_t60: 0.03,
                band: (7_500.0 * (1.0 + 0.2 * tone), 1.5),
                highpass: 4_000.0,
                ..noise(1.6, 0.25)
            };
            r.metal = Some(MetalRecipe {
                hertz: METAL_CR78.map(|f| f * tune),
                mask: 0b11_1100,
                ring: 0.0,
                noise: 0.0,
                band: (6_500.0, 1.0, 0.25, 0.2 * decay),
                band2: (0.0, 1.0, 0.0, 0.2),
                highpass: 3_000.0,
                attack: 0.001,
                bits: 0,
            });
            r.lowpass = 12_000.0;
            r.level = 0.18;
        }
        MachineKind::BongoCr78 => {
            let mut bongo = body(420.0 * tune, Wave::Sine, 1.0, 0.12 * decay);
            bongo.sweep = [(0.08 + 0.1 * snap, 0.015), (0.0, 0.1)];
            r.bodies[0] = bongo;
            r.click = (0.1, 0.0006, 3_000.0);
            r.lowpass = 9_000.0;
            r.level = 0.27;
        }
        MachineKind::CongaCr78 => {
            let mut conga = body(210.0 * tune, Wave::Sine, 1.0, 0.22 * decay);
            conga.sweep = [(0.1 + 0.1 * snap, 0.02), (0.0, 0.1)];
            r.bodies[0] = conga;
            r.click = (0.1, 0.0006, 3_000.0);
            r.lowpass = 9_000.0;
            r.level = 0.32;
        }
        MachineKind::ClavesCr78 => {
            r.bodies[0] = body(2_500.0 * tune, Wave::Sine, 1.0, 0.05 * decay);
            r.lowpass = 10_000.0;
            r.level = 0.26;
        }
    }
    r
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

/// A filter that may be switched off.
#[derive(Debug, Clone)]
struct Stage {
    on: bool,
    filter: StateVariableFilter<f64>,
}

impl Stage {
    fn new(mode: SvfMode, rate: f64) -> Self {
        Self {
            on: false,
            filter: filter(mode, 1_000.0, 0.707, rate),
        }
    }

    fn set(&mut self, hertz: f64, q: f64, rate: f64) {
        self.on = hertz > 0.0;
        if self.on {
            self.filter.set_frequency(hertz.clamp(10.0, 0.45 * rate));
            self.filter.set_q(q);
        }
    }

    #[inline]
    fn next(&mut self, x: f64) -> f64 {
        if self.on {
            self.filter.process_sample(0, x)
        } else {
            x
        }
    }

    fn reset(&mut self) {
        Processor::<f64>::reset(&mut self.filter);
    }
}

/// A body sounding.
#[derive(Debug, Clone, Copy, Default)]
struct BodyState {
    recipe: BodyRecipe,
    phase: f64,
    sweep: [f64; 2],
    sweep_fall: [f64; 2],
    level: f64,
    hold: u32,
    fall: f64,
}

impl BodyState {
    fn start(&mut self, recipe: BodyRecipe, rate: f64) {
        *self = Self {
            recipe,
            phase: 0.0,
            sweep: [recipe.sweep[0].0, recipe.sweep[1].0],
            sweep_fall: [
                t60_factor(recipe.sweep[0].1, rate),
                t60_factor(recipe.sweep[1].1, rate),
            ],
            level: recipe.level,
            hold: (recipe.hold * rate) as u32,
            fall: t60_factor(recipe.t60, rate),
        };
    }

    #[inline]
    fn next(&mut self, rate: f64) -> f64 {
        if self.level == 0.0 {
            return 0.0;
        }
        let x = match self.recipe.wave {
            Wave::Sine => (TAU * self.phase).sin(),
            Wave::Rounded(k) => {
                // A triangle from 0 rising, rounded.
                let t = 4.0 * (self.phase - (self.phase + 0.25).floor() + 0.25 - 0.5).abs() - 1.0;
                (k * -t).tanh() / k.tanh()
            }
        };
        let out = self.level * x;
        let hertz = self.recipe.hertz * (1.0 + self.sweep[0] + self.sweep[1]);
        self.phase += (hertz / rate).min(0.5);
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        self.sweep[0] = flush(self.sweep[0] * self.sweep_fall[0]);
        self.sweep[1] = flush(self.sweep[1] * self.sweep_fall[1]);
        if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.level = flush(self.level * self.fall);
        }
        out
    }
}

/// A noise path sounding.
#[derive(Debug, Clone)]
struct NoiseState {
    recipe: NoiseRecipe,
    spike: f64,
    tail: f64,
    rise: f64,
    spike_fall: f64,
    tail_fall: f64,
    rise_fall: f64,
    burst: f64,
    burst_fall: f64,
    bursts_left: u32,
    burst_index: usize,
    timer: u32,
    highpass: Stage,
    lowpass: Stage,
    band: Stage,
}

impl NoiseState {
    fn new(rate: f64) -> Self {
        Self {
            recipe: NoiseRecipe::default(),
            spike: 0.0,
            tail: 0.0,
            rise: 0.0,
            spike_fall: 0.0,
            tail_fall: 0.0,
            rise_fall: 0.0,
            burst: 0.0,
            burst_fall: 0.0,
            bursts_left: 0,
            burst_index: 0,
            timer: 0,
            highpass: Stage::new(SvfMode::Highpass, rate),
            lowpass: Stage::new(SvfMode::Lowpass, rate),
            band: Stage::new(SvfMode::Bandpass, rate),
        }
    }

    fn start(&mut self, recipe: NoiseRecipe, rate: f64) {
        self.recipe = recipe;
        self.rise_fall = t60_factor(recipe.attack, rate);
        self.spike_fall = t60_factor(recipe.spike.1, rate);
        self.tail_fall = t60_factor(recipe.tail.1, rate);
        self.burst_fall = t60_factor(recipe.burst_t60, rate);
        self.burst = 0.0;
        self.bursts_left = recipe.bursts.min(4);
        self.burst_index = 0;
        self.timer = 0;
        if self.bursts_left == 0 {
            self.spike = recipe.spike.0;
            self.tail = recipe.tail.0;
            self.rise = recipe.spike.0 + recipe.tail.0;
        } else {
            self.spike = 0.0;
            self.tail = 0.0;
            self.rise = 0.0;
        }
        self.highpass.set(recipe.highpass, 0.707, rate);
        self.lowpass.set(recipe.lowpass, 0.707, rate);
        self.band.set(recipe.band.0, recipe.band.1, rate);
    }

    fn level(&self) -> f64 {
        self.spike.abs()
            + self.tail.abs()
            + self.burst.abs()
            + if self.bursts_left > 0 { 1.0 } else { 0.0 }
    }

    #[inline]
    fn next(&mut self, white: f64, rate: f64) -> f64 {
        if self.recipe.level == 0.0 {
            return 0.0;
        }
        if self.bursts_left == 0
            && self.spike == 0.0
            && self.tail == 0.0
            && self.burst == 0.0
            && self.rise == 0.0
        {
            // Over: the filters are cleared rather than left to ring down
            // into subnormal numbers, which are slow.
            self.reset();
            return 0.0;
        }
        if self.bursts_left > 0 {
            if self.timer == 0 {
                // Each burst rises in the attack time, as the first does.
                self.burst = 1.0;
                self.rise += 1.0;
                self.bursts_left -= 1;
                self.burst_index += 1;
                if self.bursts_left == 0 {
                    // The last burst sets the tail going.
                    self.tail = self.recipe.tail.0;
                    self.spike = self.recipe.spike.0;
                    self.rise += self.recipe.tail.0 + self.recipe.spike.0;
                } else {
                    let gap = self.recipe.spacing[self.burst_index.min(3)]
                        - self.recipe.spacing[self.burst_index - 1];
                    self.timer = (gap.max(0.0) * rate).max(1.0) as u32;
                }
            }
            self.timer = self.timer.saturating_sub(1);
        }
        let envelope = self.spike + self.tail + self.burst - self.rise;
        self.spike = flush(self.spike * self.spike_fall);
        self.tail = flush(self.tail * self.tail_fall);
        self.rise = flush(self.rise * self.rise_fall);
        self.burst = flush(self.burst * self.burst_fall);
        let x = white * envelope * self.recipe.level;
        let x = self.highpass.next(x);
        let x = self.lowpass.next(x);
        self.band.next(x)
    }

    fn reset(&mut self) {
        self.recipe.level = 0.0;
        self.spike = 0.0;
        self.tail = 0.0;
        self.rise = 0.0;
        self.burst = 0.0;
        self.bursts_left = 0;
        self.highpass.reset();
        self.lowpass.reset();
        self.band.reset();
    }
}

/// The metal path sounding.
#[derive(Debug, Clone)]
struct MetalState {
    recipe: MetalRecipe,
    on: bool,
    squares: [Oscillator; 6],
    level: [f64; 2],
    fall: [f64; 2],
    rise: f64,
    rise_fall: f64,
    band: Stage,
    band2: Stage,
    highpass: Stage,
}

impl MetalState {
    fn new(rate: f64) -> Self {
        let mut squares = [0.0; 6].map(|_| Oscillator::new(Waveform::Square, 400.0));
        for square in &mut squares {
            square.set_sample_rate(rate);
        }
        Self {
            recipe: MetalRecipe::default(),
            on: false,
            squares,
            level: [0.0; 2],
            fall: [0.0; 2],
            rise: 0.0,
            rise_fall: 0.0,
            band: Stage::new(SvfMode::Bandpass, rate),
            band2: Stage::new(SvfMode::Bandpass, rate),
            highpass: Stage::new(SvfMode::Highpass, rate),
        }
    }

    fn start(&mut self, recipe: MetalRecipe, rate: f64) {
        self.recipe = recipe;
        self.on = true;
        for (index, (square, hertz)) in self.squares.iter_mut().zip(recipe.hertz).enumerate() {
            square.set_frequency(hertz.min(0.45 * rate));
            // The oscillators of the circuit run freely, out of step with
            // each other: started in step they would rise as one step.
            // Each starts a few samples into its turn, the same at every
            // strike.
            square.reset_phase();
            for _ in 0..(5 + 7 * index) {
                square.next_sample();
            }
        }
        self.level = [recipe.band.2, recipe.band2.2];
        self.fall = [
            t60_factor(recipe.band.3, rate),
            t60_factor(recipe.band2.3, rate),
        ];
        self.rise = recipe.band.2 + recipe.band2.2;
        self.rise_fall = t60_factor(recipe.attack, rate);
        self.band.set(recipe.band.0, recipe.band.1, rate);
        self.band2.set(recipe.band2.0, recipe.band2.1, rate);
        self.highpass.set(recipe.highpass, 0.707, rate);
    }

    fn level(&self) -> f64 {
        if self.on {
            self.level[0].abs() + self.level[1].abs()
        } else {
            0.0
        }
    }

    #[inline]
    fn next(&mut self, white: f64) -> f64 {
        if !self.on {
            return 0.0;
        }
        if self.level == [0.0; 2] {
            // Over; see the noise path.
            self.reset();
            return 0.0;
        }
        let recipe = &self.recipe;
        let mut squares = [0.0; 6];
        let mut sum = 0.0;
        let mut count = 0.0;
        for (bit, (value, square)) in squares.iter_mut().zip(&mut self.squares).enumerate() {
            if recipe.mask & (1 << bit) != 0 {
                *value = square.next_sample();
                sum += *value;
                count += 1.0;
            }
        }
        let mut metal = sum / f64::max(count, 1.0);
        if recipe.ring > 0.0 {
            let ring =
                (squares[0] * squares[1] + squares[2] * squares[3] + squares[4] * squares[5]) / 3.0;
            metal = (1.0 - recipe.ring) * metal + recipe.ring * ring;
        }
        metal += recipe.noise * white;
        let rise = self.rise;
        let first = self.band.next(metal);
        let second = if self.band2.on {
            self.band2.next(metal)
        } else {
            0.0
        };
        let mut shaped = [first, second];
        if recipe.bits > 0 {
            // Quantised to the ROM's word, scaled to its full range first.
            let steps = f64::from((1u32 << (recipe.bits - 1)) - 1);
            for value in &mut shaped {
                *value = ((*value * 2.5).clamp(-1.0, 1.0) * steps).round() / steps / 2.5;
            }
        }
        let out = shaped[0] * self.level[0] + shaped[1] * self.level[1];
        let out = out * (1.0 - rise / (self.recipe.band.2 + self.recipe.band2.2).max(1e-12));
        self.level[0] = flush(self.level[0] * self.fall[0]);
        self.level[1] = flush(self.level[1] * self.fall[1]);
        self.rise = flush(self.rise * self.rise_fall);
        self.highpass.next(out)
    }

    fn reset(&mut self) {
        self.on = false;
        self.level = [0.0; 2];
        self.band.reset();
        self.band2.reset();
        self.highpass.reset();
    }
}

/// One drum of a machine, sounding.
#[derive(Debug, Clone)]
pub(crate) struct Machine {
    rate: f64,
    bodies: [BodyState; 3],
    click_left: u32,
    click_len: u32,
    click_level: f64,
    click_pole: f64,
    click_low: f64,
    noise: [NoiseState; 2],
    metal: MetalState,
    white: Noise,
    clip: SoftClip,
    drive: f64,
    lowpass: Stage,
    level: f64,
    alive: bool,
}

impl Machine {
    /// A drum at rest. Allocates.
    pub(crate) fn new(rate: f64) -> Self {
        Self {
            rate,
            bodies: [BodyState::default(); 3],
            click_left: 0,
            click_len: 1,
            click_level: 0.0,
            click_pole: 0.0,
            click_low: 0.0,
            noise: [NoiseState::new(rate), NoiseState::new(rate)],
            metal: MetalState::new(rate),
            white: Noise::for_note(0, 0.0, 41),
            clip: SoftClip::default(),
            drive: 0.0,
            lowpass: Stage::new(SvfMode::Lowpass, rate),
            level: 0.0,
            alive: false,
        }
    }

    /// Strikes `kind` from silence.
    pub(crate) fn strike(&mut self, kind: MachineKind, hit: &MachineHit, seed: u64) {
        self.silence();
        let rate = self.rate;
        let recipe = recipe(kind, hit);
        for (state, body) in self.bodies.iter_mut().zip(recipe.bodies) {
            if body.hertz > 0.0 && body.level > 0.0 {
                state.start(body, rate);
            }
        }
        let (click, seconds, brightness) = recipe.click;
        let accent = if hit.accent { 1.4 } else { 1.0 };
        self.click_level = click * accent;
        self.click_len = (seconds * rate).max(1.0) as u32;
        self.click_left = if click > 0.0 { self.click_len } else { 0 };
        self.click_pole = (-TAU * brightness.min(0.45 * rate) / rate).exp();
        for (state, path) in self.noise.iter_mut().zip(recipe.noise) {
            if path.level > 0.0 {
                state.start(path, rate);
            }
        }
        if let Some(metal) = recipe.metal {
            self.metal.start(metal, rate);
        }
        self.drive = recipe.drive;
        self.lowpass.set(recipe.lowpass, 0.707, rate);
        self.level = recipe.level * hit.amplitude;
        self.white = Noise::for_note(0, 0.0, seed);
        self.alive = true;
    }

    /// One sample.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        let rate = self.rate;
        let mut out = 0.0;
        let mut loudest = 0.0f64;
        for body in &mut self.bodies {
            loudest = loudest.max(body.level);
            out += body.next(rate);
        }
        let mut click = 0.0;
        if self.click_left > 0 {
            let t = 1.0 - f64::from(self.click_left) / f64::from(self.click_len);
            click = self.click_level * (core::f64::consts::PI * t).sin();
            self.click_left -= 1;
        }
        self.click_low = flush(click + self.click_pole * (self.click_low - click));
        loudest = loudest.max(self.click_low.abs()).max(click);
        out += self.click_low;
        let white = self.white.next();
        for path in &mut self.noise {
            loudest = loudest.max(path.level());
            out += path.next(white, rate);
        }
        loudest = loudest.max(self.metal.level());
        out += self.metal.next(white);
        if self.drive > 0.0 {
            out = self.clip.next(self.drive * out) / self.drive.tanh();
        }
        let out = self.lowpass.next(out);
        if loudest < SILENCE {
            self.alive = false;
        }
        out * self.level
    }

    pub(crate) fn is_sounding(&self) -> bool {
        self.alive
    }

    pub(crate) fn silence(&mut self) {
        self.alive = false;
        self.bodies = [BodyState::default(); 3];
        self.click_left = 0;
        self.click_low = 0.0;
        for path in &mut self.noise {
            path.reset();
        }
        self.metal.reset();
        self.clip.reset();
        self.lowpass.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments::measure::{
        RATE, between, cents, frame_levels, frequency_near, time_to_fall,
    };

    fn hit(tune: f64) -> MachineHit {
        MachineHit {
            amplitude: 1.0,
            tune,
            decay: 1.0,
            tone: 0.5,
            snap: 0.5,
            texture: 0.0,
            accent: false,
        }
    }

    fn sound(kind: MachineKind, hit: &MachineHit, seconds: f64) -> Vec<f64> {
        let mut machine = Machine::new(RATE);
        machine.strike(kind, hit, 1);
        (0..(seconds * RATE) as usize)
            .map(|_| machine.next())
            .collect()
    }

    #[test]
    fn the_909_kick_sweeps_down_to_its_pitch() {
        for semitones in [0.0, 7.0] {
            let tune = (semitones / 12.0f64).exp2();
            let signal = sound(MachineKind::Kick909, &hit(tune), 0.6);
            let expected = 50.0 * tune;
            let settled = frequency_near(between(&signal, 0.25, 0.55), expected);
            let early = frequency_near(between(&signal, 0.002, 0.012), 3.0 * expected);
            println!("{semitones}: {early:.0} Hz falling to {settled:.2} Hz");
            assert!(cents(settled, expected).abs() < 5.0, "{settled}");
            assert!(early > 2.0 * expected, "{early}");
        }
    }

    #[test]
    fn the_909_clap_is_bursts_and_a_tail() {
        // The loudest millisecond of each of its four bursts falls where
        // its retriggered envelope sets them.
        let signal = sound(MachineKind::Clap909, &hit(1.0), 0.3);
        let levels = frame_levels(&signal, 0.001);
        for at in [0.0, 0.0085, 0.018, 0.0285] {
            let from = (at * 1_000.0) as usize;
            let window = &levels[from..from + 6];
            let before = if from >= 2 {
                levels[from - 2]
            } else {
                f64::NEG_INFINITY
            };
            let loudest = window.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert!(
                loudest > before + 3.0,
                "a burst at {at}: {loudest} after {before}"
            );
        }
        // And the tail rings on after the last.
        assert!(levels[100] > levels[40] - 30.0);
        assert!(time_to_fall(&signal, 40.0) > 0.1);
    }

    #[test]
    fn open_hi_hats_ring_longer_than_closed_ones() {
        for (closed, open) in [
            (MachineKind::ClosedHat909, MachineKind::OpenHat909),
            (MachineKind::ClosedHat606, MachineKind::OpenHat606),
            (MachineKind::HatCr78, MachineKind::OpenHatCr78),
        ] {
            let short = time_to_fall(&sound(closed, &hit(1.0), 1.5), 40.0);
            let long = time_to_fall(&sound(open, &hit(1.0), 1.5), 40.0);
            println!("{closed:?} {short:.2} s, {open:?} {long:.2} s");
            assert!(long > 3.0 * short, "{open:?}: {long} against {short}");
        }
    }

    #[test]
    fn the_long_guiro_scrapes_longer() {
        let short = MachineHit {
            texture: 0.0,
            ..hit(1.0)
        };
        let long = MachineHit {
            texture: 1.0,
            ..hit(1.0)
        };
        let short = time_to_fall(&sound(MachineKind::GuiroCr78, &short, 0.6), 40.0);
        let long = time_to_fall(&sound(MachineKind::GuiroCr78, &long, 0.6), 40.0);
        assert!(long > 1.3 * short, "{long} against {short}");
    }

    #[test]
    fn a_strike_is_the_same_every_time_and_ends_silent() {
        let kinds = [
            MachineKind::Kick909,
            MachineKind::Snare606,
            MachineKind::CowbellCr78,
            MachineKind::TambourineCr78,
            MachineKind::Ride909,
        ];
        for kind in kinds {
            let mut machine = Machine::new(RATE);
            let mut takes = Vec::new();
            for _ in 0..2 {
                machine.strike(kind, &hit(1.0), 7);
                let take: Vec<f64> = (0..(4.5 * RATE) as usize).map(|_| machine.next()).collect();
                takes.push(take);
            }
            assert!(takes[0] == takes[1], "{kind:?}");
            assert!(!machine.is_sounding(), "{kind:?}");
        }
    }
}

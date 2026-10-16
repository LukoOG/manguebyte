//! The master bus: the parts and the shared reverb's return, the glue
//! compressor, the master gain, and a true-peak-aware limiter that keeps
//! the output under its ceiling.

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::{Compressor, Limiter, ProcessSpec, Processor, Reverb, fir};
use manguebyte_params::{Param, ParamId, ParamSet, ParamSmoother};

use crate::chain::SleepyReverb;
use crate::engine::OUTPUT_CEILING;
use crate::fx::{Clock, Squeeze, Switch, gain_from_db};
use crate::params::{self, LEVEL_SMOOTHING, MasterParams};

/// How far the master limiter looks ahead: the time it has to bring the
/// gain down before a peak, so that it does so in a ramp rather than a
/// step. With [`TRUE_PEAK_DELAY`] it is the latency the master bus adds,
/// [`master_latency_frames`].
pub const MASTER_LOOKAHEAD_SECONDS: f64 = 0.002;

/// How many points the key reads for each sample: the sample and seven
/// between it and the next. Eight times oversampled, a reading is low by
/// at most `20 log10(cos(π f / (8 fs)))`: 0.04 dB at a quarter of the
/// sample rate, 0.17 dB at half (ITU-R BS.1770-4, Annex 2, Appendix 1).
const TRUE_PEAK_OVERSAMPLING: usize = 8;

/// Taps of each phase of the filter that reads the waveform between
/// samples. With 32 the filter passes everything up to 0.44 of the sample
/// rate, 21 kHz at 48 kHz, at its true level.
const TRUE_PEAK_TAPS: usize = 32;

/// Frames the master bus delays the audio by for the reading between
/// samples, so that the filter sees as many samples after the point it
/// reads as before it: half its taps. Then the limiter looks ahead.
pub const TRUE_PEAK_DELAY: usize = TRUE_PEAK_TAPS / 2;

/// The points between two samples the key reads.
const TRUE_PEAK_PHASES: usize = TRUE_PEAK_OVERSAMPLING - 1;

/// How long the master bus has to hear nothing at all, beyond its
/// latency, before it comes to rest and is skipped until sound comes back.
const MASTER_REST_SECONDS: f64 = 0.2;

/// Stopband of the filter that reads between samples, in decibels.
const TRUE_PEAK_STOPBAND_DB: f64 = 60.0;

/// A constant fed to the limiter beside the audio, too small ever to make
/// it act: what comes out is the limiter's gain times this, exactly, as it
/// is a power of two.
const PROBE: f64 = 1.0 / 1_024.0;

/// The frames of the limiter's look-ahead window at `sample_rate`, as
/// [`Limiter`] works them out.
fn lookahead_window(sample_rate: f64) -> usize {
    ((MASTER_LOOKAHEAD_SECONDS * sample_rate).round() as usize).max(1)
}

/// The frames the master bus delays the sound by at `sample_rate`: the
/// reading between samples ([`TRUE_PEAK_DELAY`]) and the limiter's
/// look-ahead ([`MASTER_LOOKAHEAD_SECONDS`]), less the frame the limiter's
/// window shares with the present. 111 frames at 48 kHz, 103 at 44.1 kHz.
///
/// ```
/// use manguebyte_web::master_latency_frames;
///
/// assert_eq!(master_latency_frames(48_000.0), 111);
/// assert_eq!(master_latency_frames(44_100.0), 103);
/// ```
pub fn master_latency_frames(sample_rate: f64) -> usize {
    TRUE_PEAK_DELAY + lookahead_window(sample_rate) - 1
}

/// Reads the waveform between samples, seven points between each pair, by
/// interpolating by eight with a windowed-sinc filter in polyphase form, as
/// ITU-R BS.1770-4 Annex 2 measures true peak; the filter is
/// `manguebyte-dsp`'s [`fir::lowpass`].
pub(crate) struct TruePeakKey {
    /// The phases between two samples, each oldest input first.
    phases: [[f64; TRUE_PEAK_TAPS]; TRUE_PEAK_PHASES],
    /// The last inputs of each side, written twice so that they are always
    /// contiguous.
    history: [[f64; 2 * TRUE_PEAK_TAPS]; 2],
    position: usize,
    /// A peak follower over both sides, falling by [`ENVELOPE_FALL`] a
    /// frame: never less than the largest input of the window times
    /// `ENVELOPE_FALL` to the power of the window's length.
    envelope: f64,
    /// The most a point between samples can be, as a multiple of the
    /// envelope: the largest sum of the magnitudes of a phase's taps, over
    /// the envelope's fall across the window.
    bound: f64,
}

/// How much [`TruePeakKey`]'s envelope keeps from one frame to the next.
const ENVELOPE_FALL: f64 = 0.99;

impl TruePeakKey {
    pub(crate) fn new() -> Self {
        // An odd length whose delay, half of it, is a whole number of input
        // samples, so that phase 0 is the input itself; its transition is
        // centred on half the input's rate.
        let factor = TRUE_PEAK_OVERSAMPLING;
        let taps = factor * TRUE_PEAK_TAPS + 1;
        let filter = fir::lowpass(taps, 0.5 / factor as f64, TRUE_PEAK_STOPBAND_DB);
        let mut phases = [[0.0; TRUE_PEAK_TAPS]; TRUE_PEAK_PHASES];
        for (k, phase) in phases.iter_mut().enumerate() {
            for (j, tap) in phase.iter_mut().enumerate() {
                // Input `TRUE_PEAK_TAPS - 1 - j` frames old; the filter is
                // scaled by the factor, for the zeros interpolation puts
                // between the samples.
                *tap = factor as f64 * filter[k + 1 + factor * (TRUE_PEAK_TAPS - 1 - j)];
            }
        }
        let gain = phases
            .iter()
            .map(|phase| phase.iter().map(|tap| tap.abs()).sum::<f64>())
            .fold(1.0, f64::max);
        Self {
            phases,
            history: [[0.0; 2 * TRUE_PEAK_TAPS]; 2],
            position: 0,
            envelope: 0.0,
            bound: gain / ENVELOPE_FALL.powi(TRUE_PEAK_TAPS as i32),
        }
    }

    /// Forgets the inputs.
    pub(crate) fn clear(&mut self) {
        self.history = [[0.0; 2 * TRUE_PEAK_TAPS]; 2];
        self.position = 0;
        self.envelope = 0.0;
    }

    /// Takes the next frame and returns the frame [`TRUE_PEAK_DELAY`]
    /// frames old with the largest magnitude of the waveform from it to the
    /// frame after, both sides: its samples and the seven points between.
    ///
    /// Only what reaches `ceiling` matters to the limiter: while no point
    /// between samples can (the window's inputs, through the filter's
    /// largest gain, stay under it), the samples are given instead, which
    /// leaves the limiter doing exactly what it would have done, for a
    /// fraction of the work.
    #[inline]
    pub(crate) fn push(&mut self, left: f64, right: f64, ceiling: f64) -> [f64; 3] {
        let at = self.position;
        for (history, sample) in self.history.iter_mut().zip([left, right]) {
            history[at] = sample;
            history[at + TRUE_PEAK_TAPS] = sample;
        }
        self.position = (at + 1) % TRUE_PEAK_TAPS;
        self.envelope = (self.envelope * ENVELOPE_FALL)
            .max(left.abs())
            .max(right.abs());
        let between = self.envelope * self.bound >= ceiling;
        let start = self.position;
        let mut delayed = [0.0; 2];
        let mut peak = 0.0f64;
        for (side, history) in self.history.iter().enumerate() {
            let window: &[f64; TRUE_PEAK_TAPS] = history[start..start + TRUE_PEAK_TAPS]
                .try_into()
                .expect("the window is as long as a phase");
            delayed[side] = window[TRUE_PEAK_TAPS - 1 - TRUE_PEAK_DELAY];
            peak = peak.max(delayed[side].abs());
            if between {
                for phase in &self.phases {
                    let mut point = 0.0;
                    for (sample, tap) in window.iter().zip(phase) {
                        point += sample * tap;
                    }
                    peak = peak.max(point.abs());
                }
            }
        }
        [delayed[0], delayed[1], peak]
    }
}

/// The master bus.
pub(crate) struct Master {
    pub(crate) params: MasterParams,
    sample_rate: f64,
    clock: Clock,
    /// What the parts send to the reverb, over the quantum being rendered.
    send: AudioBuffer<f32>,
    reverb: SleepyReverb,
    reverb_level: ParamSmoother,
    bus: AudioBuffer<f64>,
    glue: Compressor<f64>,
    glue_switch: Switch,
    glue_makeup: ParamSmoother,
    glue_dry: AudioBuffer<f64>,
    gain: ParamSmoother,
    key: TruePeakKey,
    limiter: Limiter<f64>,
    /// The limiter's ceiling, as a gain.
    ceiling: f64,
    /// The limiter's four channels: left, right, the key and the probe.
    limit: AudioBuffer<f64>,
    latency: usize,
    /// Whether the bus is at rest: silent, empty, and skipped until sound
    /// comes back.
    resting: bool,
    /// Frames in a row on which nothing came into the bus.
    quiet: usize,
    /// How many quiet frames bring it to rest.
    rest_frames: usize,
    /// Frames on which the limiter has turned the level down.
    limited: u64,
    /// The limiter's gain reduction on the last frame, decibels.
    reduction_db: f64,
}

impl Master {
    pub(crate) fn new(spec: &ProcessSpec) -> Self {
        let params = MasterParams::new();
        let rate = spec.sample_rate;
        let frames = spec.max_block_size;
        let stereo = ProcessSpec {
            num_channels: 2,
            ..*spec
        };
        let reverb = Reverb::new()
            .with_mix(1.0)
            .with_decay_seconds(params.reverb.decay.plain())
            .with_size(params.reverb.size.plain())
            .with_damping(params.reverb.damping.plain());
        let mut glue = Compressor::new();
        glue.prepare(&stereo);
        let mut limiter = Limiter::new()
            .with_lookahead_ms(MASTER_LOOKAHEAD_SECONDS * 1_000.0)
            .with_release_ms(params.limiter.release.plain());
        limiter.prepare(&ProcessSpec {
            num_channels: 4,
            ..*spec
        });
        let smoother = |value: f32| {
            let mut smoother = ParamSmoother::new(LEVEL_SMOOTHING);
            smoother.prepare(rate, value);
            smoother
        };
        let mut master = Self {
            sample_rate: rate,
            clock: Clock::new(rate),
            send: AudioBuffer::new(2, frames),
            reverb: SleepyReverb::new(reverb, &stereo),
            reverb_level: smoother(gain_from_db(params.reverb.level.plain())),
            bus: AudioBuffer::new(2, frames),
            glue,
            glue_switch: Switch::new(rate, false),
            glue_makeup: smoother(1.0),
            glue_dry: AudioBuffer::new(2, frames),
            gain: smoother(gain_from_db(params.gain.plain())),
            key: TruePeakKey::new(),
            limiter,
            ceiling: 1.0,
            limit: AudioBuffer::new(4, frames),
            latency: master_latency_frames(rate),
            resting: true,
            quiet: 0,
            rest_frames: master_latency_frames(rate)
                + (MASTER_REST_SECONDS * rate).round() as usize,
            limited: 0,
            reduction_db: 0.0,
            params,
        };
        master.configure_glue();
        master.set_ceiling(master.params.limiter.ceiling.plain());
        debug_assert_eq!(
            master.latency,
            TRUE_PEAK_DELAY + master.limiter.latency_samples()
        );
        master
    }

    /// The frames the bus delays the sound by.
    pub(crate) fn latency(&self) -> usize {
        self.latency
    }

    /// The beat and tempo at the next frame.
    pub(crate) fn clock(&self) -> Clock {
        self.clock
    }

    /// Frames on which the limiter has turned the level down, since the
    /// engine was made.
    pub(crate) fn limited(&self) -> u64 {
        self.limited
    }

    /// How far the limiter turned the level down on the last frame, dB.
    pub(crate) fn reduction_db(&self) -> f64 {
        self.reduction_db
    }

    /// Whether the shared reverb is asleep.
    pub(crate) fn reverb_asleep(&self) -> bool {
        self.reverb.is_asleep()
    }

    /// Whether the glue compressor is in, or fading.
    pub(crate) fn glue_running(&self) -> bool {
        !self.glue_switch.is_silent()
    }

    /// Silences the reverb send for a quantum of `frames` frames.
    pub(crate) fn begin(&mut self, frames: usize) {
        self.send.set_num_frames(frames);
        self.send.clear();
    }

    /// The left and right halves of the reverb send over frames
    /// `from..to` of the quantum, for the parts to add to.
    pub(crate) fn sends(&mut self, from: usize, to: usize) -> (&mut [f32], &mut [f32]) {
        let (left, right) = self.send.channel_pair_mut(0, 1);
        (&mut left[from..to], &mut right[from..to])
    }

    fn configure_glue(&mut self) {
        let p = &self.params.glue;
        Squeeze::configure(
            &mut self.glue,
            p.threshold.plain(),
            p.ratio.plain(),
            p.knee.plain(),
            p.attack.plain(),
            p.release.plain(),
        );
    }

    fn set_ceiling(&mut self, db: f64) {
        let highest = 20.0 * f64::from(OUTPUT_CEILING).log10();
        let db = db.min(highest);
        self.limiter.set_ceiling_db(db);
        // As the limiter works it out.
        self.ceiling = 10f64.powf(db / 20.0);
    }

    /// Takes a new value of a master parameter, `value` exactly as the
    /// event gave it; `false` if `id` is not one.
    pub(crate) fn set_param(&mut self, id: ParamId, value: f64) -> bool {
        let Some(param) = self.params.find_param(id) else {
            return false;
        };
        param.set_plain(value);
        let plain = param.plain();
        match id {
            params::MASTER_TEMPO => {
                self.clock = Clock {
                    origin: self.clock.beat_at(0),
                    since: 0,
                    beats_per_frame: plain / 60.0 / self.sample_rate,
                };
            }
            params::MASTER_BEAT => {
                // The event's own value, which a 32-bit parameter would
                // round: beat 1 000 000.25 needs more than 24 bits.
                if value.is_finite() {
                    self.clock.origin = value.clamp(0.0, 1e12);
                    self.clock.since = 0;
                }
            }
            params::MASTER_GAIN => self.gain.set_target(gain_from_db(plain)),
            params::MASTER_REVERB_LEVEL => self.reverb_level.set_target(gain_from_db(plain)),
            params::MASTER_REVERB_DECAY => self.reverb.reverb.set_decay_seconds(plain),
            params::MASTER_REVERB_SIZE => self.reverb.reverb.set_size(plain),
            params::MASTER_REVERB_DAMPING => self.reverb.reverb.set_damping(plain),
            params::GLUE_ON => {
                let on = plain >= 0.5;
                if on && self.glue_switch.is_silent() {
                    Processor::<f64>::reset(&mut self.glue);
                    self.glue_makeup
                        .reset(gain_from_db(self.params.glue.makeup.plain()));
                }
                self.glue_switch.set(on);
            }
            params::GLUE_MAKEUP => {
                if self.glue_switch.is_silent() {
                    self.glue_makeup.reset(gain_from_db(plain));
                } else {
                    self.glue_makeup.set_target(gain_from_db(plain));
                }
            }
            params::GLUE_THRESHOLD
            | params::GLUE_RATIO
            | params::GLUE_ATTACK
            | params::GLUE_RELEASE
            | params::GLUE_KNEE => self.configure_glue(),
            params::LIMITER_CEILING => self.set_ceiling(plain),
            params::LIMITER_RELEASE => self.limiter.set_release_ms(plain),
            _ => {}
        }
        true
    }

    /// Runs the bus over frames `from..to` of the quantum. `left` and
    /// `right` hold the sum of the parts there, and get the output: what
    /// went in [`latency`](Self::latency) frames earlier, through the bus.
    /// The parts' sends are in [`sends`](Self::sends) over the same frames.
    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32], from: usize, to: usize) {
        let frames = to - from;
        if frames == 0 {
            return;
        }
        // The shared reverb's return.
        self.send
            .windowed(from..to, |piece| self.reverb.process(piece));
        self.bus.set_num_frames(frames);
        {
            let (bus_left, bus_right) = self.bus.channel_pair_mut(0, 1);
            let (wet_left, wet_right) = (
                &self.send.channel(0)[from..to],
                &self.send.channel(1)[from..to],
            );
            for frame in 0..frames {
                let level = f64::from(self.reverb_level.next_value());
                let finite = |sample: f32| {
                    if sample.is_finite() {
                        f64::from(sample)
                    } else {
                        0.0
                    }
                };
                bus_left[frame] = finite(left[from + frame]) + f64::from(wet_left[frame]) * level;
                bus_right[frame] =
                    finite(right[from + frame]) + f64::from(wet_right[frame]) * level;
            }
        }
        let mut frame = 0;
        while frame < frames {
            if self.resting {
                // At rest, silence stays silence: find where sound starts.
                let woken = (frame..frames)
                    .find(|&at| self.bus.channel(0)[at] != 0.0 || self.bus.channel(1)[at] != 0.0);
                let end = woken.unwrap_or(frames);
                left[from + frame..from + end].fill(0.0);
                right[from + frame..from + end].fill(0.0);
                frame = end;
                if woken.is_none() {
                    break;
                }
                self.resting = false;
                self.quiet = 0;
            }
            // Run up to the first frame it could come to rest on, so that it
            // does so on the same frame however the frames arrive.
            let end = frames.min(frame + (self.rest_frames - self.quiet));
            let last_loud = (frame..end)
                .rev()
                .find(|&at| self.bus.channel(0)[at] != 0.0 || self.bus.channel(1)[at] != 0.0);
            self.run(frame, end, &mut left[from..to], &mut right[from..to]);
            self.quiet = match last_loud {
                None => self.quiet + (end - frame),
                Some(at) => end - at - 1,
            };
            if self.quiet >= self.rest_frames {
                self.rest();
            }
            frame = end;
        }
        self.clock.since += frames as u64;
    }

    /// The glue compressor, the master gain and the limiter over frames
    /// `start..end` of the bus, into the same frames of `left` and `right`.
    fn run(&mut self, start: usize, end: usize, left: &mut [f32], right: &mut [f32]) {
        let frames = end - start;
        let (glue, switch, makeup, dry) = (
            &mut self.glue,
            &mut self.glue_switch,
            &mut self.glue_makeup,
            &mut self.glue_dry,
        );
        self.bus.windowed(start..end, |piece| {
            glue_stage(glue, switch, makeup, dry, piece)
        });

        // Master gain, then the limiter, which sees the waveform between
        // samples through the key channel.
        self.limit.set_num_frames(frames);
        {
            let mut channels = self.limit.iter_channels_mut();
            let (Some(out_left), Some(out_right), Some(key), Some(probe)) = (
                channels.next(),
                channels.next(),
                channels.next(),
                channels.next(),
            ) else {
                unreachable!("the limiter's buffer has four channels");
            };
            let (bus_left, bus_right) = (
                &self.bus.channel(0)[start..end],
                &self.bus.channel(1)[start..end],
            );
            let ceiling = self.ceiling;
            for frame in 0..frames {
                let gain = f64::from(self.gain.next_value());
                let [delayed_left, delayed_right, peak] = self
                    .key
                    .push(bus_left[frame] * gain, bus_right[frame] * gain, ceiling);
                out_left[frame] = delayed_left;
                out_right[frame] = delayed_right;
                key[frame] = peak;
                probe[frame] = PROBE;
            }
        }
        self.limiter.process(&mut self.limit);
        let (out_left, out_right, probe) = (
            self.limit.channel(0),
            self.limit.channel(1),
            self.limit.channel(3),
        );
        let ceiling = f64::from(OUTPUT_CEILING);
        for frame in 0..frames {
            // The probe is 0 while it is still on its way through the
            // limiter's delay.
            if probe[frame] > 0.0 && probe[frame] < PROBE {
                self.limited += 1;
            }
            // The limiter keeps every sample under the ceiling; this only
            // catches the last bit of rounding.
            left[start + frame] = out_left[frame].clamp(-ceiling, ceiling) as f32;
            right[start + frame] = out_right[frame].clamp(-ceiling, ceiling) as f32;
        }
        self.reduction_db = self.limiter.gain_reduction_db();
    }

    /// Brings the bus to rest after 200 ms of silence
    /// (`MASTER_REST_SECONDS`), on top of its latency: every sample in it
    /// has come out, and what it
    /// still remembers, the last of the limiter's and the glue's releases,
    /// is let go, as nothing is left for them to act on. Until sound comes
    /// back the bus is skipped.
    fn rest(&mut self) {
        self.resting = true;
        self.quiet = 0;
        self.limiter.reset();
        Processor::<f64>::reset(&mut self.glue);
        self.key.clear();
        for smoother in [&mut self.gain, &mut self.glue_makeup] {
            smoother.reset(smoother.target());
        }
        self.reduction_db = 0.0;
    }
}

/// The glue compressor over `bus`, faded in and out by its switch.
fn glue_stage(
    glue: &mut Compressor<f64>,
    switch: &mut Switch,
    makeup: &mut ParamSmoother,
    dry: &mut AudioBuffer<f64>,
    bus: &mut AudioBuffer<f64>,
) {
    if switch.is_silent() {
        return;
    }
    let steady = switch.is_full() && !makeup.is_smoothing() && makeup.current() == 1.0;
    if steady {
        glue.process(bus);
        return;
    }
    let frames = bus.num_frames();
    dry.set_num_frames(frames);
    dry.copy_from(bus);
    glue.process(bus);
    let (bus_left, bus_right) = bus.channel_pair_mut(0, 1);
    let (dry_left, dry_right) = (dry.channel(0), dry.channel(1));
    for frame in 0..frames {
        let level = f64::from(switch.next());
        let makeup = f64::from(makeup.next_value());
        let wet_left = bus_left[frame] * makeup;
        let wet_right = bus_right[frame] * makeup;
        // In full, the compressor's output itself, as when it is steady.
        if level >= 1.0 {
            bus_left[frame] = wet_left;
            bus_right[frame] = wet_right;
        } else {
            bus_left[frame] = dry_left[frame] + (wet_left - dry_left[frame]) * level;
            bus_right[frame] = dry_right[frame] + (wet_right - dry_right[frame]) * level;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The waveform between samples, read by the key, against the exact
    /// value of a sine.
    #[test]
    fn the_key_reads_between_samples() {
        let mut key = TruePeakKey::new();
        // A sine at a quarter of the rate, its samples 45 degrees either
        // side of its crests: no sample is above 0.707 of its height.
        let mut highest = 0.0f64;
        let mut sample_peak = 0.0f64;
        for n in 0..400 {
            let phase = core::f64::consts::FRAC_PI_2 * n as f64 + core::f64::consts::FRAC_PI_4;
            let x = 0.5 * phase.sin();
            let [delayed, _, peak] = key.push(x, 0.0, 0.0);
            if n > 100 {
                highest = highest.max(peak);
                sample_peak = sample_peak.max(delayed.abs());
            }
        }
        assert!((sample_peak - 0.5 * core::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        // Eight times oversampled, the crest falls on a point it reads.
        assert!((highest - 0.5).abs() < 0.005, "{highest}");
    }

    #[test]
    fn the_latency_is_the_delay_and_the_look_ahead() {
        for rate in [3_000.0, 44_100.0, 48_000.0, 96_000.0, 768_000.0] {
            let master = Master::new(&ProcessSpec {
                sample_rate: rate,
                max_block_size: 128,
                num_channels: 2,
            });
            assert_eq!(master.latency(), master_latency_frames(rate));
        }
    }
}

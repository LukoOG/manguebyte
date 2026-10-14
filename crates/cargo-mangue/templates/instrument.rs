//! Plugin Template: an instrument built with Manguebyte.
//!
//! As it stands it plays a sawtooth for every note, shaped by an envelope.
//! It is the smallest complete instrument: change what a voice sounds like
//! and add the parameters it needs.
//!
//! - The parameters are the fields of one struct. A host automates them, a
//!   saved session restores them, and both are safe from any thread.
//! - A voice is one note. The synth gives each note a voice and delivers
//!   notes, pitch bend and parameter changes on their exact frame.
//! - What comes after the voices, here the output level, follows the same
//!   events through `split_at_events`. Every change lands on its exact
//!   frame, so the output does not depend on how the host sizes its blocks.
//! - Nothing in `process` allocates, locks or waits.
//! - The tests at the end play the plugin with `Harness`, with no host.
//!
//! ```text
//! cargo test                        runs the tests
//! cargo mangue bundle --release     builds the .clap and the .vst3
//! cargo mangue install --release    and copies them to where hosts look
//! cargo mangue validate --release   runs the CLAP and VST 3 validators
//! ```

use std::sync::Arc;

use manguebyte::decibels::db_to_gain;
use manguebyte::prelude::*;

/// The plugin's parameters.
pub struct PluginTemplateParams {
    /// How long a note takes to fade after its key is released, in seconds.
    pub release: FloatParam,
    /// Output level in decibels.
    pub level: FloatParam,
}
param_set!(PluginTemplateParams { release, level });

const RELEASE: ParamId = ParamId::from_name("release");
const LEVEL: ParamId = ParamId::from_name("level");

/// How many notes can sound at once.
const VOICES: usize = 16;

/// How long a change of level takes, in milliseconds. Long enough not to
/// click.
const GLIDE_MS: f64 = 20.0;

impl PluginTemplateParams {
    fn new() -> Self {
        Self {
            release: FloatParam::new(
                "release",
                "Release",
                FloatRange::skewed_around(0.01, 5.0, 0.5),
                0.2,
            )
            .with_unit("s"),
            level: FloatParam::new("level", "Level", FloatRange::linear(-40.0, 6.0), -12.0)
                .with_unit("dB")
                .with_precision(1),
        }
    }
}

/// One note.
struct PluginTemplateVoice {
    oscillator: Oscillator,
    envelope: Adsr,
    shape: AdsrParams,
    key: f64,
    bend: f64,
    velocity: f32,
}

impl PluginTemplateVoice {
    fn new(params: &PluginTemplateParams) -> Self {
        let shape = AdsrParams {
            attack: 0.005,
            decay: 0.1,
            sustain: 0.7,
            release: f64::from(params.release.value()),
        };
        Self {
            oscillator: Oscillator::new(Waveform::Saw, 220.0),
            envelope: Adsr::new(shape),
            shape,
            key: 60.0,
            bend: 0.0,
            velocity: 0.0,
        }
    }

    fn update_pitch(&mut self) {
        self.oscillator
            .set_frequency(note_to_hz(self.key + self.bend));
    }
}

impl Voice for PluginTemplateVoice {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.oscillator.set_sample_rate(spec.sample_rate);
        self.envelope.set_sample_rate(spec.sample_rate);
    }

    fn note_on(&mut self, note: NoteRef, velocity: f32) {
        self.key = f64::from(note.key);
        self.velocity = velocity;
        self.update_pitch();
        self.envelope.note_on();
    }

    fn note_off(&mut self, _velocity: f32) {
        self.envelope.note_off();
    }

    fn choke(&mut self) {
        // Back to the state of a new voice, so that a note sounds the same
        // wherever the host happened to cut its blocks.
        self.envelope.silence();
        self.oscillator.reset_phase();
    }

    fn is_active(&self) -> bool {
        self.envelope.is_active()
    }

    fn render(&mut self, buffer: &mut AudioBuffer<f32>) {
        if !self.envelope.is_active() {
            return;
        }
        for frame in 0..buffer.num_frames() {
            let level = self.envelope.next_sample() as f32;
            let sample = self.oscillator.next_sample() as f32 * level * self.velocity;
            for channel in buffer.iter_channels_mut() {
                channel[frame] += sample;
            }
        }
    }

    fn pitch_bend(&mut self, semitones: f64) {
        self.bend = semitones;
        self.update_pitch();
    }

    fn set_param(&mut self, param: ParamId, value: f64) {
        if param == RELEASE {
            self.shape.release = value;
            self.envelope.set_params(self.shape);
        }
    }
}

/// The plugin.
pub struct PluginTemplate {
    params: Arc<PluginTemplateParams>,
    synth: Synth<PluginTemplateVoice>,
    /// The output level as a factor, on the frame being rendered. It
    /// follows the events and glides to each new value.
    gain: ParamSmoother,
}

impl PluginTemplate {
    /// Takes in one event of the block. The synth has dealt with the notes
    /// and with what changes a voice; the output level is the plugin's own.
    fn handle(&mut self, event: &Event) {
        if let EventKind::ParamValue {
            param: LEVEL,
            value,
        } = event.kind
        {
            self.gain.set_target(db_to_gain(value) as f32);
        }
    }

    /// Applies the output level to one stretch of the block during which
    /// no event arrives.
    fn apply_level(&mut self, piece: &mut AudioBuffer<f32>) {
        for frame in 0..piece.num_frames() {
            let gain = self.gain.next_value();
            for channel in piece.iter_channels_mut() {
                channel[frame] *= gain;
            }
        }
    }
}

impl Plugin for PluginTemplate {
    const INFO: PluginInfo = PluginInfo {
        id: "com.example.plugin-template",
        name: "Plugin Template",
        vendor: "Template Vendor",
        version: env!("CARGO_PKG_VERSION"),
        description: "Template description",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    type Params = PluginTemplateParams;

    fn new() -> Self {
        let params = Arc::new(PluginTemplateParams::new());
        Self {
            synth: Synth::with_voices(VOICES, || PluginTemplateVoice::new(&params)),
            gain: ParamSmoother::new(Smoothing::Linear(GLIDE_MS)),
            params,
        }
    }

    fn params(&self) -> Arc<PluginTemplateParams> {
        Arc::clone(&self.params)
    }

    fn prepare(&mut self, spec: &ProcessSpec) {
        self.synth.prepare(spec);
        // Whatever the parameter holds now, such as a restored session,
        // takes effect whole. From here on the level follows the events.
        let gain = db_to_gain(f64::from(self.params.level.value())) as f32;
        self.gain.prepare(spec.sample_rate, gain);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
        // The synth follows the events, so notes and changes to what a
        // voice sounds like land on their exact frame.
        self.synth.process_with(buffer, context);

        // The output level follows them too. Reading the parameter once
        // per block instead would start a change of level at the start of
        // whichever block it fell in, and the sound would depend on where
        // the host cut its blocks.
        split_at_events(
            self,
            buffer,
            context.events,
            Self::handle,
            Self::apply_level,
        );
    }

    fn reset(&mut self) {
        self.synth.reset();
        self.gain.reset(self.gain.target());
    }
}

manguebyte::clap::export_clap!(PluginTemplate);
manguebyte::vst3::export_vst3!(PluginTemplate);

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// Root-mean-square level of one stretch of the left channel.
    fn level(buffer: &AudioBuffer<f32>, range: std::ops::Range<usize>) -> f64 {
        let slice = &buffer.channel(0)[range];
        let energy: f64 = slice.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (energy / slice.len() as f64).sqrt()
    }

    #[test]
    fn is_silent_until_played_and_fades_after_release() {
        let mut harness = Harness::<PluginTemplate>::new(RATE, 256);
        assert_eq!(harness.render(4_800).peak(), 0.0);

        harness.note(harness.position(), 12_000, 57, 0.9);
        let sound = harness.render(48_000);
        assert!(level(&sound, 1_000..11_000) > 0.01);
        // The release takes 0.2 seconds, so it is over well before this.
        assert_eq!(level(&sound, 30_000..48_000), 0.0);
        assert!(sound.peak() < 1.0, "the default level clips");
        assert_eq!(sound.channel(0), sound.channel(1));
    }

    #[test]
    fn plays_the_pitch_it_is_asked_for() {
        use manguebyte::dsp::fft::{Complex, RealFft};

        let mut harness = Harness::<PluginTemplate>::new(RATE, 512);
        harness.note(0, 40_000, 69, 1.0); // A4, 440 Hz
        let sound = harness.render(32_768);

        let mut fft = RealFft::<f32>::new(32_768);
        let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
        fft.forward(sound.channel(0), &mut spectrum);
        let loudest = (1..spectrum.len())
            .max_by(|&a, &b| spectrum[a].norm_sqr().total_cmp(&spectrum[b].norm_sqr()))
            .unwrap();
        let hertz = loudest as f64 * RATE / 32_768.0;
        assert!((hertz - 440.0).abs() < 2.0, "loudest partial at {hertz} Hz");
    }

    #[test]
    fn the_level_parameter_sets_how_loud_it_is() {
        let loudness = |decibels: f32| {
            let mut harness = Harness::<PluginTemplate>::new(RATE, 256);
            harness.params().level.set(decibels);
            harness.note(0, 20_000, 57, 1.0);
            level(&harness.render(14_400), 4_800..14_400)
        };
        // Twelve decibels apart is a factor of 3.98.
        let ratio = loudness(-12.0) / loudness(-24.0);
        assert!((ratio - 3.981).abs() < 0.01, "a ratio of {ratio}");
    }

    #[test]
    fn a_change_of_level_starts_on_its_own_frame() {
        let render = |turned_down: bool| {
            let mut harness = Harness::<PluginTemplate>::new(RATE, 256);
            harness.note(0, 20_000, 57, 1.0);
            if turned_down {
                // In the middle of a block, not on its edge.
                harness.automate(1_000, "level", -30.0);
            }
            harness.render(4_800)
        };
        let (steady, turned_down) = (render(false), render(true));
        let (steady, turned_down) = (steady.channel(0), turned_down.channel(0));

        // Untouched up to the frame of the change and quieter from that
        // frame on.
        assert_eq!(steady[..1_000], turned_down[..1_000]);
        assert!(turned_down[1_000].abs() < steady[1_000].abs());
        // It glides and does not jump: half of the way after half of the
        // 960 frames, and 18 dB down from the end of them on.
        let ratio = db_to_gain(-18.0) as f32;
        let halfway = steady[1_479] * (1.0 + ratio) / 2.0;
        assert!((turned_down[1_479] - halfway).abs() < 1e-6);
        for frame in 1_959..4_800 {
            let expected = steady[frame] * ratio;
            assert!((turned_down[frame] - expected).abs() < 1e-6, "{frame}");
        }
    }

    #[test]
    fn sounds_the_same_whatever_the_block_size() {
        let render = |max: usize, pattern: &[usize]| {
            let mut harness = Harness::<PluginTemplate>::new(RATE, max);
            harness = harness.with_block_pattern(pattern);
            harness.note(100, 9_000, 45, 0.8);
            harness.note(4_000, 9_000, 52, 0.7);
            harness.note(4_001, 2_000, 57, 0.6);
            harness.automate(7_777, "release", 0.05);
            harness.automate(5_000, "level", -30.0);
            // A change that arrives while the last one is still gliding.
            harness.automate(5_300, "level", -6.0);
            harness.render(24_000)
        };
        let reference = render(256, &[256]);
        assert!(reference.peak() > 0.01);
        assert!(render(64, &[64]) == reference);
        assert!(render(1_024, &[1, 999, 13, 1_024]) == reference);
    }

    #[test]
    fn state_restores_every_parameter() {
        let harness = Harness::<PluginTemplate>::new(RATE, 256);
        harness.params().release.set(1.5);
        harness.params().level.set(-3.0);
        let saved = harness.save_state();

        let fresh = Harness::<PluginTemplate>::new(RATE, 256);
        assert!(fresh.load_state(&saved).unwrap().is_exact());
        assert_eq!(fresh.params().release.value(), 1.5);
        assert_eq!(fresh.params().level.value(), -3.0);
    }
}

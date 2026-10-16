//! Plugin Template: an audio effect built with Manguebyte.
//!
//! As it stands it turns its input up or down. It is the smallest complete
//! plugin: put your own processing where the gain is.
//!
//! - The parameters are the fields of one struct. A host automates them, a
//!   saved session restores them, and both are safe from any thread.
//! - `process` receives each block together with what happens in it.
//!   `split_at_events` applies every parameter change on its exact frame, so
//!   the output does not depend on how the host sizes its blocks.
//! - Nothing in `process` allocates, locks or waits.
//! - The tests at the end run the plugin with `Harness`, with no host.
//!
//! ```text
//! cargo test                        runs the tests
//! cargo mangue bundle --release     builds the .clap and the .vst3
//! cargo mangue bundle --release --format lv2   builds the .lv2
//! cargo mangue install --release    and copies them to where hosts look
//! cargo mangue validate --release   runs the CLAP and VST 3 validators
//! ```

use std::sync::Arc;

use manguebyte::decibels::db_to_gain;
use manguebyte::prelude::*;

/// The plugin's parameters.
pub struct PluginTemplateParams {
    /// How much the input is turned up or down, in decibels.
    pub gain: FloatParam,
}
param_set!(PluginTemplateParams { gain });

const GAIN: ParamId = ParamId::from_name("gain");

/// How long a change of gain takes, in milliseconds. Long enough not to
/// click.
const GLIDE_MS: f64 = 10.0;

impl PluginTemplateParams {
    fn new() -> Self {
        Self {
            gain: FloatParam::new("gain", "Gain", FloatRange::linear(-60.0, 12.0), 0.0)
                .with_unit("dB")
                .with_precision(1),
        }
    }
}

/// The plugin.
pub struct PluginTemplate {
    params: Arc<PluginTemplateParams>,
    /// The factor applied on the frame being rendered. It follows the
    /// events and glides to each new value.
    gain: ParamSmoother,
}

impl PluginTemplate {
    /// Takes in one event of the block. Only parameter changes matter to
    /// an effect like this one.
    fn handle(&mut self, event: &Event) {
        if let EventKind::ParamValue { param: GAIN, value } = event.kind {
            self.gain.set_target(db_to_gain(value) as f32);
        }
    }

    /// Renders one stretch of the block during which no parameter changes.
    fn render(&mut self, piece: &mut AudioBuffer<f32>) {
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
        kind: PluginKind::Effect,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_EFFECT;
    type Params = PluginTemplateParams;

    fn new() -> Self {
        Self {
            params: Arc::new(PluginTemplateParams::new()),
            gain: ParamSmoother::new(Smoothing::Linear(GLIDE_MS)),
        }
    }

    fn params(&self) -> Arc<PluginTemplateParams> {
        Arc::clone(&self.params)
    }

    fn prepare(&mut self, spec: &ProcessSpec) {
        // Whatever the parameter holds now, such as a restored session,
        // takes effect whole. From here on the plugin follows the events.
        let gain = db_to_gain(f64::from(self.params.gain.value())) as f32;
        self.gain.prepare(spec.sample_rate, gain);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
        split_at_events(self, buffer, context.events, Self::handle, Self::render);
    }

    fn reset(&mut self) {
        self.gain.reset(self.gain.target());
    }
}

manguebyte::clap::export_clap!(PluginTemplate);
manguebyte::vst3::export_vst3!(PluginTemplate);
manguebyte::lv2::export_lv2!(PluginTemplate);

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// A stereo buffer holding the same value on every frame.
    fn steady(level: f32, frames: usize) -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(2, frames);
        for channel in buffer.iter_channels_mut() {
            channel.fill(level);
        }
        buffer
    }

    /// Something that is not the same on every frame, to pass through.
    fn programme(frames: usize) -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(2, frames);
        for (index, channel) in buffer.iter_channels_mut().enumerate() {
            for (frame, sample) in channel.iter_mut().enumerate() {
                let phase = frame as f32 * (0.05 + 0.01 * index as f32);
                *sample = 0.5 * phase.sin();
            }
        }
        buffer
    }

    #[test]
    fn leaves_the_input_alone_at_zero_decibels() {
        let mut harness = Harness::<PluginTemplate>::new(RATE, 256);
        let input = programme(4_800);
        assert!(harness.process(&input) == input);
    }

    #[test]
    fn turns_the_input_down_by_the_gain() {
        let mut harness = Harness::<PluginTemplate>::new(RATE, 256);
        harness.params().gain.set(-6.0);
        let output = harness.process(&steady(1.0, 4_800));
        let expected = db_to_gain(-6.0) as f32;
        assert!((output.channel(0)[4_799] - expected).abs() < 1e-6);
        assert_eq!(output.channel(0), output.channel(1));
    }

    #[test]
    fn automation_lands_on_its_frame_and_does_not_click() {
        let mut harness = Harness::<PluginTemplate>::new(RATE, 256);
        // A change in the middle of a block, not on its edge.
        harness.automate(1_000, "gain", -12.0);
        let output = harness.process(&steady(1.0, 4_800));
        let samples = output.channel(0);
        let target = db_to_gain(-12.0) as f32;

        // Untouched up to the frame of the change, moving from that frame
        // on, and there once the glide of 480 frames is over.
        assert_eq!(samples[999], 1.0);
        assert!(samples[1_000] < 1.0);
        assert!((samples[1_480] - target).abs() < 1e-6);
        let largest_step = samples
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        assert!(largest_step < 0.002, "a step of {largest_step}");
    }

    #[test]
    fn sounds_the_same_whatever_the_block_size() {
        let render = |max: usize, pattern: &[usize]| {
            let mut harness = Harness::<PluginTemplate>::new(RATE, max);
            harness = harness.with_block_pattern(pattern);
            harness.automate(2_500, "gain", -20.0);
            // A change that arrives while the last one is still gliding.
            harness.automate(2_600, "gain", 6.0);
            harness.automate(9_001, "gain", -3.0);
            harness.process(&programme(24_000))
        };
        let reference = render(256, &[256]);
        assert!(reference != programme(24_000), "nothing changed");
        assert!(render(64, &[64]) == reference);
        assert!(render(1_024, &[1, 999, 13, 1_024]) == reference);
    }

    #[test]
    fn state_restores_every_parameter() {
        let harness = Harness::<PluginTemplate>::new(RATE, 256);
        harness.params().gain.set(-9.5);
        let saved = harness.save_state();

        let fresh = Harness::<PluginTemplate>::new(RATE, 256);
        assert!(fresh.load_state(&saved).unwrap().is_exact());
        assert_eq!(fresh.params().gain.value(), -9.5);
    }
}

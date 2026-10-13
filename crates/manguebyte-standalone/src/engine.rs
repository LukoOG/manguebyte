use std::sync::Arc;

use manguebyte_core::AudioBuffer;
use manguebyte_core::spsc::Consumer;
use manguebyte_devices::midi::{MidiInputMessage, MidiReceiver};
use manguebyte_dsp::event::EventBuffer;
use manguebyte_dsp::{ProcessContext, ProcessSpec, Transport};
use manguebyte_midi::MidiMessage;
use manguebyte_params::ParamSet;
use manguebyte_plugin::{ParamWatcher, Plugin, apply_param_events};

/// Most events delivered to the plugin in one block.
const EVENTS_PER_BLOCK: usize = 2_048;

/// The part of a standalone application that runs on the audio thread.
///
/// It does for a plugin what a host does: gathers MIDI into events with
/// their frames, turns parameter changes into events, keeps a transport
/// moving and maps the plugin's channels onto the device's.
/// [`Standalone`](crate::Standalone) owns one and gives it to the device
/// callback; it is public so that the same logic can be driven by something
/// else, such as a test or a custom audio back end.
pub struct Engine<P: Plugin> {
    plugin: P,
    params: Arc<P::Params>,
    watcher: ParamWatcher,
    events: EventBuffer,
    transport: Transport,
    sample_rate: f64,
    midi: Vec<MidiReceiver>,
    notes: Consumer<MidiMessage>,
    /// The block in the plugin's own channel layout.
    work: AudioBuffer<f32>,
}

impl<P: Plugin> Engine<P> {
    /// Creates the plugin and prepares it.
    ///
    /// `midi` are the ports to read; `notes` carries messages sent by the
    /// program itself. Returns a description of the problem if the plugin's
    /// parameters do not pass validation.
    pub fn new(
        sample_rate: f64,
        max_block_size: usize,
        tempo_bpm: f64,
        midi: Vec<MidiReceiver>,
        notes: Consumer<MidiMessage>,
    ) -> Result<Self, String> {
        let mut plugin = P::new();
        let params = plugin.params();
        params.validate().map_err(|error| error.to_string())?;
        plugin.prepare(&ProcessSpec {
            sample_rate,
            max_block_size,
            num_channels: P::LAYOUT.outputs,
        });
        Ok(Self {
            watcher: ParamWatcher::new(&*params),
            plugin,
            params,
            events: EventBuffer::with_capacity(EVENTS_PER_BLOCK),
            transport: Transport::playing_at(tempo_bpm),
            sample_rate,
            midi,
            notes,
            work: AudioBuffer::new(P::LAYOUT.outputs, max_block_size),
        })
    }

    /// The plugin's parameters.
    pub fn params(&self) -> Arc<P::Params> {
        Arc::clone(&self.params)
    }

    /// Produces one block for the device.
    ///
    /// `input` is what the device captured, if anything. `output` is the
    /// device's buffer, in the device's channel count; it must not be longer
    /// than the block size the engine was created with. Real-time safe.
    pub fn render(&mut self, input: Option<&AudioBuffer<f32>>, output: &mut AudioBuffer<f32>) {
        let frames = output.num_frames().min(self.work.capacity());
        let params: &dyn ParamSet = &*self.params;

        // Parameter changes first, then notes from the program, then MIDI
        // ports, each message on the frame its arrival time maps to.
        self.events.clear();
        self.watcher.emit_changes(params, &mut self.events);
        while let Some(message) = self.notes.pop() {
            let _ = self.events.push_midi(0, message);
        }
        for receiver in &mut self.midi {
            let events = &mut self.events;
            receiver.read(frames, self.sample_rate, |offset, message| {
                if let MidiInputMessage::Channel(message) = message {
                    let _ = events.push_midi(offset, message);
                }
            });
        }
        apply_param_events(params, &self.events);
        self.watcher.remember(params);

        self.work.set_num_frames(frames);
        self.work.clear();
        if let Some(input) = input {
            let channels = P::LAYOUT
                .inputs
                .min(self.work.num_channels())
                .min(input.num_channels());
            let shared = frames.min(input.num_frames());
            for channel in 0..channels {
                self.work.channel_mut(channel)[..shared]
                    .copy_from_slice(&input.channel(channel)[..shared]);
            }
        }

        let context = ProcessContext::new(&self.events, &self.transport);
        self.plugin.process(&mut self.work, &context);
        self.transport.advance(frames, self.sample_rate);

        // A mono plugin is heard on both speakers; otherwise channels map
        // one to one and any extra device channels stay silent.
        let plugin_channels = self.work.num_channels();
        for channel in 0..output.num_channels() {
            let source = if plugin_channels == 1 && channel < 2 {
                Some(0)
            } else {
                (channel < plugin_channels).then_some(channel)
            };
            let target = output.channel_mut(channel);
            match source {
                Some(source) => {
                    target[..frames].copy_from_slice(self.work.channel(source));
                    target[frames..].fill(0.0);
                }
                None => target.fill(0.0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manguebyte_core::spsc::{self, Producer};
    use manguebyte_dsp::event::{EventKind, NoteRef};
    use manguebyte_dsp::{Processor, Synth, Voice};
    use manguebyte_params::{FloatParam, FloatRange, param_set};
    use manguebyte_plugin::{AudioLayout, PluginInfo, PluginKind};

    struct LevelParams {
        level: FloatParam,
    }
    param_set!(LevelParams { level });

    /// Outputs the key number of each held note, times `level`.
    #[derive(Default)]
    struct KeyVoice(f32);
    impl Voice for KeyVoice {
        fn prepare(&mut self, _: &ProcessSpec) {}
        fn note_on(&mut self, note: NoteRef, _: f32) {
            self.0 = f32::from(note.key);
        }
        fn note_off(&mut self, _: f32) {
            self.0 = 0.0;
        }
        fn choke(&mut self) {
            self.0 = 0.0;
        }
        fn is_active(&self) -> bool {
            self.0 > 0.0
        }
        fn render(&mut self, buffer: &mut AudioBuffer<f32>) {
            for channel in buffer.iter_channels_mut() {
                channel.iter_mut().for_each(|sample| *sample += self.0);
            }
        }
    }

    struct Keys {
        synth: Synth<KeyVoice>,
        params: Arc<LevelParams>,
        level: f32,
    }

    impl Plugin for Keys {
        const INFO: PluginInfo = PluginInfo {
            id: "org.manguebyte.test.standalone-keys",
            name: "Keys",
            vendor: "Manguebyte tests",
            version: "0.1.0",
            description: "Plays key numbers",
            kind: PluginKind::Instrument,
        };
        const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
        type Params = LevelParams;

        fn new() -> Self {
            Self {
                synth: Synth::with_voices(4, KeyVoice::default),
                params: Arc::new(LevelParams {
                    level: FloatParam::new("level", "Level", FloatRange::linear(0.0, 2.0), 1.0),
                }),
                level: 1.0,
            }
        }
        fn params(&self) -> Arc<LevelParams> {
            Arc::clone(&self.params)
        }
        fn prepare(&mut self, spec: &ProcessSpec) {
            self.synth.prepare(spec);
        }
        fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
            // Follows events only, like a plugin that never polls.
            for event in context.events {
                if let EventKind::ParamValue { value, .. } = event.kind {
                    self.level = value as f32;
                }
            }
            self.synth.process_with(buffer, context);
            buffer.apply_gain(self.level);
        }
    }

    /// Scales its mono input.
    struct Half(Arc<LevelParams>);
    impl Plugin for Half {
        const INFO: PluginInfo = PluginInfo {
            id: "org.manguebyte.test.standalone-half",
            name: "Half",
            vendor: "Manguebyte tests",
            version: "0.1.0",
            description: "Halves the input",
            kind: PluginKind::Effect,
        };
        const LAYOUT: AudioLayout = AudioLayout::MONO_EFFECT;
        type Params = LevelParams;

        fn new() -> Self {
            Self(Arc::new(LevelParams {
                level: FloatParam::new("level", "Level", FloatRange::linear(0.0, 2.0), 0.5),
            }))
        }
        fn params(&self) -> Arc<LevelParams> {
            Arc::clone(&self.0)
        }
        fn prepare(&mut self, _: &ProcessSpec) {}
        fn process(&mut self, buffer: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {
            buffer.apply_gain(self.0.level.value());
        }
    }

    fn engine<P: Plugin>() -> (Engine<P>, Producer<MidiMessage>) {
        let (notes, receiver) = spsc::channel(64);
        let engine = Engine::<P>::new(48_000.0, 16, 120.0, Vec::new(), receiver).unwrap();
        (engine, notes)
    }

    fn on(note: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
        }
    }

    #[test]
    fn notes_sent_by_the_program_are_played_and_parameters_are_followed() {
        let (mut engine, mut notes) = engine::<Keys>();
        let params = engine.params();
        let mut device = AudioBuffer::<f32>::new(2, 8);

        engine.render(None, &mut device);
        assert_eq!(device.peak(), 0.0);

        notes.push(on(60)).unwrap();
        notes.push(on(64)).unwrap();
        engine.render(None, &mut device);
        assert_eq!(device.channel(0), &[124.0; 8]);
        assert_eq!(device.channel(1), &[124.0; 8]);

        // A parameter written from another thread reaches a plugin that
        // only follows events.
        params.level.set(0.5);
        notes
            .push(MidiMessage::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0,
            })
            .unwrap();
        engine.render(None, &mut device);
        assert_eq!(device.channel(0), &[32.0; 8]);
    }

    #[test]
    fn the_plugin_layout_is_mapped_onto_the_device() {
        // A stereo plugin on a device with four outputs: the extra two
        // stay silent.
        let (mut engine, mut notes) = engine::<Keys>();
        notes.push(on(10)).unwrap();
        let mut quad = AudioBuffer::<f32>::new(4, 8);
        quad.channel_mut(3).fill(9.0);
        engine.render(None, &mut quad);
        assert_eq!(quad.channel(1), &[10.0; 8]);
        assert_eq!(quad.channel(2), &[0.0; 8]);
        assert_eq!(quad.channel(3), &[0.0; 8]);

        // On a mono device, the left channel is what is heard.
        let mut mono = AudioBuffer::<f32>::new(1, 8);
        engine.render(None, &mut mono);
        assert_eq!(mono.channel(0), &[10.0; 8]);
    }

    #[test]
    fn an_effect_processes_the_captured_input_and_mono_goes_to_both_speakers() {
        let (mut engine, _notes) = engine::<Half>();
        let mut captured = AudioBuffer::<f32>::new(2, 8);
        captured.channel_mut(0).fill(1.0);
        captured.channel_mut(1).fill(100.0); // not an input of a mono plugin
        let mut device = AudioBuffer::<f32>::new(2, 8);
        engine.render(Some(&captured), &mut device);
        assert_eq!(device.channel(0), &[0.5; 8]);
        assert_eq!(device.channel(1), &[0.5; 8]);

        // With no input device, an effect hears silence.
        engine.render(None, &mut device);
        assert_eq!(device.peak(), 0.0);
    }

    #[test]
    fn a_block_longer_than_expected_does_not_panic() {
        let (mut engine, mut notes) = engine::<Keys>();
        notes.push(on(10)).unwrap();
        let mut device = AudioBuffer::<f32>::new(2, 40);
        device.channel_mut(0).fill(7.0);
        engine.render(None, &mut device);
        // What fits is rendered; the rest is silence rather than garbage.
        assert_eq!(&device.channel(0)[..16], &[10.0; 16]);
        assert_eq!(&device.channel(0)[16..], &[0.0; 24]);
    }
}

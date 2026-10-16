//! Loading an SFZ instrument for a sampler part, one file at a time.

use std::io;

use manguebyte_sampler::{
    Instrument, InstrumentBuilder, InstrumentError, MemorySource, SampleSource, Sfz,
};

/// An SFZ instrument on its way to a sampler part, built in short steps.
///
/// [`new`](Self::new) parses the text, which names the sample files;
/// [`paths`](Self::paths) lists them, each once, as the text resolves them
/// (after `default_path`, with `\` turned into `/` and `.` and `..`
/// resolved), so that the caller can fetch them; [`add_file`](Self::add_file)
/// decodes one; and [`finish`](Self::finish) makes the instrument, which
/// [`Engine::load_instrument`](crate::Engine::load_instrument) installs.
/// Each step is bounded by the size of one file, which is what lets
/// `js/worklet.js` do one between two render quanta without a glitch.
///
/// The text cannot `#include` other files: there is nothing to read them
/// from while it is parsed, so such text is an error. Samples are played at
/// their own rate, at the right ratio to the engine's (see
/// [`SfzInstrument`]), so nothing is resampled while loading.
///
/// ```
/// use manguebyte_web::{Engine, EngineConfig, InstrumentKind, SfzLoad};
/// # use manguebyte_core::AudioBuffer;
/// # use manguebyte_formats::wav::{self, WavFormat};
/// # let mut tone = AudioBuffer::<f32>::new(1, 2_400);
/// # for (n, s) in tone.channel_mut(0).iter_mut().enumerate() {
/// #     *s = (std::f64::consts::TAU * 440.0 * n as f64 / 24_000.0).sin() as f32 * 0.5;
/// # }
/// # let mut a4 = Vec::new();
/// # wav::write(&mut a4, &tone, 24_000, WavFormat::Int16).unwrap();
///
/// let mut load = SfzLoad::new(
///     "<control> default_path=./
///      <region> sample=a4.wav pitch_keycenter=69 lokey=0 hikey=127",
/// )?;
/// assert_eq!(load.paths().collect::<Vec<_>>(), ["a4.wav"]);
/// load.add_file("a4.wav", a4)?; // the bytes of the WAV file
/// let instrument = load.finish()?;
///
/// let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
/// engine.create_part(0, InstrumentKind::Sampler).unwrap();
/// engine.load_instrument(0, instrument).unwrap();
/// # Ok::<(), manguebyte_sampler::InstrumentError>(())
/// ```
pub struct SfzLoad {
    builder: InstrumentBuilder,
}

impl core::fmt::Debug for SfzLoad {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SfzLoad")
            .field("builder", &self.builder)
            .finish()
    }
}

/// A finished SFZ instrument, its samples decoded, ready for
/// [`Engine::load_instrument`](crate::Engine::load_instrument).
///
/// A sample plays at its own rate, read between its frames at the ratio of
/// that rate to the engine's (so a 24 kHz sample in a 48 kHz engine is read
/// at half speed, and sounds at the pitch it was recorded at), with the
/// interpolation the part's `sampler.interpolation` parameter chooses.
pub type SfzInstrument = Instrument;

/// Hands out each file once, by moving it out.
struct OneFile {
    path: String,
    bytes: Option<Vec<u8>>,
}

impl SampleSource for OneFile {
    fn read(&mut self, path: &str) -> io::Result<Vec<u8>> {
        match self.bytes.take() {
            Some(bytes) if path == self.path => Ok(bytes),
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no file named {path:?} was given"),
            )),
        }
    }
}

impl SfzLoad {
    /// Parses SFZ text.
    ///
    /// # Errors
    ///
    /// [`InstrumentError::Sfz`] if the text is not valid SFZ, or has an
    /// `#include`.
    pub fn new(text: &str) -> Result<Self, InstrumentError> {
        let sfz = Sfz::parse(text)?;
        Ok(Self {
            builder: InstrumentBuilder::new(sfz),
        })
    }

    /// The files the text names, each once, in the order its regions first
    /// name them, as [`add_file`](Self::add_file) takes them.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.builder.paths()
    }

    /// The files of [`paths`](Self::paths) not added yet.
    pub fn missing(&self) -> impl Iterator<Item = &str> {
        self.builder.missing()
    }

    /// [`missing`](Self::missing) as a JSON array of strings. Allocates.
    pub fn missing_json(&self) -> String {
        let mut json = String::from("[");
        for (index, path) in self.missing().enumerate() {
            if index > 0 {
                json.push(',');
            }
            push_json_string(&mut json, path);
        }
        json.push(']');
        json
    }

    /// The parsed text.
    pub fn sfz(&self) -> &Sfz {
        self.builder.sfz()
    }

    /// Decodes the sample file at `path`, one of [`paths`](Self::paths),
    /// from its bytes. Returns `false`, doing nothing, for a path the text
    /// does not name or one already added. Allocates.
    ///
    /// # Errors
    ///
    /// [`InstrumentError::Load`] if the bytes are not audio this framework
    /// decodes (WAV, AIFF or FLAC), or are longer than
    /// [`MAX_SAMPLE_FRAMES`](manguebyte_sampler::MAX_SAMPLE_FRAMES).
    pub fn add_file(&mut self, path: &str, bytes: Vec<u8>) -> Result<bool, InstrumentError> {
        let mut source = OneFile {
            path: path.to_owned(),
            bytes: Some(bytes),
        };
        Ok(self.builder.load(path, &mut source)?)
    }

    /// Makes the instrument. Allocates.
    ///
    /// # Errors
    ///
    /// [`InstrumentError::Load`] if a file is still missing, naming it.
    pub fn finish(self) -> Result<SfzInstrument, InstrumentError> {
        Ok(self.builder.finish(&mut MemorySource::new())?)
    }
}

/// Appends `text` to `json` as a JSON string.
pub(crate) fn push_json_string(json: &mut String, text: &str) {
    json.push('"');
    for character in text.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            control if u32::from(control) < 0x20 => {
                json.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => json.push(other),
        }
    }
    json.push('"');
}

/// A JSON summary of an instrument: how many regions and samples it has,
/// the bytes of decoded audio it holds, and what the parser had to say.
/// Allocates.
pub(crate) fn summary_json(instrument: &SfzInstrument) -> String {
    let mut json = format!(
        "{{\"regions\":{},\"samples\":{},\"sample_bytes\":{},\"warnings\":[",
        instrument.num_regions(),
        instrument.num_samples(),
        instrument.sample_bytes()
    );
    for (index, warning) in instrument.warnings().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        push_json_string(&mut json, &warning.to_string());
    }
    json.push_str("]}");
    json
}

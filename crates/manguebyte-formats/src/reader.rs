//! Reading an audio file block by block.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::aiff::{self, AiffDecoder};
use crate::flac::FlacReader;
use crate::metadata::Metadata;
use crate::mp3::{self, Mp3Reader};
use crate::pcm::{PcmReader, read_up_to};
use crate::vorbis::VorbisReader;
use crate::wav::{self, WavDecoder};
use crate::{FileFormat, FormatError, LOSSY_BITS};

/// Reads an audio file a block at a time, from any position.
///
/// Where [`read_file`](crate::read_file) loads a whole file, this keeps only
/// the block being read: a recording of an hour costs the same memory as one
/// of a second. It reads WAV (RF64 included), AIFF, AIFF-C, FLAC, Ogg
/// Vorbis and MP3, and works out which one it has been given from the
/// contents.
///
/// ```
/// use std::io::Cursor;
///
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_formats::AudioReader;
/// use manguebyte_formats::wav::{self, WavFormat};
///
/// // A file of ten frames, held in memory for the example.
/// let mut source = AudioBuffer::<f32>::new(1, 10);
/// for (frame, sample) in source.channel_mut(0).iter_mut().enumerate() {
///     *sample = frame as f32 / 16.0;
/// }
/// let mut bytes = Vec::new();
/// wav::write(&mut bytes, &source, 48_000, WavFormat::Float32).unwrap();
///
/// let mut reader = AudioReader::new(Cursor::new(bytes))?;
/// assert_eq!(reader.sample_rate(), 48_000);
/// assert_eq!(reader.len_frames(), Some(10));
///
/// // Blocks of four frames: the last one comes back short.
/// let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 4);
/// assert_eq!(reader.read(&mut block)?, 4);
/// assert_eq!(reader.read(&mut block)?, 4);
/// assert_eq!(reader.read(&mut block)?, 2);
/// assert_eq!(reader.read(&mut block)?, 0);
///
/// // And back to any frame.
/// reader.seek(7)?;
/// assert_eq!(reader.read(&mut block)?, 3);
/// assert_eq!(block.channel(0)[0], 7.0 / 16.0);
/// # Ok::<(), manguebyte_formats::FormatError>(())
/// ```
///
/// # Memory
///
/// For WAV and AIFF the reader holds one buffer of at most 32 KiB, or of one
/// frame if a frame is larger than that, allocated when the file is opened.
/// Reading and seeking do not touch the heap after that.
///
/// For FLAC it holds a stretch of the file about one frame long and at
/// least 32 KiB (two frames while a seek is under way), one decoded frame,
/// and the seek table, of which at most 32768 points are kept. None of this
/// grows with the length of the file, and none of it is sized by what a
/// header claims beyond the limits of the format.
///
/// For Ogg Vorbis it holds the codebooks of the stream, a stretch of the
/// file of a page or two, and for each channel a block and the half block
/// that overlaps the next; a stereo stream as libvorbis writes it takes
/// about 300 KB. The [`vorbis`](crate::vorbis) module gives the limits.
///
/// For MP3 it holds one frame, the bit reservoir, the decoder's state, and
/// the place of every so many frames of the stream, at most 16384 of them:
/// under 100 KB for a stream of any length.
///
/// # Threads
///
/// Reading does I/O, so it does not belong on the audio thread. To play a
/// file from an audio callback, hand the reader to a
/// [`StreamingPlayer`](crate::StreamingPlayer).
///
/// # Limits
///
/// - The file must start at the beginning of the stream it is read from.
/// - A FLAC stream that declares its length is read up to that length, even
///   if more frames follow. [`flac::read`](crate::flac::read) decodes every
///   frame there is.
/// - A FLAC frame of more bytes than twice what the largest block the
///   format allows takes uncompressed is rejected as damaged. No encoder
///   writes one, and without the limit a damaged file could ask for
///   unbounded memory.
/// - A FLAC file whose first frame has a damaged header is not opened: that
///   header says how the frames of the stream are numbered.
/// - A FLAC frame that passes its checksums and decodes to samples outside
///   the stream's bit depth is rejected as malformed. No encoder writes one,
///   and playing it would be playing noise at full scale.
/// - Seeking in FLAC relies on what the frames say about themselves. A
///   stream whose frames carry the wrong numbers can be read from its
///   start, which does not look at them, but a seek in it may land in the
///   wrong place. So may a seek in a file made to hold, inside its audio,
///   a run of bytes that reads as one whole frame followed by the next. No
///   encoder produces either. With a file that is not trusted, read on from
///   the start, or from a seek to frame 0, to be certain of the place.
pub struct AudioReader<R> {
    inner: Inner<R>,
    format: FileFormat,
    sample_rate: u32,
    bits_per_sample: u16,
    is_float: bool,
    metadata: Metadata,
}

enum Inner<R> {
    Wav(PcmReader<R, WavDecoder>),
    Aiff(PcmReader<R, AiffDecoder>),
    // Boxed because a FLAC decoder's state is much larger than the others'.
    Flac(Box<FlacReader<R>>),
    Vorbis(Box<VorbisReader<R>>),
    Mp3(Box<Mp3Reader<R>>),
}

impl AudioReader<File> {
    /// Opens the audio file at `path`.
    ///
    /// The format is recognised from the file's contents, not from its name.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, FormatError> {
        Self::new(File::open(path)?)
    }
}

impl<R: Read + Seek> AudioReader<R> {
    /// Reads the header of the audio file in `reader` and gets ready to read
    /// from its first frame.
    ///
    /// Only the header is read here. The reader does its own buffering, so
    /// `reader` can be a plain [`File`].
    pub fn new(mut reader: R) -> Result<Self, FormatError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut magic = [0u8; 10];
        let have = read_up_to(&mut reader, &mut magic)?;
        let format = FileFormat::detect(&magic[..have]).ok_or(FormatError::Unrecognized)?;
        Ok(match format {
            FileFormat::Wav => {
                let mut stream = wav::open(&mut reader).map_err(FormatError::Wav)?;
                Self {
                    format,
                    sample_rate: stream.sample_rate,
                    bits_per_sample: stream.bits_per_sample,
                    is_float: stream.is_float,
                    metadata: std::mem::take(&mut stream.metadata),
                    inner: Inner::Wav(PcmReader::new(reader, stream.decoder(), stream.layout)),
                }
            }
            FileFormat::Aiff => {
                let mut stream = aiff::open(&mut reader).map_err(FormatError::Aiff)?;
                Self {
                    format,
                    sample_rate: stream.sample_rate,
                    bits_per_sample: stream.bits_per_sample,
                    is_float: stream.is_float,
                    metadata: std::mem::take(&mut stream.metadata),
                    inner: Inner::Aiff(PcmReader::new(reader, stream.decoder(), stream.layout)),
                }
            }
            FileFormat::Flac => {
                let mut stream = FlacReader::new(reader).map_err(FormatError::Flac)?;
                Self {
                    format,
                    sample_rate: stream.sample_rate(),
                    bits_per_sample: stream.bits_per_sample(),
                    is_float: false,
                    metadata: stream.take_metadata(),
                    inner: Inner::Flac(Box::new(stream)),
                }
            }
            FileFormat::OggVorbis => {
                let mut stream =
                    VorbisReader::with_comments(reader).map_err(FormatError::Vorbis)?;
                let comments = stream.take_comments();
                let metadata = Metadata::from_vorbis_comments(
                    comments
                        .comments
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                );
                Self {
                    format,
                    sample_rate: stream.sample_rate(),
                    bits_per_sample: LOSSY_BITS,
                    is_float: true,
                    metadata,
                    inner: Inner::Vorbis(Box::new(stream)),
                }
            }
            FileFormat::Mp3 => {
                let metadata = mp3::read_tags(&mut reader)?;
                let stream = Mp3Reader::new(reader).map_err(FormatError::Mp3)?;
                Self {
                    format,
                    sample_rate: stream.sample_rate(),
                    bits_per_sample: LOSSY_BITS,
                    is_float: true,
                    metadata,
                    inner: Inner::Mp3(Box::new(stream)),
                }
            }
        })
    }

    /// What the file says beside its audio: tags, markers, loops, pictures,
    /// and the chunks this crate keeps without interpreting them. The
    /// [`metadata`](crate::metadata) module says where each format keeps
    /// each field.
    ///
    /// It is read when the file is opened and kept for as long as the
    /// reader lives: at most 64 MiB, and far less in a real file unless it
    /// holds large pictures. [`take_metadata`](Self::take_metadata) hands
    /// it over and frees it.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Hands over the metadata, leaving it empty here.
    pub fn take_metadata(&mut self) -> Metadata {
        std::mem::take(&mut self.metadata)
    }

    /// The format the file is in.
    pub fn format(&self) -> FileFormat {
        self.format
    }

    /// Sample rate in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of channels. Never zero.
    pub fn num_channels(&self) -> usize {
        match &self.inner {
            Inner::Wav(stream) => stream.num_channels(),
            Inner::Aiff(stream) => stream.num_channels(),
            Inner::Flac(stream) => stream.num_channels(),
            Inner::Vorbis(stream) => stream.num_channels(),
            Inner::Mp3(stream) => stream.num_channels(),
        }
    }

    /// Bit depth the samples are stored with.
    pub fn bits_per_sample(&self) -> u16 {
        self.bits_per_sample
    }

    /// Whether the samples are stored as floating point.
    pub fn is_float(&self) -> bool {
        self.is_float
    }

    /// Length of the audio in frames, when the file says.
    ///
    /// WAV and AIFF always do, Ogg Vorbis in the granule position of its
    /// last page, and MP3 in a Xing or VBRI header. A FLAC stream written by
    /// an encoder that could not go back to fill in its header does not,
    /// and an MP3 stream without such a header does not; the length is then
    /// what [`seek`](Self::seek) returns for a frame past any end, such as
    /// `u64::MAX`.
    pub fn len_frames(&self) -> Option<u64> {
        match &self.inner {
            Inner::Wav(stream) => Some(stream.frames()),
            Inner::Aiff(stream) => Some(stream.frames()),
            Inner::Flac(stream) => stream.frames(),
            Inner::Vorbis(stream) => stream.frames(),
            Inner::Mp3(stream) => stream.frames(),
        }
    }

    /// The frame the next [`read`](Self::read) starts at.
    pub fn position(&self) -> u64 {
        match &self.inner {
            Inner::Wav(stream) => stream.position(),
            Inner::Aiff(stream) => stream.position(),
            Inner::Flac(stream) => stream.position(),
            Inner::Vorbis(stream) => stream.position(),
            Inner::Mp3(stream) => stream.position(),
        }
    }

    /// Reads the next frames into `buffer` and returns how many were read.
    ///
    /// As many frames as `buffer` has are read, so a block of any size is
    /// asked for with [`AudioBuffer::set_num_frames`]. Fewer come back only
    /// at the end of the audio, and zero once it has all been read. Frames
    /// of `buffer` past the count returned are left as they were. Integer
    /// full scale is mapped to `±1.0`.
    ///
    /// What is read does not depend on how it is cut into blocks: the same
    /// frames come back bit for bit whatever the block sizes.
    ///
    /// If this fails, some frames may already have been put in `buffer` and
    /// the position is where reading stopped, which in a damaged FLAC file
    /// is the start of the frame that could not be decoded. Reading again
    /// tries the same place again. [`seek`](Self::seek) back to try the
    /// whole block again, or forward to carry on after the damage.
    ///
    /// # Panics
    ///
    /// Panics if `buffer` does not have exactly
    /// [`num_channels`](Self::num_channels) channels.
    pub fn read(&mut self, buffer: &mut AudioBuffer<f32>) -> Result<usize, FormatError> {
        assert_eq!(
            buffer.num_channels(),
            self.num_channels(),
            "the buffer must have as many channels as the file"
        );
        match &mut self.inner {
            Inner::Wav(stream) => Ok(stream.read(buffer)?),
            Inner::Aiff(stream) => Ok(stream.read(buffer)?),
            Inner::Flac(stream) => stream.read(buffer).map_err(FormatError::Flac),
            Inner::Vorbis(stream) => stream.read(buffer).map_err(FormatError::Vorbis),
            Inner::Mp3(stream) => stream.read(buffer).map_err(FormatError::Mp3),
        }
    }

    /// Moves to `frame`, so that the next [`read`](Self::read) starts
    /// exactly there, and returns the new position.
    ///
    /// A frame past the end moves to the end, which is not an error: the
    /// position returned is then the length of the audio.
    ///
    /// In WAV and AIFF this costs nothing until the next read. In FLAC the
    /// frame is found through the file's seek table when it has one, and
    /// otherwise by bisection: the middle of the file is searched for the
    /// start of a frame, whose header says which samples it holds, until the
    /// search has closed in on the frame wanted. A seek table is not
    /// trusted: a point that does not lead to a frame is ignored.
    ///
    /// Audio can contain bytes that look like the start of a frame, so a
    /// frame is taken for one only if it decodes whole, with both its
    /// checksums right, and is followed by a frame that carries the next
    /// number or by the end of the file. The limits of that are among the
    /// limits of this type.
    ///
    /// The search stops a few blocks short of the frame wanted, counted in
    /// samples, and decodes from there. It decodes two frames for each
    /// halving of the file: some thirty for half an hour of digital
    /// silence, which is the case with the most frames to the byte.
    ///
    /// In Ogg Vorbis the search is a bisection over the pages, which say
    /// how many samples are finished by their end; from the page found,
    /// the block sizes of the packets after it lead to the packet that
    /// holds the frame wanted, and that packet and the one before it, whose
    /// overlap it needs, are all that is decoded. A damaged page on the way
    /// is stepped over.
    ///
    /// MP3 frames carry no number, so they are counted: the first time a
    /// seek goes past a place, by reading the header of each frame on the
    /// way, four bytes a frame, remembering where every so many frames
    /// start. From there a seek reads the frames whose main data the bit
    /// reservoir reaches back into, and decodes the two granules before the
    /// frame wanted, which leave the decoder as a decode from the start
    /// would. A stream without a Xing or VBRI header states no length, and
    /// a seek past the frames counted so far counts on to the end if it
    /// has to.
    ///
    /// Whatever the format, what is read after a seek is, bit for bit, what
    /// reading from the start gives at that place.
    ///
    /// If this fails, the position is unchanged.
    pub fn seek(&mut self, frame: u64) -> Result<u64, FormatError> {
        match &mut self.inner {
            Inner::Wav(stream) => Ok(stream.seek(frame)),
            Inner::Aiff(stream) => Ok(stream.seek(frame)),
            Inner::Flac(stream) => stream.seek(frame).map_err(FormatError::Flac),
            Inner::Vorbis(stream) => stream.seek(frame).map_err(FormatError::Vorbis),
            Inner::Mp3(stream) => stream.seek(frame).map_err(FormatError::Mp3),
        }
    }

    /// Gives back the underlying reader.
    pub fn into_inner(self) -> R {
        match self.inner {
            Inner::Wav(stream) => stream.into_inner(),
            Inner::Aiff(stream) => stream.into_inner(),
            Inner::Flac(stream) => stream.into_inner(),
            Inner::Vorbis(stream) => stream.into_inner(),
            Inner::Mp3(stream) => stream.into_inner(),
        }
    }
}

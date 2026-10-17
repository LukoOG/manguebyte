//! The CELT layer of Opus (RFC 6716, section 4.3): a transform codec of
//! band energies and normalised band shapes, with a pitch post-filter.
//!
//! This decodes as the reference implementation's floating-point build
//! does, operation for operation where the order of the arithmetic changes
//! the result, so that its output stays within the rounding of the
//! reference's. Only the 48 kHz mode exists in Opus; other output rates
//! are not offered here, as the Ogg Opus reader always decodes at 48 kHz.

mod bands;
mod mdct;
mod pvq;
mod rate;
pub(crate) mod tables;

#[cfg(test)]
pub(crate) use bands::APPROXIMATE_EXP2;
use bands::{BandScratch, FrameBands, anti_collapse, exp2, quant_all_bands};
use mdct::Imdct;
use rate::{MAX_FINE_BITS, caps, compute_allocation};
use tables::{
    BANDS, BETA_COEF, BETA_INTRA, E_MEANS, E_PROB_MODEL, EBANDS, MAX_LM, OVERLAP, POSTFILTER_TAPS,
    PRED_COEF, PREEMPHASIS, SHORT_MDCT, SMALL_ENERGY_ICDF, SPREAD_ICDF, TAPSET_ICDF, TF_SELECT,
    TRIM_ICDF,
};

use super::range::{BITRES, RangeDecoder};

/// Samples of output history kept for the post-filter.
const DECODE_BUFFER: usize = 2_048;
/// The shortest period of the post-filter.
const COMBFILTER_MIN_PERIOD: i32 = 15;
/// The spread when the frame codes none.
const SPREAD_NORMAL: usize = 2;

/// The longest frame, 20 ms at 48 kHz.
pub(crate) const MAX_FRAME: usize = SHORT_MDCT << MAX_LM;

/// Why a CELT frame could not be decoded. The caller turns this into the
/// error of its format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CeltError {
    /// The frame size is not 2.5, 5, 10 or 20 ms.
    FrameSize,
    /// The frame read past its end: it was damaged.
    Overrun,
}

/// Decodes the CELT frames of one stream of one or two channels. It holds
/// only the state that runs from frame to frame, so that a copy of it
/// resumes the decoding where it was taken; the work space is in
/// [`CeltScratch`].
#[derive(Debug, Clone)]
pub(crate) struct CeltDecoder {
    /// Channels of output.
    channels: usize,
    /// Channels the frames code: 1 or 2, set for each frame.
    stream_channels: usize,
    /// The bands decoded: `start` is 17 in a hybrid frame, `end` follows
    /// the bandwidth.
    start: usize,
    end: usize,
    /// For each channel, the output history and the overlap of the next
    /// block (`decode_mem`).
    decode_mem: Vec<f32>,
    old_band_e: [f32; 2 * BANDS],
    old_log_e: [f32; 2 * BANDS],
    old_log_e2: [f32; 2 * BANDS],
    background_log_e: [f32; 2 * BANDS],
    preemph_mem: [f32; 2],
    postfilter_period: i32,
    postfilter_period_old: i32,
    postfilter_gain: f32,
    postfilter_gain_old: f32,
    postfilter_tapset: usize,
    postfilter_tapset_old: usize,
    /// The seed of the noise, carried from the range of the last frame.
    rng: u32,
}

/// The work space of [`CeltDecoder::decode`], which keeps nothing from one
/// frame to the next and may be shared by the decoders of several streams.
pub(crate) struct CeltScratch {
    imdct: Imdct,
    bands: BandScratch,
    x: Vec<f32>,
    freq: Vec<f32>,
}

impl CeltScratch {
    pub(crate) fn new() -> Self {
        Self {
            imdct: Imdct::new(),
            bands: BandScratch::new(),
            x: vec![0.0; 2 * MAX_FRAME],
            freq: vec![0.0; MAX_FRAME],
        }
    }
}

impl CeltDecoder {
    pub(crate) fn new(channels: usize) -> Self {
        let mut decoder = Self {
            channels,
            stream_channels: channels,
            start: 0,
            end: BANDS,
            decode_mem: vec![0.0; channels * (DECODE_BUFFER + OVERLAP)],
            old_band_e: [0.0; 2 * BANDS],
            old_log_e: [0.0; 2 * BANDS],
            old_log_e2: [0.0; 2 * BANDS],
            background_log_e: [0.0; 2 * BANDS],
            preemph_mem: [0.0; 2],
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            rng: 0,
        };
        decoder.reset();
        decoder
    }

    /// Forgets everything, as at the start of a stream
    /// (`OPUS_RESET_STATE`).
    pub(crate) fn reset(&mut self) {
        self.decode_mem.fill(0.0);
        self.old_band_e = [0.0; 2 * BANDS];
        self.old_log_e = [-28.0; 2 * BANDS];
        self.old_log_e2 = [-28.0; 2 * BANDS];
        self.background_log_e = [0.0; 2 * BANDS];
        self.preemph_mem = [0.0; 2];
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
        self.rng = 0;
    }

    pub(crate) fn set_start_band(&mut self, start: usize) {
        self.start = start;
    }

    pub(crate) fn set_end_band(&mut self, end: usize) {
        self.end = end;
    }

    pub(crate) fn set_stream_channels(&mut self, channels: usize) {
        self.stream_channels = channels;
    }

    /// The window of the overlap, which the Opus layer fades with.
    pub(crate) fn window(&self) -> &'static [f32; OVERLAP] {
        mdct::window()
    }

    /// Decodes a frame of `frame_size` samples from `ec`, a frame of `len`
    /// bytes (`celt_decode_with_ec`), into `out`, which holds `stride`
    /// samples per channel, one channel after the other. With `accumulate`
    /// the samples are added to what `out` holds.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode(
        &mut self,
        scratch: &mut CeltScratch,
        ec: &mut RangeDecoder<'_>,
        len: usize,
        frame_size: usize,
        out: &mut [f32],
        stride: usize,
        accumulate: bool,
    ) -> Result<usize, CeltError> {
        let lm = match frame_size {
            120 => 0,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => return Err(CeltError::FrameSize),
        };
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let cc = self.channels;
        let c = self.stream_channels;
        let start = self.start;
        let end = self.end;

        if c == 1 {
            for band in 0..BANDS {
                self.old_band_e[band] = self.old_band_e[band].max(self.old_band_e[BANDS + band]);
            }
        }

        let total_bits = (len * 8) as i32;
        let mut tell = ec.tell();
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            ec.bit_logp(15)
        } else {
            false
        };
        if silence {
            // As if every bit had been read.
            ec.skip_to_end();
            tell = total_bits;
        }

        let mut postfilter_gain = 0.0f32;
        let mut postfilter_pitch = 0i32;
        let mut postfilter_tapset = 0usize;
        if start == 0 && tell + 16 <= total_bits {
            if ec.bit_logp(1) {
                let octave = ec.uint(6);
                postfilter_pitch = (16 << octave) + ec.bits(4 + octave) as i32 - 1;
                let qg = ec.bits(3);
                if ec.tell() + 2 <= total_bits {
                    postfilter_tapset = ec.icdf(&TAPSET_ICDF, 2);
                }
                postfilter_gain = 0.093_75 * (qg + 1) as f32;
            }
            tell = ec.tell();
        }

        let transient = if lm > 0 && tell + 3 <= total_bits {
            let set = ec.bit_logp(3);
            tell = ec.tell();
            set
        } else {
            false
        };
        let short_blocks = transient;
        let intra = if tell + 3 <= total_bits {
            ec.bit_logp(3)
        } else {
            false
        };

        self.unquant_coarse_energy(start, end, intra, ec, c, lm);

        let mut tf_res = [0i32; BANDS];
        tf_decode(start, end, transient, &mut tf_res, lm, ec);

        let tell = ec.tell();
        let spread = if tell + 4 <= total_bits {
            ec.icdf(&SPREAD_ICDF, 5)
        } else {
            SPREAD_NORMAL
        };

        let mut cap = [0i32; BANDS];
        caps(lm, c, &mut cap);

        // The boosts of individual bands (dynalloc).
        let mut offsets = [0i32; BANDS];
        let mut dynalloc_logp = 6;
        let mut total_frac = total_bits << BITRES;
        let mut tell_frac = ec.tell_frac();
        for band in start..end {
            let width = (c * (EBANDS[band + 1] - EBANDS[band])) << lm;
            let width = width as i32;
            // Six bits, but no more than one a bin and no less than an
            // eighth of a bit a bin.
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            while tell_frac + ((loop_logp as i32) << BITRES) < total_frac && boost < cap[band] {
                let flag = ec.bit_logp(loop_logp);
                tell_frac = ec.tell_frac();
                if !flag {
                    break;
                }
                boost += quanta;
                total_frac -= quanta;
                loop_logp = 1;
            }
            offsets[band] = boost;
            if boost > 0 {
                dynalloc_logp = (dynalloc_logp - 1).max(2);
            }
        }

        let alloc_trim = if tell_frac + (6 << BITRES) <= total_frac {
            ec.icdf(&TRIM_ICDF, 7) as i32
        } else {
            5
        };

        let mut bits = (((len * 8) as i32) << BITRES) - ec.tell_frac() - 1;
        let anti_collapse_rsv = if transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;

        let allocation =
            compute_allocation(start, end, &offsets, &cap, alloc_trim, bits, c, lm, ec);

        self.unquant_fine_energy(start, end, &allocation.fine_quant, ec, c);

        // Make room for the new block.
        for channel in 0..cc {
            let mem = &mut self.decode_mem
                [channel * (DECODE_BUFFER + OVERLAP)..(channel + 1) * (DECODE_BUFFER + OVERLAP)];
            mem.copy_within(n..DECODE_BUFFER + OVERLAP / 2, 0);
        }

        // The shapes.
        let mut collapse_masks = [0u8; 2 * BANDS];
        let frame = FrameBands {
            start,
            end,
            channels: c,
            lm,
            short_blocks,
            spread,
            dual_stereo: allocation.dual_stereo,
            intensity: allocation.intensity,
            tf_res: &tf_res,
            pulses: &allocation.pulses,
            total_bits: ((len * 8) as i32 * (1 << BITRES)) - anti_collapse_rsv,
            balance: allocation.balance,
            coded_bands: allocation.coded_bands,
        };
        let x = &mut scratch.x[..c * n];
        x.fill(0.0);
        let mut x_full = std::mem::take(&mut scratch.x);
        quant_all_bands(
            &frame,
            &mut x_full[..2 * n],
            &mut collapse_masks,
            ec,
            &mut self.rng,
            &mut scratch.bands,
        );

        let anti_collapse_on = anti_collapse_rsv > 0 && ec.bits(1) != 0;

        let bits_left = (len * 8) as i32 - ec.tell();
        self.unquant_energy_finalise(
            start,
            end,
            &allocation.fine_quant,
            &allocation.fine_priority,
            bits_left,
            ec,
            c,
        );

        if anti_collapse_on {
            anti_collapse(
                &mut x_full,
                &collapse_masks,
                lm,
                c,
                n,
                start,
                end,
                &self.old_band_e,
                &self.old_log_e,
                &self.old_log_e2,
                &allocation.pulses,
                self.rng,
            );
        }

        if silence {
            self.old_band_e = [-28.0; 2 * BANDS];
        }

        self.synthesis(
            scratch,
            &mut x_full,
            start,
            end.min(BANDS),
            c,
            cc,
            transient,
            lm,
            silence,
        );
        scratch.x = x_full;

        // The post-filter, across the first short block from the old
        // parameters to the current ones, and over the rest from the
        // current ones to the new.
        for channel in 0..cc {
            self.postfilter_period = self.postfilter_period.max(COMBFILTER_MIN_PERIOD);
            self.postfilter_period_old = self.postfilter_period_old.max(COMBFILTER_MIN_PERIOD);
            let base = channel * (DECODE_BUFFER + OVERLAP) + DECODE_BUFFER - n;
            let window = mdct::window();
            comb_filter(
                &mut self.decode_mem,
                base,
                self.postfilter_period_old,
                self.postfilter_period,
                SHORT_MDCT,
                self.postfilter_gain_old,
                self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
                window,
            );
            if lm != 0 {
                comb_filter(
                    &mut self.decode_mem,
                    base + SHORT_MDCT,
                    self.postfilter_period,
                    postfilter_pitch,
                    n - SHORT_MDCT,
                    self.postfilter_gain,
                    postfilter_gain,
                    self.postfilter_tapset,
                    postfilter_tapset,
                    window,
                );
            }
        }
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter_pitch;
        self.postfilter_gain = postfilter_gain;
        self.postfilter_tapset = postfilter_tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        if c == 1 {
            let (first, second) = self.old_band_e.split_at_mut(BANDS);
            second.copy_from_slice(first);
        }

        if !transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
            // The noise floor rises by at most 2.4 dB a second.
            let increase = m as f32 * 0.001;
            for (background, &energy) in self.background_log_e.iter_mut().zip(&self.old_band_e) {
                *background = (*background + increase).min(energy);
            }
        } else {
            for (log, &energy) in self.old_log_e.iter_mut().zip(&self.old_band_e) {
                *log = log.min(energy);
            }
        }
        for channel in 0..2 {
            for band in (0..start).chain(end..BANDS) {
                self.old_band_e[channel * BANDS + band] = 0.0;
                self.old_log_e[channel * BANDS + band] = -28.0;
                self.old_log_e2[channel * BANDS + band] = -28.0;
            }
        }
        self.rng = ec.range();

        self.deemphasis(n, out, stride, accumulate);
        if ec.tell() > (len * 8) as i32 {
            return Err(CeltError::Overrun);
        }
        Ok(frame_size)
    }

    /// The coarse energy of each band, in steps of 6 dB, predicted from the
    /// last frame and the band below (`unquant_coarse_energy`, section
    /// 4.3.2.1).
    #[allow(clippy::needless_range_loop)]
    fn unquant_coarse_energy(
        &mut self,
        start: usize,
        end: usize,
        intra: bool,
        ec: &mut RangeDecoder<'_>,
        channels: usize,
        lm: usize,
    ) {
        let model = &E_PROB_MODEL[lm][usize::from(intra)];
        let mut prev = [0.0f32; 2];
        let (coef, beta) = if intra {
            (0.0, BETA_INTRA)
        } else {
            (PRED_COEF[lm], BETA_COEF[lm])
        };
        let budget = (ec.storage() * 8) as i32;
        for band in start..end {
            for c in 0..channels {
                let tell = ec.tell();
                let qi = if budget - tell >= 15 {
                    let at = 2 * band.min(20);
                    laplace_decode(ec, u32::from(model[at]) << 7, u32::from(model[at + 1]) << 6)
                } else if budget - tell >= 2 {
                    let symbol = ec.icdf(&SMALL_ENERGY_ICDF, 2) as i32;
                    (symbol >> 1) ^ -(symbol & 1)
                } else if budget - tell >= 1 {
                    -i32::from(ec.bit_logp(1))
                } else {
                    -1
                };
                let q = qi as f32;
                let old = &mut self.old_band_e[band + c * BANDS];
                *old = old.max(-9.0);
                let tmp = coef * *old + prev[c] + q;
                *old = tmp;
                prev[c] = prev[c] + q - beta * q;
            }
        }
    }

    /// The fine energy, in the bits the allocation gave each band
    /// (`unquant_fine_energy`, section 4.3.2.2).
    #[allow(clippy::needless_range_loop)]
    fn unquant_fine_energy(
        &mut self,
        start: usize,
        end: usize,
        fine_quant: &[i32; BANDS],
        ec: &mut RangeDecoder<'_>,
        channels: usize,
    ) {
        for band in start..end {
            let bits = fine_quant[band];
            if bits <= 0 {
                continue;
            }
            for c in 0..channels {
                let q2 = ec.bits(bits as u32);
                let offset = (q2 as f32 + 0.5) * (1 << (14 - bits)) as f32 * (1.0 / 16_384.0) - 0.5;
                self.old_band_e[band + c * BANDS] += offset;
            }
        }
    }

    /// One more bit of fine energy to the bands that the bits left at the
    /// end of the frame reach (`unquant_energy_finalise`).
    #[allow(clippy::too_many_arguments)]
    fn unquant_energy_finalise(
        &mut self,
        start: usize,
        end: usize,
        fine_quant: &[i32; BANDS],
        fine_priority: &[i32; BANDS],
        mut bits_left: i32,
        ec: &mut RangeDecoder<'_>,
        channels: usize,
    ) {
        for priority in 0..2 {
            let mut band = start;
            while band < end && bits_left >= channels as i32 {
                if fine_quant[band] >= MAX_FINE_BITS || fine_priority[band] != priority {
                    band += 1;
                    continue;
                }
                for c in 0..channels {
                    let q2 = ec.bits(1);
                    let offset = (q2 as f32 - 0.5)
                        * (1 << (14 - fine_quant[band] - 1)) as f32
                        * (1.0 / 16_384.0);
                    self.old_band_e[band + c * BANDS] += offset;
                    bits_left -= 1;
                }
                band += 1;
            }
        }
    }

    /// The bands scaled by their energies and transformed back to time
    /// (`celt_synthesis`), into the history of each channel.
    #[allow(clippy::too_many_arguments)]
    fn synthesis(
        &mut self,
        scratch: &mut CeltScratch,
        x: &mut [f32],
        start: usize,
        end: usize,
        c: usize,
        cc: usize,
        transient: bool,
        lm: usize,
        silence: bool,
    ) {
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let (blocks, nb, shift) = if transient {
            (m, SHORT_MDCT, MAX_LM)
        } else {
            (1, n, MAX_LM - lm)
        };
        let stride = DECODE_BUFFER + OVERLAP;
        let out_at = DECODE_BUFFER - n;
        if cc == 2 && c == 1 {
            // A mono frame to two channels.
            denormalise_bands(
                &x[..n],
                &mut scratch.freq[..n],
                &self.old_band_e[..BANDS],
                start,
                end,
                m,
                silence,
            );
            for channel in 0..2 {
                let mem = &mut self.decode_mem[channel * stride..(channel + 1) * stride];
                for b in 0..blocks {
                    scratch.imdct.backward(
                        &scratch.freq[b..],
                        blocks,
                        &mut mem[out_at + nb * b..],
                        shift,
                    );
                }
            }
        } else if cc == 1 && c == 2 {
            // A stereo frame mixed down to one channel.
            denormalise_bands(
                &x[..n],
                &mut scratch.freq[..n],
                &self.old_band_e[..BANDS],
                start,
                end,
                m,
                silence,
            );
            let (first, second) = x.split_at_mut(n);
            denormalise_bands(
                &second[..n],
                &mut first[..n],
                &self.old_band_e[BANDS..],
                start,
                end,
                m,
                silence,
            );
            for (f, &other) in scratch.freq[..n].iter_mut().zip(&first[..n]) {
                *f = 0.5 * *f + 0.5 * other;
            }
            let mem = &mut self.decode_mem[..stride];
            for b in 0..blocks {
                scratch.imdct.backward(
                    &scratch.freq[b..],
                    blocks,
                    &mut mem[out_at + nb * b..],
                    shift,
                );
            }
        } else {
            for channel in 0..c {
                denormalise_bands(
                    &x[channel * n..(channel + 1) * n],
                    &mut scratch.freq[..n],
                    &self.old_band_e[channel * BANDS..(channel + 1) * BANDS],
                    start,
                    end,
                    m,
                    silence,
                );
                let mem = &mut self.decode_mem[channel * stride..(channel + 1) * stride];
                for b in 0..blocks {
                    scratch.imdct.backward(
                        &scratch.freq[b..],
                        blocks,
                        &mut mem[out_at + nb * b..],
                        shift,
                    );
                }
            }
        }
    }

    /// Undoes the pre-emphasis and hands the samples out (`deemphasis`).
    fn deemphasis(&mut self, n: usize, out: &mut [f32], stride: usize, accumulate: bool) {
        let mem_stride = DECODE_BUFFER + OVERLAP;
        for channel in 0..self.channels {
            let input = &self.decode_mem
                [channel * mem_stride + DECODE_BUFFER - n..channel * mem_stride + DECODE_BUFFER];
            let output = &mut out[channel * stride..channel * stride + n];
            let mut memory = self.preemph_mem[channel];
            for (y, &x) in output.iter_mut().zip(input) {
                let tmp = x + 1e-30 + memory;
                memory = PREEMPHASIS * tmp;
                let sample = tmp * (1.0 / 32_768.0);
                if accumulate {
                    *y += sample;
                } else {
                    *y = sample;
                }
            }
            self.preemph_mem[channel] = memory;
        }
    }
}

/// The factor of an output gain given in 1/256 dB, worked out as the
/// reference does (`OPUS_SET_GAIN`).
pub(crate) fn exp2_gain(gain_q8: i16) -> f32 {
    if gain_q8 == 0 {
        1.0
    } else {
        exp2(6.488_141e-4 * f32::from(gain_q8))
    }
}

/// Reads a value of the Laplace distribution that the coarse energy is
/// coded with (`ec_laplace_decode`): `fs` is the probability of zero in
/// 1/32768, `decay` how fast the rest falls off.
fn laplace_decode(ec: &mut RangeDecoder<'_>, fs: u32, decay: u32) -> i32 {
    const MIN_P: u32 = 1;
    const N_MIN: u32 = 16;
    let mut value = 0i32;
    let fm = ec.decode_bin(15);
    let mut fl = 0u32;
    let mut fs = fs;
    if fm >= fs {
        value += 1;
        fl = fs;
        // The probability of plus or minus one.
        let ft = 32_768 - MIN_P * (2 * N_MIN) - fs;
        fs = ((ft * (16_384 - decay)) >> 15) + MIN_P;
        // The decaying part.
        while fs > MIN_P && fm >= fl + 2 * fs {
            fs *= 2;
            fl += fs;
            fs = (((fs - 2 * MIN_P) * decay) >> 15) + MIN_P;
            value += 1;
        }
        // Beyond it, every value has the least probability.
        if fs <= MIN_P {
            let di = (fm - fl) >> 1;
            value += di as i32;
            fl += 2 * di * MIN_P;
        }
        if fm < fl + fs {
            value = -value;
        } else {
            fl += fs;
        }
    }
    ec.update(fl, (fl + fs).min(32_768), 32_768);
    value
}

/// The change of time-frequency resolution of each band (`tf_decode`,
/// section 4.3.4.5).
fn tf_decode(
    start: usize,
    end: usize,
    transient: bool,
    tf_res: &mut [i32; BANDS],
    lm: usize,
    ec: &mut RangeDecoder<'_>,
) {
    let mut budget = (ec.storage() * 8) as i32;
    let mut tell = ec.tell();
    let mut logp: u32 = if transient { 2 } else { 4 };
    let select_rsv = lm > 0 && tell + (logp as i32) < budget;
    budget -= i32::from(select_rsv);
    let mut changed = 0;
    let mut current = 0;
    for slot in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp as i32 <= budget {
            current ^= i32::from(ec.bit_logp(logp));
            tell = ec.tell();
            changed |= current;
        }
        *slot = current;
        logp = if transient { 4 } else { 5 };
    }
    let row = &TF_SELECT[lm];
    let t = 4 * usize::from(transient);
    let mut select = 0;
    if select_rsv && row[t + changed as usize] != row[t + 2 + changed as usize] {
        select = usize::from(ec.bit_logp(1));
    }
    for slot in tf_res.iter_mut().take(end).skip(start) {
        *slot = i32::from(row[t + 2 * select + *slot as usize]);
    }
}

/// Each band's shape times its gain, `2^(energy + mean)`
/// (`denormalise_bands`).
fn denormalise_bands(
    x: &[f32],
    freq: &mut [f32],
    log_e: &[f32],
    start: usize,
    end: usize,
    m: usize,
    silence: bool,
) {
    let n = freq.len();
    let (start, end) = if silence { (0, 0) } else { (start, end) };
    let bound = if silence { 0 } else { m * EBANDS[end] };
    freq[..m * EBANDS[start]].fill(0.0);
    for band in start..end {
        let lg = log_e[band] + E_MEANS[band];
        let g = exp2(lg.min(32.0));
        for j in m * EBANDS[band]..m * EBANDS[band + 1] {
            freq[j] = x[j] * g;
        }
    }
    freq[bound.min(n)..].fill(0.0);
}

/// The pitch post-filter (`comb_filter`, section 4.3.7.1), in place over
/// `n` samples of `mem` from `at`, with the history before `at`: a comb
/// of three taps at the period, faded over the overlap from the old
/// period, gain and taps to the new ones.
#[allow(clippy::too_many_arguments)]
fn comb_filter(
    mem: &mut [f32],
    at: usize,
    t0: i32,
    t1: i32,
    n: usize,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
    window: &[f32; OVERLAP],
) {
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }
    let t0 = t0.max(COMBFILTER_MIN_PERIOD) as usize;
    let t1 = t1.max(COMBFILTER_MIN_PERIOD) as usize;
    let g00 = g0 * POSTFILTER_TAPS[tapset0][0];
    let g01 = g0 * POSTFILTER_TAPS[tapset0][1];
    let g02 = g0 * POSTFILTER_TAPS[tapset0][2];
    let g10 = g1 * POSTFILTER_TAPS[tapset1][0];
    let g11 = g1 * POSTFILTER_TAPS[tapset1][1];
    let g12 = g1 * POSTFILTER_TAPS[tapset1][2];
    // The history read reaches back the period and two taps more.
    let mut x1 = mem[at + 1 - t1];
    let mut x2 = mem[at - t1];
    let mut x3 = mem[at - t1 - 1];
    let mut x4 = mem[at - t1 - 2];
    let overlap = if g0 == g1 && t0 == t1 && tapset0 == tapset1 {
        0
    } else {
        OVERLAP.min(n)
    };
    let mut i = 0;
    while i < overlap {
        let x0 = mem[at + i + 2 - t1];
        let f = window[i] * window[i];
        let one_f = 1.0 - f;
        let y = mem[at + i]
            + (one_f * g00) * mem[at + i - t0]
            + (one_f * g01) * (mem[at + i + 1 - t0] + mem[at + i - t0 - 1])
            + (one_f * g02) * (mem[at + i + 2 - t0] + mem[at + i - t0 - 2])
            + (f * g10) * x2
            + (f * g11) * (x1 + x3)
            + (f * g12) * (x0 + x4);
        mem[at + i] = y;
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
        i += 1;
    }
    if g1 == 0.0 {
        return;
    }
    // The constant filter for the rest.
    let at = at + i;
    let mut x4 = mem[at - t1 - 2];
    let mut x3 = mem[at - t1 - 1];
    let mut x2 = mem[at - t1];
    let mut x1 = mem[at + 1 - t1];
    for j in 0..n - i {
        let x0 = mem[at + j + 2 - t1];
        let y = mem[at + j] + g10 * x2 + g11 * (x1 + x3) + g12 * (x0 + x4);
        mem[at + j] = y;
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
}

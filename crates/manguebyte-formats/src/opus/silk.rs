//! The SILK layer of Opus (RFC 6716, section 4.2): a linear predictive
//! codec, with long-term (pitch) prediction, of 8, 12 or 16 kHz audio.
//!
//! SILK is decoded in fixed point, bit for bit as the RFC and its
//! reference implementation have it, so the samples it gives are the
//! reference's; they are then resampled to 48 kHz by the resampler of
//! [`resampler`], which is built as the reference's is. Packet loss
//! concealment and comfort noise, which only change the output of frames
//! that were lost, are not here.

mod lpc;
mod resampler;
mod tables;

use lpc::{MAX_ORDER, decode_nlsf, inverse32_var_q, lshift_sat32, nlsf_to_lpc, smulwb, smulww};
use resampler::Resampler;
use tables::*;

use super::range::RangeDecoder;

/// The longest SILK frame, 20 ms at 16 kHz, and its longest subframe.
const MAX_FRAME_LENGTH: usize = 320;
const MAX_SUBFRAME_LENGTH: usize = 80;
/// The output history kept for the long-term predictor.
const OUT_BUF_LENGTH: usize = MAX_FRAME_LENGTH + 2 * MAX_SUBFRAME_LENGTH;
const LTP_ORDER: usize = 5;

const TYPE_NO_VOICE_ACTIVITY: usize = 0;
const TYPE_VOICED: usize = 2;

/// How a frame's parameters are coded: on their own, against the frame
/// before, or on their own but without the LTP scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coding {
    Independent,
    IndependentNoLtpScaling,
    Conditional,
}

/// The symbols of one frame (`SideInfoIndices`).
#[derive(Debug, Clone, Copy, Default)]
struct Indices {
    gains: [i32; 4],
    lsf: [i32; MAX_ORDER + 1],
    lag: i32,
    contour: usize,
    signal_type: usize,
    quant_offset_type: usize,
    lsf_interp_q2: i32,
    periodicity: usize,
    ltp: [usize; 4],
    ltp_scale: usize,
    seed: u32,
}

/// The decoder of one channel (`silk_decoder_state`).
#[derive(Debug, Clone)]
struct Channel {
    /// The internal rate in kHz, or 0 before the first frame.
    fs_khz: usize,
    nb_subfr: usize,
    frames_per_packet: usize,
    frame_length: usize,
    subfr_length: usize,
    ltp_mem_length: usize,
    lpc_order: usize,
    prev_gain_q16: i32,
    exc_q14: [i32; MAX_FRAME_LENGTH],
    slpc_q14: [i32; MAX_ORDER],
    out_buf: [i16; OUT_BUF_LENGTH],
    lag_prev: i32,
    last_gain_index: i32,
    prev_nlsf_q15: [i32; MAX_ORDER],
    first_frame_after_reset: bool,
    ec_prev_signal_type: usize,
    ec_prev_lag_index: i32,
    vad_flags: [bool; 3],
    lbrr_flag: bool,
    lbrr_flags: [bool; 3],
    frames_decoded: usize,
    prev_signal_type: usize,
    resampler: Resampler,
    resampler_khz: usize,
    indices: Indices,
}

impl Channel {
    /// A channel with nothing decoded (`silk_init_decoder`).
    fn new() -> Self {
        Self {
            fs_khz: 0,
            nb_subfr: 0,
            frames_per_packet: 0,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_gain_q16: 65_536,
            exc_q14: [0; MAX_FRAME_LENGTH],
            slpc_q14: [0; MAX_ORDER],
            out_buf: [0; OUT_BUF_LENGTH],
            lag_prev: 0,
            last_gain_index: 0,
            prev_nlsf_q15: [0; MAX_ORDER],
            first_frame_after_reset: true,
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            vad_flags: [false; 3],
            lbrr_flag: false,
            lbrr_flags: [false; 3],
            frames_decoded: 0,
            prev_signal_type: 0,
            resampler: Resampler::new(16_000),
            resampler_khz: 0,
            indices: Indices::default(),
        }
    }

    /// Sets the internal rate and frame size (`silk_decoder_set_fs`).
    fn set_fs(&mut self, fs_khz: usize) {
        self.subfr_length = 5 * fs_khz;
        let frame_length = self.nb_subfr * self.subfr_length;
        if self.resampler_khz != fs_khz {
            self.resampler = Resampler::new(fs_khz as u32 * 1_000);
            self.resampler_khz = fs_khz;
        }
        if self.fs_khz != fs_khz || frame_length != self.frame_length {
            if self.fs_khz != fs_khz {
                self.ltp_mem_length = 20 * fs_khz;
                self.lpc_order = if fs_khz == 16 { 16 } else { 10 };
                self.first_frame_after_reset = true;
                self.lag_prev = 100;
                self.last_gain_index = 10;
                self.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
                self.out_buf = [0; OUT_BUF_LENGTH];
                self.slpc_q14 = [0; MAX_ORDER];
            }
            self.fs_khz = fs_khz;
            self.frame_length = frame_length;
        }
    }
}

/// The state of the stereo prediction (`stereo_dec_state`).
#[derive(Debug, Clone, Copy, Default)]
struct Stereo {
    pred_prev_q13: [i32; 2],
    mid: [i16; 2],
    side: [i16; 2],
}

/// Why a SILK frame could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SilkError {
    /// The frame asks for something SILK has no such thing as.
    Malformed,
}

/// Decodes the SILK frames of a stream of one or two channels
/// (`silk_decoder`): the state that runs from frame to frame, the work
/// space being in [`SilkScratch`].
#[derive(Debug, Clone)]
pub(crate) struct SilkDecoder {
    channels: [Box<Channel>; 2],
    stereo: Stereo,
    /// Channels of output.
    api_channels: usize,
    /// Channels the frames last coded.
    internal_channels: usize,
    prev_decode_only_middle: bool,
}

/// The work space of [`SilkDecoder::decode`]: one frame of each channel at
/// the internal rate, two samples ahead of the frame for the stereo
/// unmixing's history.
pub(crate) struct SilkScratch {
    work: [Vec<i16>; 2],
}

impl SilkScratch {
    pub(crate) fn new() -> Self {
        Self {
            work: [vec![0; WORK_LENGTH], vec![0; WORK_LENGTH]],
        }
    }
}

/// The output of each channel at the internal rate, two samples ahead of
/// the frame for the stereo prediction's history.
const WORK_LENGTH: usize = MAX_FRAME_LENGTH + 2;

impl SilkDecoder {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            channels: [Box::new(Channel::new()), Box::new(Channel::new())],
            stereo: Stereo::default(),
            api_channels: channels,
            internal_channels: 0,
            prev_decode_only_middle: false,
        }
    }

    /// Forgets everything (`silk_ResetDecoder`).
    pub(crate) fn reset(&mut self) {
        for channel in &mut self.channels {
            **channel = Channel::new();
        }
        self.stereo = Stereo::default();
        self.prev_decode_only_middle = false;
    }

    /// Decodes the next SILK frame of 10 or 20 ms of an Opus frame of
    /// `payload_ms` (`silk_Decode`): `new_packet` for the first of the Opus
    /// frame, whose header is read first. The output, at 48 kHz, goes into
    /// `out` at `stride` samples a channel. Returns the samples written per
    /// channel.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode(
        &mut self,
        scratch: &mut SilkScratch,
        ec: &mut RangeDecoder<'_>,
        new_packet: bool,
        internal_rate: u32,
        payload_ms: usize,
        internal_channels: usize,
        out: &mut [i16],
        stride: usize,
    ) -> Result<usize, SilkError> {
        if new_packet {
            for channel in &mut self.channels {
                channel.frames_decoded = 0;
            }
        }
        // The second channel starts from nothing when a stream turns
        // stereo.
        if internal_channels > self.internal_channels {
            *self.channels[1] = Channel::new();
        }
        let fs_khz = ((internal_rate >> 10) + 1) as usize;
        let stereo_to_mono = internal_channels == 1
            && self.internal_channels == 2
            && fs_khz == self.channels[0].fs_khz;

        if self.channels[0].frames_decoded == 0 {
            let (frames, subframes) = match payload_ms {
                10 => (1, 2),
                20 => (1, 4),
                40 => (2, 4),
                60 => (3, 4),
                _ => return Err(SilkError::Malformed),
            };
            for channel in self.channels.iter_mut().take(internal_channels) {
                channel.frames_per_packet = frames;
                channel.nb_subfr = subframes;
                channel.set_fs(fs_khz);
            }
        }
        if self.api_channels == 2 && internal_channels == 2 && (self.internal_channels == 1) {
            self.stereo.pred_prev_q13 = [0; 2];
            self.stereo.side = [0; 2];
            self.channels[1].resampler = self.channels[0].resampler.clone();
            self.channels[1].resampler_khz = self.channels[0].resampler_khz;
        }
        self.internal_channels = internal_channels;

        if self.channels[0].frames_decoded == 0 {
            // The VAD and LBRR flags of every frame of the packet.
            for channel in self.channels.iter_mut().take(internal_channels) {
                for flag in channel.vad_flags.iter_mut().take(channel.frames_per_packet) {
                    *flag = ec.bit_logp(1);
                }
                channel.lbrr_flag = ec.bit_logp(1);
            }
            for channel in self.channels.iter_mut().take(internal_channels) {
                channel.lbrr_flags = [false; 3];
                if channel.lbrr_flag {
                    if channel.frames_per_packet == 1 {
                        channel.lbrr_flags[0] = true;
                    } else {
                        let symbol = if channel.frames_per_packet == 2 {
                            ec.icdf(&LBRR_FLAGS_40MS, 8)
                        } else {
                            ec.icdf(&LBRR_FLAGS_60MS, 8)
                        } + 1;
                        for (i, flag) in channel.lbrr_flags.iter_mut().enumerate() {
                            *flag = (symbol >> i) & 1 != 0;
                        }
                    }
                }
            }
            // The LBRR frames, the redundancy for packets lost before
            // this one, are read past.
            let frames = self.channels[0].frames_per_packet;
            for i in 0..frames {
                for n in 0..internal_channels {
                    if !self.channels[n].lbrr_flags[i] {
                        continue;
                    }
                    if internal_channels == 2 && n == 0 {
                        let mut pred = [0; 2];
                        stereo_decode_pred(ec, &mut pred);
                        if !self.channels[1].lbrr_flags[i] {
                            ec.icdf(&MID_ONLY, 8);
                        }
                    }
                    let coding = if i > 0 && self.channels[n].lbrr_flags[i - 1] {
                        Coding::Conditional
                    } else {
                        Coding::Independent
                    };
                    let channel = &mut self.channels[n];
                    decode_indices(channel, ec, i, true, coding)?;
                    let mut pulses = [0i16; MAX_FRAME_LENGTH + 16];
                    decode_pulses(
                        ec,
                        &mut pulses,
                        channel.indices.signal_type,
                        channel.indices.quant_offset_type,
                        channel.frame_length,
                    );
                }
            }
        }

        // The stereo prediction.
        let mut ms_pred_q13 = [0i32; 2];
        let mut decode_only_middle = false;
        if internal_channels == 2 {
            stereo_decode_pred(ec, &mut ms_pred_q13);
            let index = self.channels[0].frames_decoded;
            if !self.channels[1].vad_flags[index.min(2)] {
                decode_only_middle = ec.icdf(&MID_ONLY, 8) == 1;
            }
        }
        // The side channel starts afresh after frames that coded none.
        if internal_channels == 2 && !decode_only_middle && self.prev_decode_only_middle {
            let side = &mut self.channels[1];
            side.out_buf = [0; OUT_BUF_LENGTH];
            side.slpc_q14 = [0; MAX_ORDER];
            side.lag_prev = 100;
            side.last_gain_index = 10;
            side.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
            side.first_frame_after_reset = true;
        }

        let frame_index = self.channels[0].frames_decoded;
        if frame_index >= self.channels[0].frames_per_packet {
            return Err(SilkError::Malformed);
        }
        let frame_length = self.channels[0].frame_length;
        for n in 0..internal_channels {
            if n == 0 || !decode_only_middle {
                let coding = if frame_index == 0 {
                    Coding::Independent
                } else if n > 0 && self.prev_decode_only_middle {
                    Coding::IndependentNoLtpScaling
                } else {
                    Coding::Conditional
                };
                let channel = &mut self.channels[n];
                let work = &mut scratch.work[n];
                decode_frame(
                    channel,
                    ec,
                    frame_index,
                    coding,
                    &mut work[2..2 + frame_length],
                )?;
            } else {
                scratch.work[n][2..2 + frame_length].fill(0);
            }
            self.channels[n].frames_decoded += 1;
        }

        if self.api_channels == 2 && internal_channels == 2 {
            let (mid, side) = scratch.work.split_at_mut(1);
            stereo_ms_to_lr(
                &mut self.stereo,
                &mut mid[0],
                &mut side[0],
                &ms_pred_q13,
                self.channels[0].fs_khz,
                frame_length,
            );
        } else {
            // A mono stream keeps the same one-sample delay.
            let mid = &mut scratch.work[0];
            mid[..2].copy_from_slice(&self.stereo.mid);
            self.stereo
                .mid
                .copy_from_slice(&mid[frame_length..frame_length + 2]);
        }

        let out_len = frame_length * 48 / self.channels[0].fs_khz;
        for n in 0..self.api_channels.min(internal_channels) {
            let input = &scratch.work[n][1..1 + frame_length];
            self.channels[n]
                .resampler
                .run(&mut out[n * stride..n * stride + out_len], input);
        }
        if self.api_channels == 2 && internal_channels == 1 {
            if stereo_to_mono {
                // The right channel keeps its own resampler a frame longer,
                // in case it was collapsing.
                let input = &scratch.work[0][1..1 + frame_length];
                self.channels[1]
                    .resampler
                    .run(&mut out[stride..stride + out_len], input);
            } else {
                out.copy_within(..out_len, stride);
            }
        }
        self.prev_decode_only_middle = decode_only_middle;
        Ok(out_len)
    }
}

/// The stereo prediction weights (`silk_stereo_decode_pred`).
fn stereo_decode_pred(ec: &mut RangeDecoder<'_>, pred_q13: &mut [i32; 2]) {
    let n = ec.icdf(&STEREO_STAGE1, 8);
    let mut ix = [[0usize; 3]; 2];
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for row in &mut ix {
        row[0] = ec.icdf(&UNIFORM3, 8);
        row[1] = ec.icdf(&UNIFORM5, 8);
    }
    for (row, pred) in ix.iter_mut().zip(pred_q13.iter_mut()) {
        row[0] += 3 * row[2];
        let low = STEREO_WEIGHTS_Q13[row[0]];
        let step = smulwb(STEREO_WEIGHTS_Q13[row[0] + 1] - low, 6_554);
        *pred = low + step * (2 * row[1] as i32 + 1);
    }
    pred_q13[0] -= pred_q13[1];
}

/// Mid and side to left and right (`silk_stereo_MS_to_LR`, section 4.2.8),
/// with the prediction of the side from the mid interpolated over the
/// first 8 ms. Both buffers hold two samples of history first.
fn stereo_ms_to_lr(
    state: &mut Stereo,
    x1: &mut [i16],
    x2: &mut [i16],
    pred_q13: &[i32; 2],
    fs_khz: usize,
    frame_length: usize,
) {
    x1[..2].copy_from_slice(&state.mid);
    x2[..2].copy_from_slice(&state.side);
    state
        .mid
        .copy_from_slice(&x1[frame_length..frame_length + 2]);
    state
        .side
        .copy_from_slice(&x2[frame_length..frame_length + 2]);

    let mut pred0 = state.pred_prev_q13[0];
    let mut pred1 = state.pred_prev_q13[1];
    let interp = 8 * fs_khz;
    let denom_q16 = (1i32 << 16) / interp as i32;
    let round16 = |value: i32| ((value >> 15) + 1) >> 1;
    let delta0 = round16((pred_q13[0] - state.pred_prev_q13[0]).wrapping_mul(denom_q16));
    let delta1 = round16((pred_q13[1] - state.pred_prev_q13[1]).wrapping_mul(denom_q16));
    for n in 0..frame_length {
        if n < interp {
            pred0 += delta0;
            pred1 += delta1;
        } else if n == interp {
            pred0 = pred_q13[0];
            pred1 = pred_q13[1];
        }
        let sum = (i32::from(x1[n]) + i32::from(x1[n + 2]) + (i32::from(x1[n + 1]) << 1)) << 9;
        let sum = smlawb(i32::from(x2[n + 1]) << 8, sum, pred0);
        let sum = smlawb(sum, i32::from(x1[n + 1]) << 11, pred1);
        x2[n + 1] = sat16(rshift_round(sum, 8));
    }
    state.pred_prev_q13 = *pred_q13;
    for n in 0..frame_length {
        let sum = i32::from(x1[n + 1]) + i32::from(x2[n + 1]);
        let diff = i32::from(x1[n + 1]) - i32::from(x2[n + 1]);
        x1[n + 1] = sat16(sum);
        x2[n + 1] = sat16(diff);
    }
}

#[inline]
fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwb(b, c))
}

#[inline]
fn sat16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[inline]
fn rshift_round(value: i32, shift: u32) -> i32 {
    if shift == 1 {
        (value >> 1) + (value & 1)
    } else {
        ((value >> (shift - 1)) + 1) >> 1
    }
}

/// The symbols of a frame (`silk_decode_indices`, sections 4.2.7.3 to
/// 4.2.7.7).
fn decode_indices(
    channel: &mut Channel,
    ec: &mut RangeDecoder<'_>,
    frame_index: usize,
    lbrr: bool,
    coding: Coding,
) -> Result<(), SilkError> {
    let indices = &mut channel.indices;
    let type_symbol = if lbrr || channel.vad_flags[frame_index.min(2)] {
        ec.icdf(&FRAME_TYPE_ACTIVE, 8) + 2
    } else {
        ec.icdf(&FRAME_TYPE_INACTIVE, 8)
    };
    indices.signal_type = type_symbol >> 1;
    indices.quant_offset_type = type_symbol & 1;

    // The gains.
    if coding == Coding::Conditional {
        indices.gains[0] = ec.icdf(&GAIN_DELTA, 8) as i32;
    } else {
        indices.gains[0] = (ec.icdf(&GAIN_MSB[indices.signal_type], 8) << 3) as i32;
        indices.gains[0] += ec.icdf(&GAIN_LSB, 8) as i32;
    }
    for gain in indices.gains.iter_mut().take(channel.nb_subfr).skip(1) {
        *gain = ec.icdf(&GAIN_DELTA, 8) as i32;
    }

    // The frequencies.
    let wideband = channel.fs_khz == 16;
    let voiced = indices.signal_type == TYPE_VOICED;
    let stage1 = ec.icdf(&LSF_STAGE1[usize::from(wideband)][usize::from(voiced)], 8);
    indices.lsf[0] = stage1 as i32;
    for k in 0..channel.lpc_order {
        let letter = if wideband {
            usize::from(LSF_SELECT_WB[stage1][k] - b'i') + 8
        } else {
            usize::from(LSF_SELECT_NB[stage1][k] - b'a')
        };
        let mut symbol = ec.icdf(&LSF_STAGE2[letter], 8) as i32;
        if symbol == 0 {
            symbol -= ec.icdf(&LSF_EXTENSION, 8) as i32;
        } else if symbol == 8 {
            symbol += ec.icdf(&LSF_EXTENSION, 8) as i32;
        }
        indices.lsf[k + 1] = symbol - 4;
    }
    indices.lsf_interp_q2 = if channel.nb_subfr == 4 {
        ec.icdf(&LSF_INTERPOLATION, 8) as i32
    } else {
        4
    };

    if voiced {
        // The pitch lags.
        let mut absolute = true;
        if coding == Coding::Conditional && channel.ec_prev_signal_type == TYPE_VOICED {
            let delta = ec.icdf(&PITCH_DELTA, 8) as i32;
            if delta > 0 {
                indices.lag = channel.ec_prev_lag_index + delta - 9;
                absolute = false;
            }
        }
        if absolute {
            let high = ec.icdf(&PITCH_HIGH, 8) as i32 * (channel.fs_khz as i32 >> 1);
            let low = match channel.fs_khz {
                8 => ec.icdf(&PITCH_LOW_NB, 8),
                12 => ec.icdf(&PITCH_LOW_MB, 8),
                _ => ec.icdf(&PITCH_LOW_WB, 8),
            } as i32;
            indices.lag = high + low;
        }
        channel.ec_prev_lag_index = indices.lag;
        indices.contour = match (channel.fs_khz, channel.nb_subfr) {
            (8, 4) => ec.icdf(&CONTOUR_NB_20MS, 8),
            (8, _) => ec.icdf(&CONTOUR_NB_10MS, 8),
            (_, 4) => ec.icdf(&CONTOUR_WB_20MS, 8),
            _ => ec.icdf(&CONTOUR_WB_10MS, 8),
        };
        // The long-term predictor's filters.
        indices.periodicity = ec.icdf(&LTP_PERIODICITY, 8);
        for ltp in indices.ltp.iter_mut().take(channel.nb_subfr) {
            *ltp = match indices.periodicity {
                0 => ec.icdf(&LTP_FILTER_0, 8),
                1 => ec.icdf(&LTP_FILTER_1, 8),
                _ => ec.icdf(&LTP_FILTER_2, 8),
            };
        }
        indices.ltp_scale = if coding == Coding::Independent {
            ec.icdf(&LTP_SCALING, 8)
        } else {
            0
        };
    }
    channel.ec_prev_signal_type = indices.signal_type;
    indices.seed = ec.icdf(&UNIFORM4, 8) as u32;
    Ok(())
}

/// The excitation pulses of a frame (`silk_decode_pulses`, section
/// 4.2.7.8).
fn decode_pulses(
    ec: &mut RangeDecoder<'_>,
    pulses: &mut [i16; MAX_FRAME_LENGTH + 16],
    signal_type: usize,
    quant_offset_type: usize,
    frame_length: usize,
) {
    let rate_level = ec.icdf(&RATE_LEVEL[signal_type >> 1], 8);
    let blocks = frame_length.div_ceil(16);
    let mut sum_pulses = [0usize; 20];
    let mut lshifts = [0u32; 20];
    for block in 0..blocks {
        let mut count = ec.icdf(&PULSE_COUNT[rate_level], 8);
        while count == 17 {
            lshifts[block] += 1;
            let level = if lshifts[block] == 10 { 10 } else { 9 };
            count = ec.icdf(&PULSE_COUNT[level], 8);
        }
        sum_pulses[block] = count;
    }
    for block in 0..blocks {
        let out = &mut pulses[block * 16..block * 16 + 16];
        if sum_pulses[block] > 0 {
            shell_decode(ec, out, sum_pulses[block]);
        } else {
            out.fill(0);
        }
    }
    for block in 0..blocks {
        if lshifts[block] > 0 {
            for value in &mut pulses[block * 16..block * 16 + 16] {
                let mut magnitude = i32::from(*value);
                for _ in 0..lshifts[block] {
                    magnitude = (magnitude << 1) + ec.icdf(&EXCITATION_LSB, 8) as i32;
                }
                *value = magnitude as i16;
            }
        }
    }
    // The signs.
    let row = &SIGN_NEGATIVE[2 * signal_type + quant_offset_type];
    for block in 0..blocks {
        if sum_pulses[block] == 0 && lshifts[block] == 0 {
            continue;
        }
        let icdf = [255 - row[sum_pulses[block].min(6)] + 1, 0];
        for value in &mut pulses[block * 16..block * 16 + 16] {
            if *value > 0 && ec.icdf(&icdf, 8) == 0 {
                *value = -*value;
            }
        }
    }
}

/// Where the pulses of a block of 16 fall: each partition's split between
/// its halves, from the whole block down to single samples, depth first
/// (`silk_shell_decoder`).
fn shell_decode(ec: &mut RangeDecoder<'_>, out: &mut [i16], pulses: usize) {
    fn split(ec: &mut RangeDecoder<'_>, out: &mut [i16], pulses: usize, table: usize) {
        let n = out.len();
        if n == 1 {
            out[0] = pulses as i16;
            return;
        }
        let (left, right) = out.split_at_mut(n / 2);
        if pulses == 0 {
            left.fill(0);
            right.fill(0);
            return;
        }
        let first = ec.icdf(&SPLIT[table][pulses - 1][..pulses + 1], 8);
        split(ec, left, first, table + 1);
        split(ec, right, pulses - first, table + 1);
    }
    split(ec, &mut out[..16], pulses.min(16), 0);
}

/// Decodes one frame of one channel (`silk_decode_frame`), into `out` at
/// the internal rate.
fn decode_frame(
    channel: &mut Channel,
    ec: &mut RangeDecoder<'_>,
    frame_index: usize,
    coding: Coding,
    out: &mut [i16],
) -> Result<(), SilkError> {
    decode_indices(channel, ec, frame_index, false, coding)?;
    let mut pulses = [0i16; MAX_FRAME_LENGTH + 16];
    let indices = channel.indices;
    decode_pulses(
        ec,
        &mut pulses,
        indices.signal_type,
        indices.quant_offset_type,
        channel.frame_length,
    );
    let control = decode_parameters(channel, coding);
    decode_core(channel, &control, out, &pulses);
    channel.prev_signal_type = indices.signal_type;
    channel.first_frame_after_reset = false;

    let length = channel.frame_length;
    let keep = channel.ltp_mem_length - length;
    channel.out_buf.copy_within(length..length + keep, 0);
    channel.out_buf[keep..keep + length].copy_from_slice(&out[..length]);
    channel.lag_prev = control.pitch_lags[channel.nb_subfr - 1];
    Ok(())
}

/// The parameters of a frame, worked out from its symbols
/// (`silk_decoder_control`).
struct Control {
    pitch_lags: [i32; 4],
    gains_q16: [i32; 4],
    pred_coef_q12: [[i16; MAX_ORDER]; 2],
    ltp_coef_q14: [i16; 4 * LTP_ORDER],
    ltp_scale_q14: i32,
}

/// Turns the symbols into gains, filters and lags
/// (`silk_decode_parameters`).
fn decode_parameters(channel: &mut Channel, coding: Coding) -> Control {
    let indices = &mut channel.indices;
    let mut control = Control {
        pitch_lags: [0; 4],
        gains_q16: [0; 4],
        pred_coef_q12: [[0; MAX_ORDER]; 2],
        ltp_coef_q14: [0; 4 * LTP_ORDER],
        ltp_scale_q14: 0,
    };

    // The gains (section 4.2.7.4).
    let mut previous = channel.last_gain_index;
    for (k, gain) in control
        .gains_q16
        .iter_mut()
        .enumerate()
        .take(channel.nb_subfr)
    {
        if k == 0 && coding != Coding::Conditional {
            previous = indices.gains[0].max(previous - 16);
        } else {
            let step = indices.gains[k] - 4;
            let threshold = 2 * 36 - 64 + previous;
            if step > threshold {
                previous += (step << 1) - threshold;
            } else {
                previous += step;
            }
        }
        previous = previous.clamp(0, 63);
        *gain = log2lin((smulwb(0x1D_1C71, previous) + 2_090).min(3_967));
    }
    channel.last_gain_index = previous;

    // The filters.
    let order = channel.lpc_order;
    let mut nlsf = [0i32; MAX_ORDER];
    decode_nlsf(
        order == 16,
        indices.lsf[0] as usize,
        &indices.lsf[1..],
        &mut nlsf[..order],
    );
    nlsf_to_lpc(&nlsf[..order], &mut control.pred_coef_q12[1][..order]);
    if channel.first_frame_after_reset {
        indices.lsf_interp_q2 = 4;
    }
    if indices.lsf_interp_q2 < 4 {
        let mut interpolated = [0i32; MAX_ORDER];
        for k in 0..order {
            interpolated[k] = channel.prev_nlsf_q15[k]
                + ((indices.lsf_interp_q2 * (nlsf[k] - channel.prev_nlsf_q15[k])) >> 2);
        }
        nlsf_to_lpc(
            &interpolated[..order],
            &mut control.pred_coef_q12[0][..order],
        );
    } else {
        control.pred_coef_q12[0] = control.pred_coef_q12[1];
    }
    channel.prev_nlsf_q15[..order].copy_from_slice(&nlsf[..order]);

    if indices.signal_type == TYPE_VOICED {
        // The lags (section 4.2.7.6.1).
        let fs = channel.fs_khz as i32;
        let min_lag = 2 * fs;
        let max_lag = 18 * fs;
        let lag = min_lag + indices.lag;
        for k in 0..channel.nb_subfr {
            let offset = match (channel.fs_khz, channel.nb_subfr) {
                (8, 4) => LAGS_NB_20MS.get(indices.contour).map_or(0, |row| row[k]),
                (8, _) => LAGS_NB_10MS.get(indices.contour).map_or(0, |row| row[k]),
                (_, 4) => LAGS_WB_20MS.get(indices.contour).map_or(0, |row| row[k]),
                _ => LAGS_WB_10MS.get(indices.contour).map_or(0, |row| row[k]),
            };
            control.pitch_lags[k] = (lag + i32::from(offset)).clamp(min_lag, max_lag);
        }
        // The filters (section 4.2.7.6.2).
        for k in 0..channel.nb_subfr {
            let taps: [i8; 5] = match indices.periodicity {
                0 => LTP_TAPS_0[indices.ltp[k]],
                1 => LTP_TAPS_1[indices.ltp[k]],
                _ => LTP_TAPS_2[indices.ltp[k]],
            };
            for (i, &tap) in taps.iter().enumerate() {
                control.ltp_coef_q14[k * LTP_ORDER + i] = i16::from(tap) << 7;
            }
        }
        control.ltp_scale_q14 = LTP_SCALES_Q14[indices.ltp_scale];
    } else {
        indices.periodicity = 0;
    }
    control
}

/// `2^(x / 128)`, approximated as SILK does (`silk_log2lin`).
fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    }
    if in_log_q7 >= 3_967 {
        return i32::MAX;
    }
    let out = 1i32 << (in_log_q7 >> 7);
    let frac = in_log_q7 & 0x7F;
    let correction = smlawb(frac, frac * (128 - frac), -174);
    if in_log_q7 < 2_048 {
        out + ((out * correction) >> 7)
    } else {
        out + (out >> 7) * correction
    }
}

/// `a * (1 << q) / b`, in SILK's fixed point (`silk_DIV32_varQ`).
fn div32_var_q(a: i32, b: i32, q: i32) -> i32 {
    if b == 0 || b == i32::MIN || a == i32::MIN {
        return 0;
    }
    let a_headroom = a.unsigned_abs().leading_zeros() as i32 - 1;
    let a_norm = a << a_headroom.max(0);
    let b_headroom = b.unsigned_abs().leading_zeros() as i32 - 1;
    let b_norm = b << b_headroom;
    let b_inv = (i32::MAX >> 2) / (b_norm >> 16);
    let mut result = smulwb(a_norm, b_inv);
    let a_rest = a_norm
        .wrapping_sub((((i64::from(b_norm) * i64::from(result)) >> 32) as i32).wrapping_shl(3));
    result = smlawb(result, a_rest, b_inv);
    let shift = 29 + a_headroom.max(0) - b_headroom - q;
    if shift < 0 {
        lshift_sat32(result, -shift)
    } else if shift < 32 {
        result >> shift
    } else {
        0
    }
}

/// The excitation through the long-term and the short-term predictor
/// (`silk_decode_core`, section 4.2.7.9).
#[allow(clippy::needless_range_loop)]
fn decode_core(channel: &mut Channel, control: &Control, out: &mut [i16], pulses: &[i16]) {
    let indices = &channel.indices;
    let offset_q10 = QUANT_OFFSETS_Q10[indices.signal_type >> 1][indices.quant_offset_type];
    let interpolated = indices.lsf_interp_q2 < 4;

    // The excitation, with the noise of the generator.
    let mut seed = indices.seed as i32;
    for i in 0..channel.frame_length {
        seed = seed.wrapping_mul(196_314_165).wrapping_add(907_633_515);
        let mut value = i32::from(pulses[i]) << 14;
        if value > 0 {
            value -= 80 << 4;
        } else if value < 0 {
            value += 80 << 4;
        }
        value += offset_q10 << 4;
        if seed < 0 {
            value = -value;
        }
        channel.exc_q14[i] = value;
        seed = seed.wrapping_add(i32::from(pulses[i]));
    }

    let order = channel.lpc_order;
    let subfr_length = channel.subfr_length;
    let ltp_mem = channel.ltp_mem_length;
    let mut slpc = [0i32; MAX_SUBFRAME_LENGTH + MAX_ORDER];
    slpc[..MAX_ORDER].copy_from_slice(&channel.slpc_q14);
    let mut s_ltp = [0i16; 2 * MAX_FRAME_LENGTH];
    let mut s_ltp_q15 = [0i32; 2 * MAX_FRAME_LENGTH];
    let mut res_q14 = [0i32; MAX_SUBFRAME_LENGTH];
    let mut ltp_buf_index = ltp_mem;

    for k in 0..channel.nb_subfr {
        let a_q12 = &control.pred_coef_q12[k >> 1];
        let b_q14 = &control.ltp_coef_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER];
        let signal_type = indices.signal_type;
        let gain_q16 = control.gains_q16[k];
        let gain_q10 = gain_q16 >> 6;
        let mut inv_gain_q31 = inverse32_var_q(gain_q16, 47);

        // The short-term state scaled to the new gain.
        let gain_adj_q16 = if gain_q16 != channel.prev_gain_q16 {
            let adjust = div32_var_q(channel.prev_gain_q16, gain_q16, 16);
            for value in &mut slpc[..MAX_ORDER] {
                *value = smulww(adjust, *value);
            }
            adjust
        } else {
            1 << 16
        };
        channel.prev_gain_q16 = gain_q16;

        let exc = &channel.exc_q14[k * subfr_length..(k + 1) * subfr_length];
        if signal_type == TYPE_VOICED {
            let lag = control.pitch_lags[k].max(0) as usize;
            // Rewhitening, with the coefficients of this subframe.
            if k == 0 || (k == 2 && interpolated) {
                let start = ltp_mem - lag - order - LTP_ORDER / 2;
                if k == 2 {
                    channel.out_buf[ltp_mem..ltp_mem + 2 * subfr_length]
                        .copy_from_slice(&out[..2 * subfr_length]);
                }
                lpc_analysis_filter(
                    &mut s_ltp[start..ltp_mem],
                    &channel.out_buf[start + k * subfr_length..ltp_mem + k * subfr_length],
                    &a_q12[..order],
                );
                // Less of the state carries over from the packet before.
                if k == 0 {
                    inv_gain_q31 = smulwb(inv_gain_q31, control.ltp_scale_q14) << 2;
                }
                for i in 0..lag + LTP_ORDER / 2 {
                    s_ltp_q15[ltp_buf_index - i - 1] =
                        smulwb(inv_gain_q31, i32::from(s_ltp[ltp_mem - i - 1]));
                }
            } else if gain_adj_q16 != 1 << 16 {
                for i in 0..lag + LTP_ORDER / 2 {
                    let at = ltp_buf_index - i - 1;
                    s_ltp_q15[at] = smulww(gain_adj_q16, s_ltp_q15[at]);
                }
            }
            // The long-term prediction.
            for i in 0..subfr_length {
                let at = ltp_buf_index - lag + LTP_ORDER / 2;
                let mut prediction = 2i32;
                for (tap, &b) in b_q14.iter().enumerate() {
                    prediction = smlawb(prediction, s_ltp_q15[at - tap], i32::from(b));
                }
                res_q14[i] = exc[i].wrapping_add(prediction << 1);
                s_ltp_q15[ltp_buf_index] = res_q14[i] << 1;
                ltp_buf_index += 1;
            }
        } else {
            res_q14[..subfr_length].copy_from_slice(exc);
        }

        // The short-term prediction.
        let output = &mut out[k * subfr_length..(k + 1) * subfr_length];
        for i in 0..subfr_length {
            let mut prediction = (order >> 1) as i32;
            for (j, &a) in a_q12[..order].iter().enumerate() {
                prediction = smlawb(prediction, slpc[MAX_ORDER + i - 1 - j], i32::from(a));
            }
            slpc[MAX_ORDER + i] = res_q14[i].saturating_add(lshift_sat32(prediction, 4));
            output[i] = sat16(rshift_round(smulww(slpc[MAX_ORDER + i], gain_q10), 8));
        }
        slpc.copy_within(subfr_length..subfr_length + MAX_ORDER, 0);
    }
    channel.slpc_q14.copy_from_slice(&slpc[..MAX_ORDER]);
}

/// The residual of `input` through the inverse of the LPC filter, into
/// `out`, the first `order` samples left at zero
/// (`silk_LPC_analysis_filter`).
fn lpc_analysis_filter(out: &mut [i16], input: &[i16], a_q12: &[i16]) {
    let order = a_q12.len();
    let len = out.len();
    for ix in order..len {
        let mut sum = 0i32;
        for (j, &a) in a_q12.iter().enumerate() {
            sum = sum.wrapping_add(i32::from(input[ix - 1 - j]) * i32::from(a));
        }
        let residual = (i32::from(input[ix]) << 12).wrapping_sub(sum);
        out[ix] = sat16(rshift_round(residual, 12));
    }
    out[..order.min(len)].fill(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log2lin_is_a_power_of_two() {
        assert_eq!(log2lin(0), 1);
        assert_eq!(log2lin(128 * 10), 1 << 10);
        assert_eq!(log2lin(128 * 16), 1 << 16);
        let half = log2lin(128 * 20 + 64);
        let exact = 2f64.powf(20.5);
        assert!((f64::from(half) / exact - 1.0).abs() < 0.01);
    }

    #[test]
    fn the_fixed_point_divisions_divide() {
        assert!(inverse32_var_q(65_536, 47) > 2_147_000_000);
        assert!((1_023..=1_024).contains(&inverse32_var_q(1 << 20, 30)));
        let ratio = div32_var_q(3 << 16, 2 << 16, 16);
        assert!((ratio - 98_304).abs() <= 2, "{ratio}");
    }
}

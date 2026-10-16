//! The effects a part's chain and the master bus can have, by name, code
//! and the picture an editor draws for each.

/// One effect: a stage of a part's chain, or a stage of the master bus.
///
/// The code is what the WebAssembly exports take; for a part's effects,
/// `1 << code` is its bit in [`Chain::bits`](crate::Chain::bits). The
/// low-pass and the reverb keep the bits they always had, 1 and 2.
///
/// A part's effects run in a fixed order, [`Effect::PART_ORDER`]:
///
/// ```text
/// high-pass → low-pass → EQ → drive → bitcrush → compressor
///   → chorus → flanger → phaser → tremolo → auto-pan → delay → reverb
///   → gain and pan → (send to the master reverb)
/// ```
///
/// Tone shaping first, so that what is distorted and compressed is the
/// sound as it was meant to be heard; dynamics before modulation, so that
/// the compressor does not pump with the sweep; time effects last, so that
/// every echo and every reflection carries the colour of the stages before
/// it, as an echo of a guitar through its pedals does.
///
/// The master bus ([`Effect::MASTER_ORDER`]) adds the parts and the return
/// of its shared reverb, then runs the glue compressor, the master gain and
/// the limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Effect {
    /// Code 0: the resonant low-pass filter, `lowpass.*`.
    Lowpass,
    /// Code 1: the reverb, `reverb.*` on a part and `master.reverb.*` on
    /// the master bus.
    Reverb,
    /// Code 2: the resonant high-pass filter, `highpass.*`.
    Highpass,
    /// Code 3: the three-band equaliser, `eq.*`.
    Eq,
    /// Code 4: drive into a waveshaper, `drive.*`.
    Drive,
    /// Code 5: the bitcrusher, `bitcrush.*`.
    Bitcrush,
    /// Code 6: the compressor, `compressor.*`.
    Compressor,
    /// Code 7: the chorus, `chorus.*`.
    Chorus,
    /// Code 8: the flanger, `flanger.*`.
    Flanger,
    /// Code 9: the phaser, `phaser.*`.
    Phaser,
    /// Code 10: the tremolo, `tremolo.*`.
    Tremolo,
    /// Code 11: the auto-pan, `autopan.*`.
    Autopan,
    /// Code 12: the delay, `delay.*`.
    Delay,
    /// Code 13: the master bus's glue compressor, `master.glue.*`.
    Glue,
    /// Code 14: the master bus's true-peak limiter, `master.limiter.*`.
    Limiter,
}

/// What an editor draws for an effect, as the `visual` field of the
/// parameter JSON names it. [`Engine::effect_curve`](crate::Engine::effect_curve)
/// gives the points; the axes depend on the kind:
///
/// | Kind | `x` | `y` |
/// |------|-----|-----|
/// | `filter_curve`, `eq_curve` | frequency, Hz, spaced evenly in octaves | gain, dB |
/// | `transfer_curve` | input level, dBFS (compressor, glue, limiter); input sample, -1 to 1 (drive) | output level, dBFS; output sample |
/// | `lfo_shape` | position in one cycle, 0 to 1 | what the cycle moves: delay in ms (chorus, flanger), frequency in Hz (phaser), gain 0 to 1 (tremolo), position -1 to 1 (auto-pan) |
/// | `delay_taps` | time after the sound, s | gain of the echo heard then |
/// | `reverb_decay` | time after the sound, s | level of the tail, dB |
/// | `bitcrush_steps` | input sample, -1 to 1 | output sample |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Visual {
    /// The frequency response of a filter.
    FilterCurve,
    /// The frequency response of the equaliser.
    EqCurve,
    /// Output level against input level.
    TransferCurve,
    /// One cycle of a modulation.
    LfoShape,
    /// The echoes of one sound.
    DelayTaps,
    /// The decay of a reverb's tail.
    ReverbDecay,
    /// The staircase of a quantiser.
    BitcrushSteps,
}

impl Visual {
    /// The kind's name in the JSON: `"filter_curve"` and so on.
    pub const fn name(self) -> &'static str {
        match self {
            Self::FilterCurve => "filter_curve",
            Self::EqCurve => "eq_curve",
            Self::TransferCurve => "transfer_curve",
            Self::LfoShape => "lfo_shape",
            Self::DelayTaps => "delay_taps",
            Self::ReverbDecay => "reverb_decay",
            Self::BitcrushSteps => "bitcrush_steps",
        }
    }
}

impl Effect {
    /// Every effect, in the order of its code.
    pub const ALL: [Self; 15] = [
        Self::Lowpass,
        Self::Reverb,
        Self::Highpass,
        Self::Eq,
        Self::Drive,
        Self::Bitcrush,
        Self::Compressor,
        Self::Chorus,
        Self::Flanger,
        Self::Phaser,
        Self::Tremolo,
        Self::Autopan,
        Self::Delay,
        Self::Glue,
        Self::Limiter,
    ];

    /// The effects of a part, in the order the chain runs them.
    pub const PART_ORDER: [Self; 13] = [
        Self::Highpass,
        Self::Lowpass,
        Self::Eq,
        Self::Drive,
        Self::Bitcrush,
        Self::Compressor,
        Self::Chorus,
        Self::Flanger,
        Self::Phaser,
        Self::Tremolo,
        Self::Autopan,
        Self::Delay,
        Self::Reverb,
    ];

    /// The effects of the master bus, in the order it runs them: the
    /// shared reverb's return joins the parts first.
    pub const MASTER_ORDER: [Self; 3] = [Self::Reverb, Self::Glue, Self::Limiter];

    /// The code the WebAssembly exports use.
    pub const fn code(self) -> u32 {
        match self {
            Self::Lowpass => 0,
            Self::Reverb => 1,
            Self::Highpass => 2,
            Self::Eq => 3,
            Self::Drive => 4,
            Self::Bitcrush => 5,
            Self::Compressor => 6,
            Self::Chorus => 7,
            Self::Flanger => 8,
            Self::Phaser => 9,
            Self::Tremolo => 10,
            Self::Autopan => 11,
            Self::Delay => 12,
            Self::Glue => 13,
            Self::Limiter => 14,
        }
    }

    /// The effect with this code, if there is one.
    pub const fn from_code(code: u32) -> Option<Self> {
        if (code as usize) < Self::ALL.len() {
            Some(Self::ALL[code as usize])
        } else {
            None
        }
    }

    /// The effect's name, the prefix of its parameters' identifiers:
    /// `"lowpass"`, `"delay"`, `"glue"` and so on.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lowpass => "lowpass",
            Self::Reverb => "reverb",
            Self::Highpass => "highpass",
            Self::Eq => "eq",
            Self::Drive => "drive",
            Self::Bitcrush => "bitcrush",
            Self::Compressor => "compressor",
            Self::Chorus => "chorus",
            Self::Flanger => "flanger",
            Self::Phaser => "phaser",
            Self::Tremolo => "tremolo",
            Self::Autopan => "autopan",
            Self::Delay => "delay",
            Self::Glue => "glue",
            Self::Limiter => "limiter",
        }
    }

    /// The effect with this name.
    ///
    /// ```
    /// use manguebyte_web::Effect;
    ///
    /// assert_eq!(Effect::from_name("delay"), Some(Effect::Delay));
    /// assert_eq!(Effect::from_name("echo"), None);
    /// ```
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| effect.name() == name)
    }

    /// What an editor draws for the effect.
    pub const fn visual(self) -> Visual {
        match self {
            Self::Lowpass | Self::Highpass => Visual::FilterCurve,
            Self::Eq => Visual::EqCurve,
            Self::Drive | Self::Compressor | Self::Glue | Self::Limiter => Visual::TransferCurve,
            Self::Bitcrush => Visual::BitcrushSteps,
            Self::Chorus | Self::Flanger | Self::Phaser | Self::Tremolo | Self::Autopan => {
                Visual::LfoShape
            }
            Self::Delay => Visual::DelayTaps,
            Self::Reverb => Visual::ReverbDecay,
        }
    }

    /// Whether a part's chain can have the effect. The glue compressor and
    /// the limiter belong to the master bus only.
    pub const fn is_part_effect(self) -> bool {
        !matches!(self, Self::Glue | Self::Limiter)
    }

    /// Whether the master bus has the effect: the glue compressor, the
    /// limiter, and the shared reverb.
    pub const fn is_master_effect(self) -> bool {
        matches!(self, Self::Glue | Self::Limiter | Self::Reverb)
    }

    /// The effect's bit in [`Chain::bits`](crate::Chain::bits), or 0 for an
    /// effect of the master bus only.
    pub const fn bit(self) -> u32 {
        if self.is_part_effect() {
            1 << self.code()
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_names_and_bits_round_trip() {
        for (code, effect) in Effect::ALL.into_iter().enumerate() {
            assert_eq!(effect.code(), code as u32);
            assert_eq!(Effect::from_code(code as u32), Some(effect));
            assert_eq!(Effect::from_name(effect.name()), Some(effect));
        }
        assert_eq!(Effect::from_code(Effect::ALL.len() as u32), None);
        assert_eq!(Effect::Lowpass.bit(), 1);
        assert_eq!(Effect::Reverb.bit(), 2);
        assert_eq!(Effect::Glue.bit(), 0);
        let mut order: Vec<Effect> = Effect::PART_ORDER.to_vec();
        order.sort_by_key(|effect| effect.code());
        let parts: Vec<Effect> = Effect::ALL
            .into_iter()
            .filter(|effect| effect.is_part_effect())
            .collect();
        assert_eq!(order, parts, "the order has every part effect once");
    }
}

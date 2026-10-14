//! OSC time tags.

use std::ops::{Add, Sub};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds from 1 January 1900, where a time tag counts from, to 1 January
/// 1970, where Unix time counts from: seventy years, seventeen of them leap
/// years. RFC 5905 gives the same figure for NTP timestamps.
const UNIX_EPOCH_IN_NTP_SECONDS: i64 = 2_208_988_800;

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// The first second, counted from 1900, that [`OscTime`] maps to a calendar
/// time: 20 January 1968, 03:14:08 UTC.
const WINDOW_START: i64 = 1 << 31;

/// One past the last such second: 26 February 2104, 09:42:24 UTC.
const WINDOW_END: i64 = WINDOW_START + (1 << 32);

/// How many units of a time tag make one second.
pub(crate) const TICKS_PER_SECOND: f64 = 4_294_967_296.0;

/// A moment in time, in the format of an OSC time tag.
///
/// A time tag is a 64-bit fixed-point number, the same as an NTP timestamp:
/// the first 32 bits count seconds since midnight UTC on 1 January 1900 and
/// the last 32 bits count fractions of a second, each about 233 picoseconds.
/// One value is reserved: [`OscTime::IMMEDIATELY`].
///
/// ```
/// use std::time::{Duration, UNIX_EPOCH};
/// use manguebyte_osc::OscTime;
///
/// let time = OscTime::from_system_time(UNIX_EPOCH + Duration::from_millis(1_500)).unwrap();
/// assert_eq!(time, OscTime::new(2_208_988_801, 1 << 31));
/// assert_eq!(time.to_system_time(), Some(UNIX_EPOCH + Duration::from_millis(1_500)));
/// assert_eq!(OscTime::IMMEDIATELY.to_system_time(), None);
/// ```
///
/// # The year 2036
///
/// Thirty-two bits of seconds run out on 7 February 2036 at 06:28:16 UTC,
/// and the OSC specification does not say what happens then. This type
/// follows the rule RFC 4330 (section 3) gives for NTP: a value with its
/// top bit set lies between 1968 and 2036, and a value with its top bit
/// clear lies between 2036 and 2104. Conversions to and from
/// [`SystemTime`] use that rule, and [`seconds_since`](Self::seconds_since)
/// subtracts modulo 2<sup>64</sup>, so a difference is right across the
/// rollover as long as the two times are less than 68 years apart.
///
/// The comparison operators compare the raw 64-bit values, which is the
/// comparison the specification means when it orders the time tags of
/// nested bundles. They do not know about the rollover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OscTime(u64);

impl OscTime {
    /// The time tag that means "as soon as the packet arrives": 63 zero
    /// bits followed by a one.
    pub const IMMEDIATELY: Self = Self(1);

    /// Builds a time from seconds since 1900 and a fraction of a second in
    /// units of 2<sup>-32</sup> seconds.
    pub const fn new(seconds: u32, fraction: u32) -> Self {
        Self(((seconds as u64) << 32) | fraction as u64)
    }

    /// Builds a time from the 64 bits of a time tag.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The 64 bits of the time tag.
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// The first 32 bits: whole seconds.
    pub const fn seconds(self) -> u32 {
        (self.0 >> 32) as u32
    }

    /// The last 32 bits: the fraction of a second.
    pub const fn fraction(self) -> u32 {
        self.0 as u32
    }

    /// Whether this is [`OscTime::IMMEDIATELY`].
    pub const fn is_immediate(self) -> bool {
        self.0 == Self::IMMEDIATELY.0
    }

    /// The system clock's current time.
    ///
    /// Reads the clock and nothing else, so it may be called on the audio
    /// thread. If the clock is set outside the years this type covers, the
    /// result is [`OscTime::IMMEDIATELY`].
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now()).unwrap_or(Self::IMMEDIATELY)
    }

    /// Converts a system time.
    ///
    /// Returns `None` for a time before 20 January 1968 or after 26
    /// February 2104, which a time tag cannot tell apart from one inside
    /// that range. The fraction is rounded up, by less than 233
    /// picoseconds, so that [`to_system_time`](Self::to_system_time) gives
    /// back the same nanosecond.
    pub fn from_system_time(time: SystemTime) -> Option<Self> {
        // Whole seconds since the Unix epoch, rounded down, and what is left.
        let (unix_seconds, nanos) = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => (i64::try_from(after.as_secs()).ok()?, after.subsec_nanos()),
            Err(error) => {
                let before = error.duration();
                let seconds = i64::try_from(before.as_secs()).ok()?;
                match before.subsec_nanos() {
                    0 => (-seconds, 0),
                    nanos => (-seconds - 1, NANOS_PER_SECOND as u32 - nanos),
                }
            }
        };
        let ntp_seconds = unix_seconds.checked_add(UNIX_EPOCH_IN_NTP_SECONDS)?;
        if !(WINDOW_START..WINDOW_END).contains(&ntp_seconds) {
            return None;
        }
        // The range is exactly 2^32 seconds long, so the low 32 bits are
        // enough for `to_system_time` to find the second again.
        let seconds = (ntp_seconds & 0xFFFF_FFFF) as u32;
        Some(Self::new(seconds, nanos_to_fraction_up(nanos)))
    }

    /// Converts to a system time.
    ///
    /// Returns `None` for [`OscTime::IMMEDIATELY`], which is not a moment,
    /// and if the platform's clock cannot represent the result.
    pub fn to_system_time(self) -> Option<SystemTime> {
        if self.is_immediate() {
            return None;
        }
        let seconds = i64::from(self.seconds());
        let ntp_seconds = if seconds >= WINDOW_START {
            seconds
        } else {
            seconds + (1 << 32)
        };
        let unix_seconds = ntp_seconds - UNIX_EPOCH_IN_NTP_SECONDS;
        let nanos = Duration::new(0, fraction_to_nanos(self.fraction()));
        if unix_seconds >= 0 {
            UNIX_EPOCH
                .checked_add(Duration::from_secs(unix_seconds.unsigned_abs()))?
                .checked_add(nanos)
        } else {
            UNIX_EPOCH
                .checked_sub(Duration::from_secs(unix_seconds.unsigned_abs()))?
                .checked_add(nanos)
        }
    }

    /// The time `frames` frames later in a stream of `sample_rate` frames a
    /// second, to the nearest unit of a time tag.
    ///
    /// This is the way to say when a block of audio begins: count the
    /// frames from the moment the stream started. A [`Duration`] cannot do
    /// it exactly, because a frame is rarely a whole number of nanoseconds,
    /// and adding one block's duration after another lets the error grow.
    /// With a whole number as the sample rate the result is exact to the
    /// unit, whatever the number of frames; with a fractional one it is as
    /// close as a double allows. Wraps around like addition.
    ///
    /// A sample rate below 1, or one that is not a number, counts as 1.
    ///
    /// ```
    /// use std::time::Duration;
    /// use manguebyte_osc::OscTime;
    ///
    /// let start = OscTime::new(3_900_000_000, 0);
    /// assert_eq!(start.add_frames(48_000, 48_000.0), start + Duration::from_secs(1));
    /// // 480 frames at 48 kHz are 42,949,672.96 units of a time tag.
    /// let block = start.add_frames(480, 48_000.0);
    /// assert_eq!(block.to_bits() - start.to_bits(), 42_949_673);
    /// ```
    pub fn add_frames(self, frames: u64, sample_rate: f64) -> Self {
        let rate = sample_rate.max(1.0);
        let ticks = if rate.fract() == 0.0 && rate <= TICKS_PER_SECOND {
            // A whole number of frames a second: exact in integers. The
            // rate is at least 1 and at most 2^32, so it fits.
            let rate = rate as u128;
            (((u128::from(frames) << 32) + rate / 2) / rate) as u64
        } else {
            // Whole seconds first, so that the fraction keeps its
            // precision however far into the stream this is.
            let whole = (frames as f64 / rate).floor();
            let rest = frames as f64 - whole * rate;
            let fraction = (rest / rate * TICKS_PER_SECOND).round() as i64;
            (whole as u64)
                .wrapping_shl(32)
                .wrapping_add(fraction as u64)
        };
        Self(self.0.wrapping_add(ticks))
    }

    /// How many seconds this time lies after `earlier`; negative if it lies
    /// before.
    ///
    /// The subtraction is modulo 2<sup>64</sup>, so the result is right
    /// across the 2036 rollover for times less than 68 years apart.
    pub fn seconds_since(self, earlier: Self) -> f64 {
        self.ticks().wrapping_sub(earlier.ticks()) as f64 / TICKS_PER_SECOND
    }

    /// The time as a signed count of 2<sup>-32</sup> second units from the
    /// 2036 rollover. Under the rule described on the type this is one
    /// unbroken timeline from 1968 to 2104.
    pub(crate) const fn ticks(self) -> i64 {
        self.0 as i64
    }
}

impl Default for OscTime {
    /// [`OscTime::IMMEDIATELY`].
    fn default() -> Self {
        Self::IMMEDIATELY
    }
}

impl Add<Duration> for OscTime {
    type Output = Self;

    /// The time `duration` later. Wraps around after 136 years, as the
    /// seconds of a time tag do.
    ///
    /// The duration is rounded to the nearest unit of a time tag, which is
    /// 2<sup>-32</sup> seconds or about 233 picoseconds. Subtraction rounds
    /// the same way, so `(time + duration) - duration` is `time` again.
    /// A sum is therefore off by up to half a unit, and one made of many
    /// small steps by up to half a unit for each step: to step through a
    /// stream of audio use [`add_frames`](OscTime::add_frames) from a
    /// fixed start. The nearest unit can lie just below the exact sum, so
    /// [`to_system_time`](OscTime::to_system_time), which rounds down, may
    /// give the nanosecond before the one that arithmetic on system times
    /// would give.
    fn add(self, duration: Duration) -> Self {
        Self(self.0.wrapping_add(duration_to_ticks(duration)))
    }
}

impl Sub<Duration> for OscTime {
    type Output = Self;

    /// The time `duration` earlier. Wraps around and rounds like addition.
    fn sub(self, duration: Duration) -> Self {
        Self(self.0.wrapping_sub(duration_to_ticks(duration)))
    }
}

/// A duration in the units of a time tag, rounded to the nearest unit.
/// Whole seconds beyond 2<sup>32</sup> wrap around, as the seconds of a
/// time tag do.
///
/// Everything in this crate that turns a duration into time tag units goes
/// through here, so that the same duration is always the same number.
pub(crate) fn duration_to_ticks(duration: Duration) -> u64 {
    // At most 999,999,999 nanoseconds, which rounds to less than 2^32, so
    // the fraction never carries into the seconds.
    let fraction =
        ((u64::from(duration.subsec_nanos()) << 32) + NANOS_PER_SECOND / 2) / NANOS_PER_SECOND;
    duration.as_secs().wrapping_shl(32).wrapping_add(fraction)
}

/// Nanoseconds as a 32-bit binary fraction of a second, rounded up.
///
/// Rounding up is what makes the trip to a time tag and back exact: one
/// unit of the fraction is under a quarter of a nanosecond, so rounding
/// down on the way back always lands on the nanosecond that went in.
fn nanos_to_fraction_up(nanos: u32) -> u32 {
    let fraction = (u64::from(nanos) << 32).div_ceil(NANOS_PER_SECOND);
    u32::try_from(fraction).unwrap_or(u32::MAX)
}

/// A 32-bit binary fraction of a second as nanoseconds, rounded down.
fn fraction_to_nanos(fraction: u32) -> u32 {
    ((u64::from(fraction) * NANOS_PER_SECOND) >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_nanosecond_count_survives_the_fraction() {
        // All of them would take a while; a stride that is coprime with
        // every power of two and of ten visits each residue class.
        let mut nanos = 0u32;
        let mut checked = 0;
        while nanos < 1_000_000_000 {
            assert_eq!(fraction_to_nanos(nanos_to_fraction_up(nanos)), nanos);
            nanos += 7_919;
            checked += 1;
        }
        assert!(checked > 100_000);
        for nanos in [0, 1, 2, 499_999_999, 500_000_000, 999_999_998, 999_999_999] {
            assert_eq!(fraction_to_nanos(nanos_to_fraction_up(nanos)), nanos);
        }
        // The reserved value cannot come out of a conversion: one
        // nanosecond is already more than four units.
        assert_eq!(nanos_to_fraction_up(0), 0);
        assert!(nanos_to_fraction_up(1) > 1);
    }

    #[test]
    fn a_duration_becomes_the_nearest_number_of_units() {
        assert_eq!(duration_to_ticks(Duration::ZERO), 0);
        assert_eq!(duration_to_ticks(Duration::from_secs(1)), 1 << 32);
        assert_eq!(duration_to_ticks(Duration::from_millis(250)), 1 << 30);
        // One nanosecond is 4.29 units and three are 12.88.
        assert_eq!(duration_to_ticks(Duration::from_nanos(1)), 4);
        assert_eq!(duration_to_ticks(Duration::from_nanos(3)), 13);
        // The largest fraction of a second stays below a whole one.
        assert_eq!(
            duration_to_ticks(Duration::new(7, 999_999_999)),
            (7 << 32) + 4_294_967_292
        );
        // Never more than half a unit from the exact value.
        let mut nanos = 0u32;
        while nanos < 1_000_000_000 {
            let exact = f64::from(nanos) * TICKS_PER_SECOND / 1e9;
            let ticks = duration_to_ticks(Duration::new(0, nanos)) as f64;
            assert!((ticks - exact).abs() <= 0.5, "{nanos}");
            nanos += 104_729;
        }
        // Seconds beyond what a time tag holds wrap around.
        assert_eq!(
            duration_to_ticks(Duration::from_secs((1 << 32) + 5)),
            5 << 32
        );
    }
}

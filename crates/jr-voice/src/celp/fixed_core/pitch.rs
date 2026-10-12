//! Adaptive codebook: pitch lag decoding and fractional-delay prediction.
//!
//! Lags are carried at 1/3-sample resolution over the range 20..=143.  The
//! first subframe of each half-frame sends an absolute lag in 8 bits, the
//! second sends a 5-bit delta relative to it.

use crate::celp::fixed_core::fixed::{acc, hi, low, mul, restoring_divide, sat};

/// Samples per subframe.
pub use crate::celp::fixed_core::SUBFRAME;
/// Taps on each side of the fractional interpolation filter.
const INTERP_TAPS: usize = 10;
/// Sub-sample resolution of the pitch lag.
const RESOLUTION: i16 = 3;

/// Integer lag plus a fractional part in `-1..=1` thirds of a sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lag {
    /// Integer part, 20..=143.
    pub integer: i16,
    /// Fractional part in thirds of a sample.
    pub frac: i16,
}

/// Forward and mirrored interpolation filters for one fractional phase.
fn interpolation_filters(phase: usize) -> (&'static [i16], &'static [i16]) {
    let forward =
        &crate::celp::fixed_core::tables::PITCH_INTERP[phase..phase + 3 * INTERP_TAPS - 2];
    let mirrored = RESOLUTION as usize - phase;
    let backward =
        &crate::celp::fixed_core::tables::PITCH_INTERP[mirrored..mirrored + 3 * INTERP_TAPS - 2];
    (forward, backward)
}

/// Interpolate one sample around `centre` with the two-sided pitch filter.
fn interpolated_sample(signal: &[i16], centre: usize, forward: &[i16], backward: &[i16]) -> i64 {
    let mut value = 0i64;
    for k in 0..INTERP_TAPS {
        value = acc(value + mul(signal[centre - k], forward[3 * k]));
    }
    for k in 0..INTERP_TAPS {
        value = acc(value + mul(signal[centre + 1 + k], backward[3 * k]));
    }
    value
}

/// First sharpened sample and interpolation phase, when the lag is in range.
fn sharpening_start(lag: &Lag) -> Option<(usize, usize)> {
    let mut first = (lag.integer - INTERP_TAPS as i16) as i32;
    let mut phase = -lag.frac;
    if phase < 0 {
        phase += RESOLUTION;
        first += 1;
    }
    if first >= SUBFRAME as i32 {
        return None;
    }
    Some((first as usize, phase as usize))
}

/// Decode the 8-bit absolute lag sent by the first subframe of a half-frame.
pub fn decode_absolute(code: i16) -> Lag {
    if code >= 197 {
        // The top of the range is coded at integer resolution only.
        return Lag {
            integer: code - 112,
            frac: 0,
        };
    }
    let quotient = low(restoring_divide((code as i64) + 2, RESOLUTION, 16));
    let integer = quotient + 19;
    let frac = code - low(mul(integer, RESOLUTION) >> 1) + 58;
    Lag { integer, frac }
}

/// Decode the 5-bit lag delta sent by the second subframe of a half-frame.
pub fn decode_relative(code: i16, prev: i16) -> Lag {
    let mut min = (prev - 5).max(20);
    if min + 9 > 143 {
        min = 134;
    }
    let quotient = low(restoring_divide((code as i64) + 2, RESOLUTION, 16)) - 1;
    Lag {
        integer: quotient + min,
        frac: code - 2 - low(mul(quotient, RESOLUTION) >> 1),
    }
}

/// Build the adaptive-codebook contribution by resampling the excitation
/// history at a fractional delay.
///
/// `exc[..at]` holds past excitation; `exc[at..at + SUBFRAME]` is written.
pub fn predict(exc: &mut [i16], at: usize, lag: &Lag) {
    // The filter is indexed by the *negated* fraction; a negative fraction is
    // folded into the integer delay.
    let mut phase = -lag.frac;
    let mut delay = lag.integer as usize;
    if phase < 0 {
        phase += RESOLUTION;
        delay += 1;
    }
    let phase = phase as usize;
    let (forward, backward) = interpolation_filters(phase);

    for n in 0..SUBFRAME {
        let base = at + n - delay;
        let value = interpolated_sample(exc, base, forward, backward);
        exc[at + n] = hi(sat(acc(value + 0x8000)));
    }
}

/// Periodic enhancement of the fixed-codebook vector ("pitch sharpening").
///
/// Only applied when the lag is shorter than a subframe: the
/// innovation is fed through the same fractional-delay predictor as the
/// adaptive codebook and mixed back in with the previous subframe's pitch gain.
pub fn sharpen(code: &mut [i16; SUBFRAME], lag: &Lag, gain: i16) {
    // The first output is produced ten samples before the delay itself, because
    // the interpolation filter straddles the delay point.
    let Some((first, phase)) = sharpening_start(lag) else {
        return;
    };

    // Guard region standing in for the (zero) innovation before this subframe,
    // followed by the innovation itself as it is progressively rewritten.
    const GUARD: usize = 2 * INTERP_TAPS + 1;
    let mut buf = [0i16; GUARD + SUBFRAME];
    buf[GUARD..GUARD + first].copy_from_slice(&code[..first]);

    let (forward, backward) = interpolation_filters(phase);

    for n in first..SUBFRAME {
        let centre = GUARD + (n - first) - INTERP_TAPS;
        let value = interpolated_sample(&buf, centre, forward, backward);
        let scaled = crate::celp::fixed_core::fixed::mul32x16(acc(value), gain);
        let mixed = acc(crate::celp::fixed_core::fixed::shift(scaled, 1)
            + ((code[n] as i64) << 16)
            + (1 << 15));
        code[n] = hi(mixed);
        buf[GUARD + n] = code[n];
    }
}

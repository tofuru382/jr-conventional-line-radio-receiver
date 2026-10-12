//! Sharpening gains for fixed-codebook excitation.

use crate::celp::fixed_core::fixed::{acc, low, shift};

/// A half, the value both extension gains are set to when they are set at all.
const HALF: i64 = 16384;
/// The previous gain has to be above this for the second gain to be allowed.
const WAS_STRONG: i64 = 13107;
/// How far the lag may have moved and still count as the same pitch.
const SAME_PITCH: i64 = 24576;

/// Whether the current spectrum is sharp enough to disable extension.
fn peaky_spectrum(reflection: i16) -> bool {
    acc(shift((reflection as i64) << 16, 3) - (HALF << 16)) > 0
}

/// Whether the previous adaptive gain is strong enough to extend.
fn gain_was_strong(previous_gain: i16) -> bool {
    acc(shift((previous_gain as i64) << 16, 1) - (WAS_STRONG << 16)) > 0
}

/// Whether the lag remains between three quarters and one and a half times
/// the previous lag.
fn pitch_continues(lag: i16, previous_lag: i16) -> bool {
    let above_floor = acc(((lag as i64) << 16) - acc(SAME_PITCH * (previous_lag as i64) * 2)) > 0;
    let below_ceiling =
        acc(shift(acc((previous_lag as i64) * SAME_PITCH * 2), 1) - ((lag as i64) << 16)) > 0;
    above_floor && below_ceiling
}

/// The two extension gains for the coming half-frame.
///
/// The first is always a half.  The second is raised to a half when the
/// spectrum is not peaky, the previous gain was already strong, and the lag has
/// stayed within three quarters and one and a half of what it was — that is,
/// when the pitch has carried over rather than restarted.  A peaky spectrum
/// clears it; anything else leaves it where it was.
pub fn interpolation_gains(
    reflection: i16,
    previous_gain: i16,
    lag: i16,
    previous_lag: i16,
) -> (i16, i16) {
    let peaky = peaky_spectrum(reflection);
    let strong = gain_was_strong(previous_gain);
    let continuous = pitch_continues(lag, previous_lag);

    // Only the peaky test clears the gain; failing one of the others leaves
    // whatever the previous half-frame carried over.
    let second = if peaky {
        0
    } else if strong && continuous {
        HALF
    } else {
        previous_gain as i64
    };
    (low(HALF), low(second))
}

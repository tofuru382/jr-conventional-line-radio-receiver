//! Switches for the evaluation tools (crates/demod-eval) to measure what each
//! receiver feature contributes. The receiver never changes them: every
//! feature is on by default, which is the configuration that ships.
use std::sync::atomic::{AtomicU32, Ordering};

/// Try a bit-by-bit combination of both detectors' frames when neither
/// decodes on its own.
pub const COMBINE: u32 = 1 << 0;
/// Accept frame markers with a few bit errors instead of exact matches.
pub const MARKER_TOLERANCE: u32 = 1 << 1;
/// Acquire on a single frame whose markers match and that decodes.
pub const SINGLE_FRAME_ACQUISITION: u32 = 1 << 2;
/// Keep frame lock while frames decode, even when their markers are damaged.
pub const FLYWHEEL: u32 = 1 << 3;
/// Judge soft voice decoding by reliability-weighted distance.
pub const WEIGHTED_DISTANCE: u32 = 1 << 4;
/// Erase at most six data bytes (instead of eight) in soft RS retries.
pub const CONSERVATIVE_ERASURES: u32 = 1 << 5;

/// Split the input into channels with one shared polyphase filter bank
/// (jr_conventional_dsp::channelizer) instead of a front end per channel.
pub const CHANNELIZER: u32 = 1 << 6;

pub const ALL: u32 = COMBINE
    | MARKER_TOLERANCE
    | SINGLE_FRAME_ACQUISITION
    | FLYWHEEL
    | WEIGHTED_DISTANCE
    | CONSERVATIVE_ERASURES
    | CHANNELIZER;

static FLAGS: AtomicU32 = AtomicU32::new(ALL);

/// Whether `feature` is on.
#[inline]
pub fn on(feature: u32) -> bool {
    FLAGS.load(Ordering::Relaxed) & feature != 0
}
/// Evaluation only: select the receiver features (process-wide).
pub fn set(features: u32) {
    FLAGS.store(features, Ordering::Relaxed);
}
pub fn get() -> u32 {
    FLAGS.load(Ordering::Relaxed)
}

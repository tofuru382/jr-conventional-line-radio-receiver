pub mod afc;
mod coding;
pub mod data;
pub mod diag;
pub mod frame;
pub mod gain;
mod monitor;
pub mod pipeline;
pub mod presets;
pub mod recording;
pub mod rtl_tcp;
pub mod session;
pub mod source;
#[cfg(feature = "synthesis")]
pub mod synthesis;
pub mod tuning;
#[cfg(feature = "usb")]
pub mod usb;
pub mod voice_frame;
pub mod wave;
/// Detector variants (the receiver only switches the alternate detector off
/// to save power on an overheating phone).
pub use jr_conventional_dsp::tuning as dsp_tuning;
pub use jr_conventional_dsp::Complex32;

/// Locks a mutex even if a thread panicked while holding it. The receiver's
/// shared state stays usable after one worker fails (a poisoned lock would
/// otherwise make every later status read panic, up to the UI thread).
pub fn lock<T: ?Sized>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

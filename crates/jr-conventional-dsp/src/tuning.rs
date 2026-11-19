//! Detector variants for the evaluation tools (crates/demod-eval) to compare.
//! The receiver never changes them; the defaults are what ships.
use std::sync::atomic::{AtomicU32, Ordering};

/// When the alternate (drift RLS) detector runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Alternate {
    /// On every symbol.
    Always = 0,
    /// Only while the channel carries a signal (see the symbol chain).
    Gated = 1,
    /// Never: the primary detector alone (a hot phone saving power).
    Off = 2,
}

static ALTERNATE: AtomicU32 = AtomicU32::new(Alternate::Gated as u32);

pub fn alternate() -> Alternate {
    match ALTERNATE.load(Ordering::Relaxed) {
        0 => Alternate::Always,
        2 => Alternate::Off,
        _ => Alternate::Gated,
    }
}
/// Selects when the alternate detector runs, process-wide. The receiver uses
/// this only to save power when the device overheats (Android thermal state).
pub fn set_alternate(mode: Alternate) {
    ALTERNATE.store(mode as u32, Ordering::Relaxed);
}

/// Evaluation only: `key=value` pairs separated by commas.
pub fn parse(settings: &str) -> Result<(), String> {
    for item in settings.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| format!("DSP setting {item} needs KEY=VALUE"))?;
        match (key, value) {
            ("alternate", "always") => ALTERNATE.store(Alternate::Always as u32, Ordering::Relaxed),
            ("alternate", "gated") => ALTERNATE.store(Alternate::Gated as u32, Ordering::Relaxed),
            ("alternate", "off") => ALTERNATE.store(Alternate::Off as u32, Ordering::Relaxed),
            _ => return Err(format!("unknown DSP setting {item}")),
        }
    }
    Ok(())
}

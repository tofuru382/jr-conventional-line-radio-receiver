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

/// How the drift-tracking detector keeps its RLS inverse correlation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Rls {
    /// One 8x8 matrix per survivor (64), updated and copied every symbol.
    Full = 0,
    /// One matrix for all survivors, updated with the best survivor's
    /// regressor; each survivor still gets its own gain and taps.
    Shared = 1,
}
static RLS: AtomicU32 = AtomicU32::new(Rls::Shared as u32);
/// How bit reliabilities (soft decisions) are computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Soft {
    /// From the differential product of the ISI-cancelled observations.
    Differential = 0,
    /// Soft-output Viterbi: cost margins of competing trellis paths.
    Sova = 1,
    /// The smaller of both.
    Min = 2,
}
static SOFT: AtomicU32 = AtomicU32::new(Soft::Sova as u32);
pub fn soft() -> Soft {
    match SOFT.load(Ordering::Relaxed) {
        1 => Soft::Sova,
        2 => Soft::Min,
        _ => Soft::Differential,
    }
}
pub fn rls() -> Rls {
    match RLS.load(Ordering::Relaxed) {
        1 => Rls::Shared,
        _ => Rls::Full,
    }
}

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
            ("rls", "full") => RLS.store(Rls::Full as u32, Ordering::Relaxed),
            ("rls", "shared") => RLS.store(Rls::Shared as u32, Ordering::Relaxed),
            ("soft", "differential") => SOFT.store(Soft::Differential as u32, Ordering::Relaxed),
            ("soft", "sova") => SOFT.store(Soft::Sova as u32, Ordering::Relaxed),
            ("soft", "min") => SOFT.store(Soft::Min as u32, Ordering::Relaxed),
            _ => return Err(format!("unknown DSP setting {item}")),
        }
    }
    Ok(())
}

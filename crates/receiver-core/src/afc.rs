//! JR-CLRR: automatic frequency correction.
//!
//! A dongle's crystal is off by some ppm and drifts as it warms up. Every
//! channel sees the same error, and each demodulator's carrier loop follows
//! it only within ±500 Hz. The session therefore measures the error on the
//! channels that hold frame sync and walks all mixers towards it, in software
//! and in small steps that the carrier loops follow without losing lock.
//!
//! It only corrects what is already received: a dongle further off than the
//! carrier loops reach never synchronises and still needs the manual ppm.

/// Base stations differ by a few tens of Hz; inside this band nothing moves.
const START_HZ: f64 = 30.0;
/// Once moving, the correction continues until the error is this small.
const STOP_HZ: f64 = 8.0;
/// Largest change per update (one second): 10 Hz is 0.013 rad a symbol, a
/// few degrees of transient phase error in the carrier loops.
const STEP_HZ: f64 = 10.0;
/// The correction never exceeds this; a larger error means a wrong channel.
const LIMIT_HZ: f64 = 1500.0;

#[derive(Default)]
pub struct Afc {
    correction: f64,
    moving: bool,
}
impl Afc {
    /// The correction in Hz added to every channel's mixer.
    pub fn correction(&self) -> f64 {
        self.correction
    }
    /// Takes the residual carrier error (Hz) of every channel that holds
    /// frame sync, as measured with the current correction applied, and
    /// returns the new correction when it changed.
    pub fn update(&mut self, errors: &mut [f64]) -> Option<f64> {
        if errors.is_empty() || errors.iter().any(|e| !e.is_finite()) {
            return None;
        }
        // The median: one station off frequency does not move the others.
        errors.sort_by(f64::total_cmp);
        let middle = errors.len() / 2;
        let error = if errors.len() % 2 == 1 {
            errors[middle]
        } else {
            (errors[middle - 1] + errors[middle]) / 2.0
        };
        self.moving = error.abs() > if self.moving { STOP_HZ } else { START_HZ };
        if !self.moving {
            return None;
        }
        let next =
            (self.correction + (error / 2.0).clamp(-STEP_HZ, STEP_HZ)).clamp(-LIMIT_HZ, LIMIT_HZ);
        if next == self.correction {
            return None;
        }
        self.correction = next;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_to_the_median_error_and_rests_inside_the_dead_band() {
        let mut afc = Afc::default();
        // Nothing synchronised, or stations within their usual spread.
        assert_eq!(afc.update(&mut []), None);
        assert_eq!(afc.update(&mut [12.0, -25.0, 29.0]), None);
        assert_eq!(afc.update(&mut [f64::NAN]), None);
        // A dongle 200 Hz low: every channel reads about +200 Hz. One
        // station far off does not matter.
        let offset = 200.0;
        let mut seconds = 0;
        loop {
            let residual = offset - afc.correction();
            let mut errors = [
                residual + 5.0,
                residual - 4.0,
                residual,
                -400.0,
                residual + 1.0,
            ];
            let Some(next) = afc.update(&mut errors) else {
                break;
            };
            assert!((next - (offset - residual)).abs() <= STEP_HZ);
            seconds += 1;
            assert!(seconds < 60);
        }
        // 10 Hz a second, then halving steps down to the stop threshold.
        assert!((19..=24).contains(&seconds), "{seconds}");
        assert!((offset - afc.correction()).abs() <= STOP_HZ);
        // Drift back the other way is followed too, from the dead band on.
        let at = afc.correction();
        assert_eq!(afc.update(&mut [-20.0, -24.0]), None);
        assert_eq!(afc.update(&mut [-40.0, -44.0]), Some(at - 10.0));
        // Two channels: their mean.
        assert_eq!(afc.update(&mut [-10.0, -14.0]), Some(at - 16.0));
    }

    #[test]
    fn correction_is_bounded() {
        let mut afc = Afc::default();
        for _ in 0..1000 {
            afc.update(&mut [400.0]);
        }
        assert_eq!(afc.correction(), LIMIT_HZ);
        assert_eq!(afc.update(&mut [400.0]), None);
    }
}

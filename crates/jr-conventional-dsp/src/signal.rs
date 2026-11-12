//! Bounded, passive diagnostics after carrier and survivor-channel correction.
//! These samples never feed the carrier loop, timing loop or dibit decisions.
use crate::dsp::Complex32;

pub const CAPACITY: usize = 1024;

pub struct SignalView {
    previous: Option<Complex32>,
    ring: [Complex32; CAPACITY],
    head: usize,
    count: usize,
}

impl Default for SignalView {
    fn default() -> Self {
        Self {
            previous: None,
            ring: [Complex32::default(); CAPACITY],
            head: 0,
            count: 0,
        }
    }
}

impl SignalView {
    pub fn push(&mut self, symbol: Option<Complex32>) {
        // The corrected QPSK observation has had the known pi/4 alternation
        // removed. Restore that increment in differential coordinates so the
        // four displayed references retain their pi/4-DQPSK meaning. Preserve
        // amplitude/phase scatter; no slicing or per-point normalization.
        let value = self
            .previous
            .zip(symbol)
            .map_or(Complex32::default(), |(previous, symbol)| {
                let real = symbol.real * previous.real + symbol.imaginary * previous.imaginary;
                let imaginary = symbol.imaginary * previous.real - symbol.real * previous.imaginary;
                let rotation = std::f32::consts::FRAC_1_SQRT_2;
                Complex32 {
                    real: (real - imaginary) * rotation,
                    imaginary: (real + imaginary) * rotation,
                }
            });
        // Missing observations age out the display and break the differential
        // pair, rather than joining unrelated symbols across a fade.
        self.previous = symbol;
        self.ring[self.head] = value;
        self.head = (self.head + 1) % CAPACITY;
        self.count = (self.count + 1).min(CAPACITY);
    }

    /// Newest window, oldest first. RMS normalization and decision-directed
    /// EVM use exactly the returned window, including amplitude outliers.
    /// This is an uncalibrated tuning aid, not SNR, BER or a voice-quality score.
    pub fn read(&self, output: &mut [Complex32]) -> (usize, f64) {
        let count = self.count.min(output.len());
        if count == 0 {
            return (0, 0.0);
        }
        let start = (self.head + CAPACITY - count) % CAPACITY;
        let power: f64 = (0..count)
            .map(|i| {
                let z = self.ring[(start + i) % CAPACITY];
                (z.real as f64).powi(2) + (z.imaginary as f64).powi(2)
            })
            .sum::<f64>()
            / count as f64;
        if !power.is_finite() || power < 1e-12 {
            return (0, 0.0);
        }
        let amplitude = power.sqrt();
        let mut squared_error = 0.0;
        for (i, point) in output[..count].iter_mut().enumerate() {
            let z = self.ring[(start + i) % CAPACITY];
            let real = z.real as f64 / amplitude;
            let imaginary = z.imaginary as f64 / amplitude;
            let ideal = std::f64::consts::FRAC_1_SQRT_2;
            squared_error += (real.abs() - ideal).powi(2) + (imaginary.abs() - ideal).powi(2);
            *point = Complex32 {
                real: real as f32,
                imaginary: imaginary as f32,
            };
        }
        (count, 100.0 * (squared_error / count as f64).sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(jitter: f64, scale: f64) -> SignalView {
        let mut view = SignalView::default();
        let mut phase = 0.0;
        for i in 0..CAPACITY * 3 {
            phase += [0.0, 1.0, -1.0, -2.0][i % 4] * std::f64::consts::FRAC_PI_2;
            let angle = phase + if i % 2 == 0 { jitter } else { -jitter };
            view.push(Some(Complex32 {
                real: (scale * angle.cos()) as f32,
                imaginary: (scale * angle.sin()) as f32,
            }));
        }
        view
    }

    #[test]
    fn ideal_differential_symbols_have_four_unit_corners_and_gain_invariant_evm() {
        for scale in [0.2, 1.0, 3.0] {
            let mut points = [Complex32::default(); CAPACITY];
            let (count, evm) = symbols(0.0, scale).read(&mut points);
            assert_eq!(count, CAPACITY);
            assert!(evm < 0.001, "{evm}");
            for point in points {
                assert!((point.real.abs() - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
                assert!((point.imaginary.abs() - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn phase_scatter_increases_evm_and_silence_replaces_history() {
        let mut points = [Complex32::default(); CAPACITY];
        let mut view = symbols(0.15, 1.0);
        let (_, evm) = view.read(&mut points);
        assert!((evm - 200.0 * 0.15_f64.sin()).abs() < 0.001);
        for _ in 0..CAPACITY + 1 {
            view.push(None);
        }
        assert_eq!(view.read(&mut points), (0, 0.0));
        assert_eq!(SignalView::default().read(&mut points), (0, 0.0));
    }

    #[test]
    fn missing_observations_break_differential_pairs() {
        let mut view = SignalView::default();
        let mut point = [Complex32::default()];
        view.push(Some(Complex32 {
            real: 1.0,
            imaginary: 0.0,
        }));
        view.push(None);
        view.push(Some(Complex32 {
            real: 0.0,
            imaginary: 1.0,
        }));
        assert_eq!(view.read(&mut point), (0, 0.0));
        view.push(Some(Complex32 {
            real: -1.0,
            imaginary: 0.0,
        }));
        let (count, evm) = view.read(&mut point);
        assert_eq!(count, 1);
        assert!(evm < 0.001);
        assert!(point[0].real < 0.0 && point[0].imaginary > 0.0);
    }
}

//! Adaptive short-memory sequence detection with per-survivor channel estimates.
//!
//! Pi/4 alternation has already been removed. Each of the 64 states remembers
//! three QPSK symbols and a four-tap complex channel. Branch metrics use amplitude
//! as well as phase. No framing patterns or payload values are known here.
use crate::dsp::Complex32;

const STATES: usize = 64;
const DEPTH: usize = 32;
// Per-tap LMS steps for the survivor channel (main tap first). Each update is
// h += step * error * conj(symbol); for the main tap that is an exponential
// average of instantaneous estimates with this weight, so it sets how quickly
// the coherent reference follows Rayleigh phase rotation near a fade.
// - Main tap 0.12: 0.08 lagged at 40 Hz Doppler; much faster loses AWGN and
//   slow-fading margin to estimation noise.
// - One-symbol tap 0.08: in a field capture it tracks real adjacent-symbol ISI;
//   slowing it lost synchronized frames.
// - Two- and three-symbol taps 0.02: at the full step they absorb gradient noise
//   and fading-induced error, invent ISI and corrupt decisions after each fade.
// Chosen with `jr-demod-eval` sweeps (AWGN, static echoes, Rayleigh
// fading to 40 Hz, short bursts) checked against a 90 s, 22-channel capture,
// where each change also gained synchronized and valid frames.
const STEPS: [f32; 4] = [0.12, 0.08, 0.02, 0.02];
// Drift tracking: exponentially weighted RLS on each survivor's taps and
// their per-symbol drift (constant-velocity model). Extrapolating the drift
// removes the lag of plain averaging when simulcast paths fade at train
// speed; the short memory (about 1 / (1 - LAMBDA) symbols) costs noise, so
// it runs beside the LMS detector rather than replacing it.
const LAMBDA: f32 = 0.8;
// Initial inverse correlation of the taps and of their drift.
const RLS_DELTA: f32 = 4.0;
const LEVEL: f32 = std::f32::consts::FRAC_1_SQRT_2;
// Bit reliabilities saturate here; a factor of SOFT_SCALE maps the estimated
// per-bit SNR onto 0..=255 so that fades and noise rank below clean symbols.
const SOFT_SCALE: f32 = 32.0;
// Smoothing for the residual (noise) power that normalizes reliabilities.
const NOISE_SMOOTHING: f32 = 0.02;
const SYMBOLS: [Complex32; 4] = [
    Complex32 {
        real: LEVEL,
        imaginary: LEVEL,
    },
    Complex32 {
        real: -LEVEL,
        imaginary: LEVEL,
    },
    Complex32 {
        real: -LEVEL,
        imaginary: -LEVEL,
    },
    Complex32 {
        real: LEVEL,
        imaginary: -LEVEL,
    },
];

pub struct Sequence {
    costs: [f32; STATES],
    channels: [[Complex32; 4]; STATES],
    tracking: Tracking,
    // Drift-tracking state, heap-allocated (32 KiB of inverses per detector)
    // and double-buffered so survivor updates swap rather than copy.
    drift: Box<DriftState>,
    drift_next: Box<DriftState>,
    history: [[u8; STATES]; DEPTH],
    write: usize,
    filled: usize,
    previous: Option<usize>,
    equalized: Option<Complex32>,
    // Maximum-ratio combined observation of the previous symbol.
    combined: Option<Complex32>,
    noise: f32,
    // Differential bit signs and reliabilities per trellis step, emitted with
    // the traceback decision for the same step.
    soft: [Option<([u8; 2], [f32; 2])>; DEPTH],
}

impl Default for Sequence {
    fn default() -> Self {
        let mut channel = [Complex32::default(); 4];
        channel[0].real = 1.0;
        Self {
            costs: [0.0; STATES],
            channels: [channel; STATES],
            tracking: Tracking::Lms,
            drift: DriftState::boxed(),
            drift_next: DriftState::boxed(),
            history: [[0; STATES]; DEPTH],
            write: 0,
            filled: 0,
            previous: None,
            equalized: None,
            combined: None,
            noise: 0.1,
            soft: [None; DEPTH],
        }
    }
}

type Inverse8 = [[Complex32; 8]; 8];
#[derive(Clone)]
struct DriftState {
    drift: [[Complex32; 4]; STATES],
    inverse: [Inverse8; STATES],
}
impl DriftState {
    fn boxed() -> Box<Self> {
        let mut state = Box::new(Self {
            drift: [[Complex32::default(); 4]; STATES],
            inverse: [[[Complex32::default(); 8]; 8]; STATES],
        });
        for p in state.inverse.iter_mut() {
            *p = identity8();
        }
        state
    }
}
// Prior on the drift states relative to the taps.
const DRIFT_DELTA: f32 = 0.01;
fn identity8() -> Inverse8 {
    let mut p = [[Complex32::default(); 8]; 8];
    for (i, row) in p.iter_mut().enumerate() {
        row[i].real = if i < 4 { RLS_DELTA } else { DRIFT_DELTA };
    }
    p
}
fn cmul(a: Complex32, b: Complex32) -> Complex32 {
    Complex32 {
        real: a.real * b.real - a.imaginary * b.imaginary,
        imaginary: a.real * b.imaginary + a.imaginary * b.real,
    }
}
/// Advance the constant-velocity model one symbol: h += d, and
/// P = F P F^H with F = [[I, I], [0, I]].
// Matrix updates index rows and columns explicitly.
#[allow(clippy::needless_range_loop)]
fn drift_predict(h: &mut [Complex32; 4], d: &[Complex32; 4], p: &mut Inverse8) {
    for (tap, step) in h.iter_mut().zip(d) {
        tap.real += step.real;
        tap.imaginary += step.imaginary;
    }
    // Rows: top += bottom; then columns: left += right.
    for i in 0..4 {
        for j in 0..8 {
            let b = p[i + 4][j];
            p[i][j].real += b.real;
            p[i][j].imaginary += b.imaginary;
        }
    }
    for row in p.iter_mut() {
        for j in 0..4 {
            let b = row[j + 4];
            row[j].real += b.real;
            row[j].imaginary += b.imaginary;
        }
    }
}
/// RLS step on the augmented state [h; d] with regressor [u; 0].
// Matrix updates index rows and columns explicitly.
#[allow(clippy::needless_range_loop)]
fn drift_update(
    h: &mut [Complex32; 4],
    d: &mut [Complex32; 4],
    p: &mut Inverse8,
    u: &[Complex32; 4],
    error: Complex32,
) {
    let conj = |c: Complex32| Complex32 {
        real: c.real,
        imaginary: -c.imaginary,
    };
    let mut pi = [Complex32::default(); 8];
    for (i, row) in p.iter().enumerate() {
        for j in 0..4 {
            let t = cmul(row[j], conj(u[j]));
            pi[i].real += t.real;
            pi[i].imaginary += t.imaginary;
        }
    }
    let mut denominator = LAMBDA;
    for j in 0..4 {
        denominator += cmul(u[j], pi[j]).real;
    }
    if !denominator.is_finite() || denominator <= 1e-6 {
        *p = identity8();
        return;
    }
    let gain = pi.map(|v| Complex32 {
        real: v.real / denominator,
        imaginary: v.imaginary / denominator,
    });
    for i in 0..4 {
        let t = cmul(gain[i], error);
        h[i].real += t.real;
        h[i].imaginary += t.imaginary;
        let t = cmul(gain[i + 4], error);
        d[i].real += t.real;
        d[i].imaginary += t.imaginary;
    }
    let mut row = [Complex32::default(); 8];
    for (j, r) in row.iter_mut().enumerate() {
        for i in 0..4 {
            let t = cmul(u[i], p[i][j]);
            r.real += t.real;
            r.imaginary += t.imaginary;
        }
    }
    let mut next = *p;
    for i in 0..8 {
        for j in 0..8 {
            let t = cmul(gain[i], row[j]);
            next[i][j].real = (p[i][j].real - t.real) / LAMBDA;
            next[i][j].imaginary = (p[i][j].imaginary - t.imaginary) / LAMBDA;
        }
    }
    for i in 0..8 {
        for j in i..8 {
            let a = next[i][j];
            let b = next[j][i];
            let real = 0.5 * (a.real + b.real);
            let imaginary = 0.5 * (a.imaginary - b.imaginary);
            next[i][j] = Complex32 { real, imaginary };
            next[j][i] = Complex32 {
                real,
                imaginary: -imaginary,
            };
        }
    }
    let trace: f32 = (0..8).map(|i| next[i][i].real).sum();
    *p = if trace.is_finite() && trace < 1e4 {
        next
    } else {
        identity8()
    };
}
/// How a detector's survivors track their channel taps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tracking {
    /// Per-tap LMS steps: low estimation noise, follows slow change.
    Lms,
    /// Drift-extrapolating RLS: follows fast time-varying ISI at higher noise.
    Drift,
}

impl Sequence {
    pub fn new(tracking: Tracking) -> Self {
        Self {
            tracking,
            ..Self::default()
        }
    }

    /// Current observation after survivor-channel ISI and complex-gain removal.
    /// This passive, unsliced estimate uses the same pre-update channel as the
    /// branch metric; it must not feed decisions or adaptation back into the DSP.
    pub fn equalized(&self) -> Option<Complex32> {
        self.equalized
    }

    /// Advance one symbol. The output is the traceback dibit and, per bit
    /// (MSB first), a reliability: 0 means erased or contradicted, larger
    /// values mean a stronger, cleaner differential observation.
    pub fn process(
        &mut self,
        input: Complex32,
        reliable: bool,
    ) -> (Option<(u8, [u8; 2])>, f32, f32) {
        let drifting = self.tracking == Tracking::Drift;
        if drifting && reliable {
            for state in 0..STATES {
                let d = self.drift.drift[state];
                drift_predict(
                    &mut self.channels[state],
                    &d,
                    &mut self.drift.inverse[state],
                );
            }
        }
        let mut costs = [f32::INFINITY; STATES];
        let mut parents = [0; STATES];
        let mut predictions = [Complex32::default(); STATES];
        for prior in 0..STATES {
            for (current, &symbol) in SYMBOLS.iter().enumerate() {
                let symbols = [
                    symbol,
                    SYMBOLS[prior % 4],
                    SYMBOLS[(prior / 4) % 4],
                    SYMBOLS[prior / 16],
                ];
                let mut prediction = Complex32::default();
                for (h, s) in self.channels[prior].iter().zip(symbols) {
                    prediction.real += h.real * s.real - h.imaginary * s.imaginary;
                    prediction.imaginary += h.real * s.imaginary + h.imaginary * s.real;
                }
                // Missing observations advance the trellis without adding
                // evidence or training the channel on amplified fade noise.
                let error = if reliable {
                    ((input.real - prediction.real).powi(2)
                        + (input.imaginary - prediction.imaginary).powi(2))
                    .min(16.0)
                } else {
                    0.0
                };
                let cost = self.costs[prior] + error;
                let state = current + 4 * (prior % 16);
                if cost < costs[state] {
                    costs[state] = cost;
                    parents[state] = prior as u8;
                    predictions[state] = prediction;
                }
            }
        }
        // The trellis always has 64 states, so selection is non-fallible.
        // Seed with state 0 and retain the first minimum on ties, preserving
        // min_by's ordering (including non-finite costs) without an unwrap.
        let mut best = 0;
        for (state, cost) in costs.iter().enumerate().skip(1) {
            if cost.total_cmp(&costs[best]).is_lt() {
                best = state;
            }
        }
        let best_prediction = predictions[best];
        let error = Complex32 {
            real: input.real - best_prediction.real,
            imaginary: input.imaginary - best_prediction.imaginary,
        };
        let phase = (input.imaginary * best_prediction.real
            - input.real * best_prediction.imaginary)
            .atan2(input.real * best_prediction.real + input.imaginary * best_prediction.imaginary);
        // The detector emits hard dibits after traceback, not a filtered IQ
        // stream. For diagnostics, cancel the delayed paths of the current best
        // survivor from the received observation and divide by its main tap.
        // Do this BEFORE adaptation so the current noise/error is not fitted away.
        let prior = parents[best] as usize;
        let channel = &self.channels[prior];
        let main = channel[0];
        let power = main.real * main.real + main.imaginary * main.imaginary;
        // ISI-cancelled observation weighted by the conjugate main tap, so its
        // magnitude follows the instantaneous channel power (fade depth).
        let combined = (reliable && power.is_finite() && power > 1e-6)
            .then(|| {
                let mut value = input;
                for (h, s) in channel[1..].iter().zip([
                    SYMBOLS[prior % 4],
                    SYMBOLS[(prior / 4) % 4],
                    SYMBOLS[prior / 16],
                ]) {
                    value.real -= h.real * s.real - h.imaginary * s.imaginary;
                    value.imaginary -= h.real * s.imaginary + h.imaginary * s.real;
                }
                Complex32 {
                    real: value.real * main.real + value.imaginary * main.imaginary,
                    imaginary: value.imaginary * main.real - value.real * main.imaginary,
                }
            })
            .filter(|v| v.real.is_finite() && v.imaginary.is_finite());
        self.equalized = combined.map(|v| Complex32 {
            real: v.real / power,
            imaginary: v.imaginary / power,
        });
        if reliable {
            let residual = error.real * error.real + error.imaginary * error.imaginary;
            if residual.is_finite() {
                self.noise += NOISE_SMOOTHING * (residual.min(4.0) - self.noise);
            }
        }
        // Differential product of consecutive combined observations, rotated
        // by pi/4 so that MSB = (Im < 0) and LSB = (Re < 0) for the labels
        // [0, 1, 3, 2]. Dividing by the summed magnitudes and the noise power
        // leaves an SNR-like reliability that collapses in fades.
        let step_soft = match (combined, self.combined) {
            (Some(now), Some(before)) => {
                let w = Complex32 {
                    real: now.real * before.real + now.imaginary * before.imaginary,
                    imaginary: now.imaginary * before.real - now.real * before.imaginary,
                };
                let rotated = Complex32 {
                    real: LEVEL * (w.real - w.imaginary),
                    imaginary: LEVEL * (w.real + w.imaginary),
                };
                let scale = 1.0
                    / ((now.real.hypot(now.imaginary) + before.real.hypot(before.imaginary))
                        * self.noise.max(1e-4)
                        + 1e-12);
                Some((
                    [
                        u8::from(rotated.imaginary < 0.0),
                        u8::from(rotated.real < 0.0),
                    ],
                    [rotated.imaginary.abs() * scale, rotated.real.abs() * scale],
                ))
            }
            _ => None,
        };
        self.combined = combined;
        let mut channels = self.channels;

        for state in 0..STATES {
            let prior = parents[state] as usize;
            let symbols = [
                SYMBOLS[state % 4],
                SYMBOLS[prior % 4],
                SYMBOLS[(prior / 4) % 4],
                SYMBOLS[prior / 16],
            ];
            let error = Complex32 {
                real: input.real - predictions[state].real,
                imaginary: input.imaginary - predictions[state].imaginary,
            };
            channels[state] = self.channels[prior];
            if drifting {
                self.drift_next.drift[state] = self.drift.drift[prior];
                self.drift_next.inverse[state] = self.drift.inverse[prior];
            }
            if reliable && drifting {
                let next = &mut *self.drift_next;
                drift_update(
                    &mut channels[state],
                    &mut next.drift[state],
                    &mut next.inverse[state],
                    &symbols,
                    error,
                );
            } else if reliable {
                for ((h, s), step) in channels[state].iter_mut().zip(symbols).zip(STEPS) {
                    h.real += step * (error.real * s.real + error.imaginary * s.imaginary);
                    h.imaginary += step * (error.imaginary * s.real - error.real * s.imaginary);
                }
                let norm: f32 = channels[state]
                    .iter()
                    .map(|h| h.real * h.real + h.imaginary * h.imaginary)
                    .sum();
                if norm > 16.0 {
                    let gain = 4.0 / norm.sqrt();
                    for h in &mut channels[state] {
                        h.real *= gain;
                        h.imaginary *= gain;
                    }
                }
            }
        }
        self.channels = channels;
        if drifting {
            std::mem::swap(&mut self.drift, &mut self.drift_next);
        }
        // Common-offset removal bounds accumulated costs for an unlimited stream.
        self.costs = costs.map(|value| value - costs[best]);
        self.history[self.write] = parents;
        self.soft[self.write] = step_soft;
        self.write = (self.write + 1) % DEPTH;
        self.filled = (self.filled + 1).min(DEPTH);
        let mut output = None;
        if self.filled == DEPTH {
            let mut state = best;
            for back in 0..DEPTH - 1 {
                state = self.history[(self.write + DEPTH - 1 - back) % DEPTH][state] as usize;
            }
            let current = state % 4;
            let dibit = self
                .previous
                .map_or(0, |last| [0, 1, 3, 2][(current + 4 - last) % 4]);
            // The traced-back step is the oldest in the ring: index `write`.
            let soft = match (self.previous, self.soft[self.write]) {
                (Some(_), Some((signs, reliabilities))) => {
                    let mut out = [0u8; 2];
                    for (bit, out) in out.iter_mut().enumerate() {
                        let decided = (dibit >> (1 - bit)) & 1;
                        if signs[bit] == decided {
                            *out = (reliabilities[bit] * SOFT_SCALE).clamp(0.0, 255.0) as u8;
                        }
                    }
                    out
                }
                _ => [0, 0],
            };
            output = Some((dibit, soft));
            self.previous = Some(current);
        }
        (
            output,
            if reliable { phase } else { 0.0 },
            if reliable {
                error.real * error.real + error.imaginary * error.imaginary
            } else {
                1.0
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(count: usize) -> Vec<usize> {
        let mut state = 0x8123_4567_u32;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 30) as usize
            })
            .collect()
    }

    #[test]
    fn diagnostic_cancels_all_delayed_paths_and_complex_gain_without_slicing() {
        let mut detector = Sequence::default();
        let prior = 2 + 4 + 16 * 3;
        detector.costs.fill(f32::INFINITY);
        detector.costs[prior] = 0.0;
        let channel = [
            Complex32 {
                real: 0.6,
                imaginary: 0.3,
            },
            Complex32 {
                real: 0.4,
                imaginary: -0.2,
            },
            Complex32 {
                real: -0.2,
                imaginary: 0.15,
            },
            Complex32 {
                real: 0.1,
                imaginary: 0.05,
            },
        ];
        detector.channels[prior] = channel;
        // A noisy observation must remain noisy after channel correction.
        let expected = Complex32 {
            real: SYMBOLS[1].real + 0.12,
            imaginary: SYMBOLS[1].imaginary - 0.07,
        };
        let mut received = Complex32::default();
        for (h, s) in channel
            .iter()
            .zip([expected, SYMBOLS[2], SYMBOLS[1], SYMBOLS[3]])
        {
            received.real += h.real * s.real - h.imaginary * s.imaginary;
            received.imaginary += h.real * s.imaginary + h.imaginary * s.real;
        }
        assert!(detector.equalized().is_none());
        let (dibit, _, _) = detector.process(received, true);
        assert!(
            dibit.is_none(),
            "diagnostics do not wait for hard traceback"
        );
        let actual = detector.equalized().unwrap();
        assert!((actual.real - expected.real).abs() < 1e-6, "{actual:?}");
        assert!(
            (actual.imaginary - expected.imaginary).abs() < 1e-6,
            "{actual:?}"
        );

        detector.process(received, false);
        assert!(
            detector.equalized().is_none(),
            "a fade must clear the observation"
        );
    }

    #[test]
    fn diagnostic_does_not_invert_an_unusable_main_tap() {
        for gain in [0.0, 1e-4] {
            let mut detector = Sequence::default();
            for channel in &mut detector.channels {
                channel[0].real = gain;
            }
            detector.process(SYMBOLS[0], true);
            assert!(detector.equalized().is_none());
        }
    }

    #[test]
    fn random_symbols_survive_isi_and_traceback_across_a_missing_interval() {
        let transmitted = symbols(6000);
        let mut detector = Sequence::default();
        let mut recovered = Vec::new();
        for (i, &q) in transmitted.iter().enumerate() {
            let previous = SYMBOLS[transmitted[i.saturating_sub(1)]];
            let sample = Complex32 {
                real: (SYMBOLS[q].real + 0.6 * previous.real) / 1.1661904,
                imaginary: (SYMBOLS[q].imaginary + 0.6 * previous.imaginary) / 1.1661904,
            };
            let reliable = !(2000..2240).contains(&i);
            let (value, _, _) = detector.process(
                if reliable {
                    sample
                } else {
                    Complex32::default()
                },
                reliable,
            );
            if let Some((dibit, _)) = value {
                recovered.push(dibit);
            }
            assert!(detector
                .costs
                .iter()
                .all(|cost| cost.is_finite() && *cost >= 0.0));
        }
        assert_eq!(recovered.len(), transmitted.len() - DEPTH + 1);
        for i in (500..1900).chain(2500..recovered.len()) {
            assert_eq!(
                recovered[i],
                [0, 1, 3, 2][(transmitted[i] + 4 - transmitted[i - 1]) % 4],
                "symbol {i}"
            );
        }
    }

    #[test]
    fn reliabilities_follow_signal_strength_and_vanish_when_erased() {
        let transmitted = symbols(3000);
        let mut detector = Sequence::default();
        let mut strong = Vec::new();
        let mut weak = Vec::new();
        let mut erased = Vec::new();
        for (i, &q) in transmitted.iter().enumerate() {
            // Clean, then 20 dB weaker with the same noise, then missing.
            let (gain, reliable) = match i {
                0..1000 => (1.0, true),
                1000..2000 => (0.1, true),
                _ => (0.0, false),
            };
            let noise = 0.2 * (((i * 7919) % 101) as f32 / 50.0 - 1.0);
            let sample = Complex32 {
                real: gain * SYMBOLS[q].real + noise,
                imaginary: gain * SYMBOLS[q].imaginary - noise,
            };
            if let (Some((_, soft)), _, _) = detector.process(sample, reliable) {
                let emitted = i + 1 - DEPTH;
                let bucket = match emitted {
                    200..1000 => &mut strong,
                    1300..2000 => &mut weak,
                    2100.. => &mut erased,
                    _ => continue,
                };
                bucket.extend(soft.map(u32::from));
            }
        }
        let mean = |v: &Vec<u32>| v.iter().sum::<u32>() as f64 / v.len() as f64;
        assert!(
            mean(&strong) > 2.0 * mean(&weak),
            "{} {}",
            mean(&strong),
            mean(&weak)
        );
        assert!(mean(&weak) > 0.0);
        assert!(erased.iter().all(|&r| r == 0));
    }

    #[test]
    fn drift_tracking_follows_two_paths_rotating_at_different_rates() {
        // Main and one-symbol paths turning in opposite directions, as two
        // simulcast stations beat while a train moves. Averaging (LMS) lags;
        // extrapolating each tap's drift keeps up.
        let transmitted = symbols(4000);
        let errors = |tracking: Tracking| {
            let mut detector = Sequence::new(tracking);
            let mut wrong = 0;
            for (i, &q) in transmitted.iter().enumerate() {
                let rotate = |gain: f32, rate: f32, s: Complex32| {
                    let (sin, cos) = (rate * i as f32).sin_cos();
                    Complex32 {
                        real: gain * (s.real * cos - s.imaginary * sin),
                        imaginary: gain * (s.real * sin + s.imaginary * cos),
                    }
                };
                let a = rotate(1.0, 0.06, SYMBOLS[q]);
                let b = rotate(0.9, -0.05, SYMBOLS[transmitted[i.saturating_sub(1)]]);
                let sample = Complex32 {
                    real: (a.real + b.real) / 1.35,
                    imaginary: (a.imaginary + b.imaginary) / 1.35,
                };
                if let (Some((dibit, _)), _, _) = detector.process(sample, true) {
                    let at = i + 1 - DEPTH;
                    if at > 500
                        && dibit != [0, 1, 3, 2][(transmitted[at] + 4 - transmitted[at - 1]) % 4]
                    {
                        wrong += 1;
                    }
                }
            }
            wrong
        };
        let (lms, drift) = (errors(Tracking::Lms), errors(Tracking::Drift));
        assert!(drift * 4 < lms.max(1), "drift {drift} lms {lms}");
    }

    #[test]
    fn long_silence_preserves_bounded_channel_state_and_recovers() {
        let mut detector = Sequence::default();
        for _ in 0..10000 {
            detector.process(Complex32::default(), false);
        }
        assert!(detector.channels.iter().all(|h| h[0].real == 1.0));
        let transmitted = symbols(1000);
        let output: Vec<_> = transmitted
            .iter()
            .filter_map(|&q| detector.process(SYMBOLS[q], true).0.map(|(d, _)| d))
            .collect();
        for (i, &dibit) in output.iter().enumerate().skip(100) {
            let at = i + 1 - DEPTH;
            assert_eq!(
                dibit,
                [0, 1, 3, 2][(transmitted[at] + 4 - transmitted[at - 1]) % 4]
            );
        }
    }
}

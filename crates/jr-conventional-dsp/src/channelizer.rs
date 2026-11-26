//! Shared polyphase DFT channelizer: one pass over the wideband IQ produces a
//! narrowband stream for every receive channel, instead of each channel
//! mixing and filtering the full-rate input on its own.
//!
//! The band is split into `M` bins of `rate / M` Hz by an `L`-tap-per-branch
//! polyphase filter and decimated by `D = M / 4` (four times oversampled), so
//! each bin's output rate `rate / D` is wide enough for a channel anywhere in
//! its bin: the residual offset (up to half a bin, about 3 kHz) plus the
//! channel filter's 4.2 kHz stays below the output Nyquist frequency. Only
//! the bins in use are computed. Each channel's demodulator then runs at the
//! low rate with its residual offset.
use crate::tuning::{self, Bins};
use crate::Complex32;
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::{f64::consts::PI, sync::Arc};

/// Prototype taps per polyphase branch.
const TAPS_PER_BRANCH: usize = 6;
/// Target output rate per channel (S/s).
const OUTPUT_RATE: f64 = 24_000.0;

/// The decimation: an output rate near [`OUTPUT_RATE`] (22-26 kS/s all
/// work with the filter below), preferring a factor with no prime above 7 so
/// the FFT over `4 D` bins is fast (1.024 MS/s would otherwise give 4 x 43).
fn decimation(rate: f64) -> Option<usize> {
    if !rate.is_finite() || rate < 4.0 * OUTPUT_RATE {
        return None;
    }
    let smooth = |mut n: usize| {
        for p in [2, 3, 5, 7] {
            while n.is_multiple_of(p) {
                n /= p;
            }
        }
        n == 1
    };
    let ideal = rate / OUTPUT_RATE;
    let (low, high) = (
        (rate / 26_000.0).ceil() as usize,
        (rate / 22_000.0).floor() as usize,
    );
    (low.max(4)..=high)
        .filter(|&d| smooth(d))
        .min_by(|a, b| {
            (*a as f64 - ideal)
                .abs()
                .total_cmp(&(*b as f64 - ideal).abs())
        })
        .or(Some((ideal.round() as usize).max(4)))
}

pub struct Channelizer {
    m: usize,
    d: usize,
    rate: f64,
    /// Prototype taps by row (see `new`).
    taps: Vec<f32>,
    /// One output's branch sums, branch M - 1 first.
    sums: Vec<Complex<f32>>,
    /// The last `M L` input samples, newest last (a ring with a mirror so
    /// any window is contiguous).
    history: Vec<Complex32>,
    write: usize,
    /// Input samples until the next output.
    countdown: usize,
    /// Output samples so far (for the bin rotation).
    produced: u64,
    bins: Vec<Bin>,
    /// Branch outputs of the current block: `M` values per output sample.
    frames: Vec<Complex<f32>>,
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Mean power of every bin (not only those in use), summed since the
    /// last [`Channelizer::take_bin_power`], while measuring.
    measuring: bool,
    power: Vec<f64>,
    power_outputs: u64,
}
struct Bin {
    k: usize,
    /// `exp(j 2 pi k p / M)` for p in 0..M.
    twiddles: Vec<Complex<f32>>,
}
/// Where a channel lands: its bin, its output rate and the remaining offset
/// from the bin centre that its own mixer must still remove.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub bin: usize,
    pub rate: f64,
    pub residual_hz: f64,
}

impl Channelizer {
    /// A channelizer for `rate` S/s input, or `None` when the input is too
    /// slow for one to help (below four outputs per bin).
    pub fn new(rate: f64) -> Option<Self> {
        let d = decimation(rate)?;
        let m = 4 * d;
        let length = m * TAPS_PER_BRANCH;
        // Blackman-Harris windowed sinc cut off at half the output rate,
        // midway between the passband edge (half a bin, the frequency
        // correction and a channel: about 8.7 kHz) and the nearest frequency
        // that aliases onto the signal: flat to 0.01 dB in the passband, over
        // 110 dB down where it could alias, at every input rate.
        let cutoff = rate / d as f64 / 2.0;
        let omega = 2.0 * PI * cutoff / rate;
        let mut h: Vec<f64> = (0..length)
            .map(|i| {
                let x = i as f64 - (length - 1) as f64 / 2.0;
                let sinc = if x == 0.0 {
                    omega
                } else {
                    (omega * x).sin() / x
                };
                let a = 2.0 * PI * i as f64 / (length - 1) as f64;
                let window = 0.355768 - 0.487396 * a.cos() + 0.144232 * (2.0 * a).cos()
                    - 0.012604 * (3.0 * a).cos();
                sinc * window
            })
            .collect();
        let sum: f64 = h.iter().sum();
        for v in &mut h {
            *v /= sum;
        }
        // Branch p sums x[n - p - l M] h[p + l M]. Laid out so that for each
        // l the M products run over contiguous input: element i of row l
        // multiplies x[n - l M - (M - 1) + i] and belongs to branch M - 1 - i.
        let taps = (0..TAPS_PER_BRANCH)
            .flat_map(|l| (0..m).map(move |i| (l, i)))
            .map(|(l, i)| h[(m - 1 - i) + l * m] as f32)
            .collect();
        // sum_p v_p exp(+j 2 pi k p / M) is an unnormalised inverse DFT.
        let fft = FftPlanner::new().plan_fft_inverse(m);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Some(Self {
            m,
            d,
            rate,
            taps,
            sums: vec![Complex::default(); m],
            history: vec![Complex32::default(); 2 * length],
            write: 0,
            countdown: d,
            produced: 0,
            bins: Vec::new(),
            frames: Vec::new(),
            fft,
            scratch,
            measuring: false,
            power: vec![0.0; m],
            power_outputs: 0,
        })
    }
    pub fn output_rate(&self) -> f64 {
        self.rate / self.d as f64
    }
    /// Input samples per output sample.
    pub fn decimation(&self) -> usize {
        self.d
    }
    /// Number of bins (the FFT size) and their spacing in Hz.
    pub fn bins(&self) -> usize {
        self.m
    }
    pub fn bin_spacing(&self) -> f64 {
        self.rate / self.m as f64
    }
    /// Starts or stops summing every bin's power (costs one pass over the
    /// FFT output; needs the FFT, see [`Bins`]).
    pub fn set_measuring(&mut self, on: bool) {
        self.measuring = on;
        self.power.fill(0.0);
        self.power_outputs = 0;
    }
    /// Mean power per bin (index k: k bins above the centre, wrapping; a
    /// full-scale tone at a bin centre reads 1) since the last call, or None
    /// when nothing was measured.
    pub fn take_bin_power(&mut self) -> Option<Vec<f64>> {
        if self.power_outputs == 0 {
            return None;
        }
        let n = self.power_outputs as f64;
        let mean = self.power.iter().map(|p| p / n).collect();
        self.power.fill(0.0);
        self.power_outputs = 0;
        Some(mean)
    }
    /// Forgets the input so far (after lost samples).
    pub fn reset(&mut self) {
        self.history.fill(Complex32::default());
        self.countdown = self.d;
    }

    /// The bin and residual offset for a channel `offset_hz` from the input
    /// centre; the bin is computed from now on.
    pub fn place(&mut self, offset_hz: f64) -> Placement {
        let spacing = self.rate / self.m as f64;
        let signed = (offset_hz / spacing).round() as i64;
        let k = signed.rem_euclid(self.m as i64) as usize;
        if !self.bins.iter().any(|b| b.k == k) {
            let twiddles = (0..self.m)
                .map(|p| {
                    let a = 2.0 * PI * ((k * p) % self.m) as f64 / self.m as f64;
                    Complex::new(a.cos() as f32, a.sin() as f32)
                })
                .collect();
            self.bins.push(Bin { k, twiddles });
        }
        Placement {
            bin: k,
            rate: self.output_rate(),
            residual_hz: offset_hz - signed as f64 * spacing,
        }
    }
    /// Filters `input` and appends each placed bin's output to `outputs`
    /// (indexed by bin, created on demand).
    pub fn process(&mut self, input: &[Complex32], outputs: &mut Vec<(usize, Vec<Complex32>)>) {
        let (m, length) = (self.m, self.m * TAPS_PER_BRANCH);
        for bin in &self.bins {
            if !outputs.iter().any(|(k, _)| *k == bin.k) {
                outputs.push((bin.k, Vec::new()));
            }
        }
        self.frames.clear();
        for &sample in input {
            self.history[self.write] = sample;
            self.history[self.write + length] = sample;
            self.write = (self.write + 1) % length;
            self.countdown -= 1;
            if self.countdown != 0 {
                continue;
            }
            self.countdown = self.d;
            // The mirrored ring keeps the last `length` inputs contiguous,
            // oldest first, ending at `end`.
            let end = self.write + length;
            self.sums.fill(Complex::default());
            for (l, row) in self.taps.chunks_exact(m).enumerate() {
                let x = &self.history[end - (l + 1) * m..end - l * m];
                for ((acc, &tap), x) in self.sums.iter_mut().zip(row).zip(x) {
                    acc.re += tap * x.real;
                    acc.im += tap * x.imaginary;
                }
            }
            self.frames.extend(self.sums.iter().rev());
        }
        let count = self.frames.len() / m;
        let fft = tuning::bins() == Bins::Fft;
        if fft {
            self.fft
                .process_with_scratch(&mut self.frames, &mut self.scratch);
            if self.measuring && count > 0 {
                for frame in self.frames.chunks_exact(m) {
                    for (sum, v) in self.power.iter_mut().zip(frame) {
                        *sum += f64::from(v.norm_sqr());
                    }
                }
                self.power_outputs += count as u64;
            }
        }
        for j in 0..count {
            let frame = &self.frames[j * m..(j + 1) * m];
            // y_k[n] = exp(-j w_k n D) sum_p v_p exp(j 2 pi k p / M); with
            // D = M / 4 the rotation repeats every four outputs.
            let quarter = ((self.produced + j as u64) % 4) as usize;
            for bin in &self.bins {
                let acc = if fft {
                    frame[bin.k]
                } else {
                    frame
                        .iter()
                        .zip(&bin.twiddles)
                        .fold(Complex::default(), |acc, (v, w)| acc + v * w)
                };
                let rotated = match (bin.k * quarter) % 4 {
                    0 => acc,
                    1 => Complex::new(acc.im, -acc.re),
                    2 => -acc,
                    _ => Complex::new(-acc.im, acc.re),
                };
                if let Some((_, out)) = outputs.iter_mut().find(|(k, _)| *k == bin.k) {
                    out.push(Complex32 {
                        real: rotated.re,
                        imaginary: rotated.im,
                    });
                }
            }
        }
        self.produced += count as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, hz: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let a = 2.0 * PI * hz * i as f64 / rate;
                Complex32 {
                    real: a.cos() as f32,
                    imaginary: a.sin() as f32,
                }
            })
            .collect()
    }
    fn power(x: &[Complex32]) -> f64 {
        x.iter()
            .map(|s| (s.real as f64).powi(2) + (s.imaginary as f64).powi(2))
            .sum::<f64>()
            / x.len() as f64
    }

    /// The bins setting is process-wide: a test that switches it to the
    /// direct sums holds this, and so does one that needs the FFT (bin
    /// power is measured only there), or the two race when run in parallel.
    static BINS_SETTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn bin_power_shows_a_tone_in_its_bin_at_full_scale() {
        let _fft = BINS_SETTING.lock().unwrap_or_else(|e| e.into_inner());
        let rate = 240_000.0;
        let mut c = Channelizer::new(rate).unwrap();
        c.set_measuring(true);
        let mut out = Vec::new();
        // 5 bins below the centre (bin 35 of 40), after the filter filled.
        let x = tone(rate, -5.0 * c.bin_spacing(), 48_000);
        c.process(&x[..2400], &mut out);
        c.take_bin_power();
        c.process(&x[2400..], &mut out);
        let power = c.take_bin_power().unwrap();
        assert_eq!(power.len(), 40);
        assert!((power[35] - 1.0).abs() < 0.02, "{}", power[35]);
        // Bins overlap (each passes ±12 kHz): one bin away the tone is
        // still there, three away it is gone.
        assert!(power[34] > 0.5 && power[36] > 0.5);
        for k in [0, 5, 31, 39] {
            assert!(power[k] < 1e-9, "bin {k}: {}", power[k]);
        }
        assert_eq!(c.take_bin_power(), None);
    }

    #[test]
    fn fft_and_direct_bins_agree() {
        let _switching = BINS_SETTING.lock().unwrap_or_else(|e| e.into_inner());
        let rate = 1_024_000.0;
        let input = tone(rate, 41_000.0, 20_000);
        let run = |mode: &str| {
            tuning::parse(mode).unwrap();
            let mut c = Channelizer::new(rate).unwrap();
            for f in [-60_000.0, 3125.0, 41_000.0] {
                c.place(f);
            }
            let mut out = Vec::new();
            c.process(&input[..7_000], &mut out);
            c.process(&input[7_000..], &mut out);
            tuning::parse("bins=fft").unwrap();
            out
        };
        let (direct, fft) = (run("bins=direct"), run("bins=fft"));
        for ((k, a), (l, b)) in direct.iter().zip(&fft) {
            assert_eq!(k, l);
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b) {
                assert!((x.real - y.real).abs() < 1e-4 && (x.imaginary - y.imaginary).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn a_channel_tone_appears_at_its_residual_and_neighbours_are_rejected() {
        let rate = 240_000.0;
        let mut c = Channelizer::new(rate).unwrap();
        assert_eq!(c.output_rate(), 24_000.0);
        assert_eq!(decimation(1_024_000.0), Some(42));
        assert_eq!(decimation(2_400_000.0), Some(100));
        assert_eq!(decimation(250_000.0), Some(10));
        assert_eq!(decimation(90_000.0), None);
        // A channel 3.125 kHz + 4 x 6.25 kHz from the centre.
        let offset = 3125.0 + 25_000.0;
        let place = c.place(offset);
        let mut out = Vec::new();
        // The tone sits 1 kHz inside the channel.
        c.process(&tone(rate, offset + 1000.0, 48_000), &mut out);
        let y = &out[0].1[200..];
        assert!((power(y) - 1.0).abs() < 0.05, "passband gain {}", power(y));
        // Its frequency at the output is the residual plus 1 kHz.
        let expected = place.residual_hz + 1000.0;
        let mut phase = 0.0;
        for w in y.windows(2) {
            let (a, b) = (w[0], w[1]);
            let re = a.real as f64 * b.real as f64 + a.imaginary as f64 * b.imaginary as f64;
            let im = a.real as f64 * b.imaginary as f64 - a.imaginary as f64 * b.real as f64;
            phase += im.atan2(re);
        }
        let measured = phase / (y.len() - 1) as f64 * 24_000.0 / (2.0 * PI);
        assert!(
            (measured - expected).abs() < 5.0,
            "{measured} vs {expected}"
        );
        // Neighbouring channels pass (the demodulator's own filter removes
        // them), but nothing may alias onto the channel: a tone one output
        // rate away would land exactly on the 1 kHz tone above.
        for away in [
            24_000.0,
            -24_000.0,
            30_000.0,
            16_800.0 - place.residual_hz - 1000.0,
        ] {
            let mut out = Vec::new();
            let mut c = Channelizer::new(rate).unwrap();
            c.place(offset);
            c.process(&tone(rate, offset + 1000.0 + away, 48_000), &mut out);
            let leak = power(&out[0].1[200..]);
            assert!(leak < 1e-6, "{away} Hz away: {leak}");
        }
    }
}

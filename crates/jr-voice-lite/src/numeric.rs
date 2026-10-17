//! Binary32 kernels. Keep reduction order and separate multiply/add rounding.
use crate::models::Tensor;
use std::sync::OnceLock;
pub const PI: f32 = std::f32::consts::PI;

pub fn affine(input: &[f32], weight: Tensor, bias: &[f32], output: &mut [f32]) {
    affine_columns(input, weight, bias, output, 0, weight.cols);
}
pub fn affine_columns(
    input: &[f32],
    weight: Tensor,
    bias: &[f32],
    output: &mut [f32],
    offset: usize,
    columns: usize,
) {
    output[..columns].fill(0.0);
    if !weight.sparse_offsets.is_empty() {
        for j in 0..columns {
            let column = offset + j;
            let mut sum = 0.0;
            for p in
                weight.sparse_offsets[column] as usize..weight.sparse_offsets[column + 1] as usize
            {
                sum += input[weight.sparse_rows[p] as usize] * weight.quantized[p] as f32;
            }
            output[j] = sum * weight.quantum;
        }
    } else if !weight.quantized.is_empty() {
        for j in 0..columns {
            let mut sum = 0.0;
            for i in 0..weight.rows {
                sum += input[i] * weight.quantized[i * weight.cols + offset + j] as f32;
            }
            output[j] = sum * weight.quantum;
        }
    } else {
        for i in 0..weight.rows {
            for j in 0..columns {
                output[j] += input[i] * weight.data[i * weight.cols + offset + j];
            }
        }
    }
    if !bias.is_empty() {
        for j in 0..columns {
            output[j] += bias[j];
        }
    }
}
pub fn normalize(input: &[f32], mean: Tensor, scale: Tensor, output: &mut [f32], clip: bool) {
    for i in 0..mean.rows {
        let x = (input[i] - mean.get(i)) / scale.get(i);
        output[i] = if clip { x.clamp(-6.0, 6.0) } else { x };
    }
}
pub fn normalize_in_place(input: &mut [f32], mean: Tensor, scale: Tensor, clip: bool) {
    for i in 0..mean.rows {
        let x = (input[i] - mean.get(i)) / scale.get(i);
        input[i] = if clip { x.clamp(-6.0, 6.0) } else { x };
    }
}
pub fn round_even(value: f32) -> f32 {
    let lower = value.floor();
    let fraction = value - lower;
    if fraction < 0.5 || (fraction == 0.5 && lower % 2.0 == 0.0) {
        lower
    } else {
        lower + 1.0
    }
}
pub fn sample(value: f32) -> i16 {
    round_even(value * 32768.0).clamp(-32768.0, 32767.0) as i16
}

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f32,
    im: f32,
}
fn rotations() -> &'static [Complex; 256] {
    static ROOTS: OnceLock<[Complex; 256]> = OnceLock::new();
    ROOTS.get_or_init(|| {
        std::array::from_fn(|i| {
            let angle = -2.0 * PI * i as f32 / 512.0;
            Complex {
                re: angle.cos(),
                im: angle.sin(),
            }
        })
    })
}
pub struct Numeric {
    work: [Complex; 512],
}
impl Default for Numeric {
    fn default() -> Self {
        Self {
            work: [Complex::default(); 512],
        }
    }
}
impl Numeric {
    fn fft(&mut self, inverse: bool) {
        let roots = rotations();
        let mut j = 0;
        for i in 1..512 {
            let mut bit = 256;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j ^= bit;
            if i < j {
                self.work.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= 512 {
            for j in 0..size / 2 {
                let mut rotation = roots[j * (512 / size)];
                if inverse {
                    rotation.im = -rotation.im;
                }
                for offset in (0..512).step_by(size) {
                    let even = self.work[offset + j];
                    let value = self.work[offset + j + size / 2];
                    let odd = Complex {
                        re: value.re * rotation.re - value.im * rotation.im,
                        im: value.re * rotation.im + value.im * rotation.re,
                    };
                    self.work[offset + j] = Complex {
                        re: even.re + odd.re,
                        im: even.im + odd.im,
                    };
                    self.work[offset + j + size / 2] = Complex {
                        re: even.re - odd.re,
                        im: even.im - odd.im,
                    };
                }
            }
            size <<= 1;
        }
        if inverse {
            for value in &mut self.work {
                value.re /= 512.0;
                value.im /= 512.0;
            }
        }
    }
    fn symmetric_spectrum(&mut self, cepstrum: &[f32]) {
        self.work.fill(Complex::default());
        for i in 0..32 {
            self.work[i + 1].re = cepstrum[i];
            self.work[511 - i].re = cepstrum[i];
        }
        self.fft(false);
    }
    pub fn bound_correction(&mut self, correction: &mut [f32]) {
        for x in &mut correction[..32] {
            *x = x.clamp(-0.25, 0.25);
        }
        self.symmetric_spectrum(correction);
        let peak = self
            .work
            .iter()
            .fold(0.0_f32, |peak, x| peak.max(x.re.abs()));
        let limit = 6.0 * 10.0_f32.ln() / 10.0;
        if peak > limit {
            for x in &mut correction[..32] {
                *x *= limit / peak;
            }
        }
    }
    pub fn lpc_cepstrum(&mut self, reflection: &[f32], output: &mut [f32]) {
        let mut denominator = [0.0; 11];
        denominator[0] = 1.0;
        for k in 0..10 {
            let r = reflection[k].tanh().clamp(-0.96, 0.96);
            let mut next = [0.0; 11];
            next[..k + 1].copy_from_slice(&denominator[..k + 1]);
            for i in 0..=k {
                next[i + 1] += r * denominator[k - i];
            }
            denominator = next;
        }
        for n in 1..=32 {
            let mut value = if n <= 10 { -denominator[n] } else { 0.0 };
            for k in 1..=(n - 1).min(10) {
                value -= (1.0 - k as f32 / n as f32) * denominator[k] * output[n - k - 1];
            }
            output[n - 1] = value;
        }
    }
    pub fn envelope_impulse(&mut self, shape: &[f32], output: &mut [f32; 256]) -> bool {
        let mut bounded = [0.0; 32];
        let mut absolute_bound = 0.0;
        for i in 0..32 {
            bounded[i] = shape[i].clamp(-30.0, 30.0);
            absolute_bound += 2.0 * bounded[i].abs();
        }
        if absolute_bound <= 29.0 {
            let weighted: [f32; 32] = std::array::from_fn(|k| (k + 1) as f32 * bounded[k]);
            output[0] = 1.0;
            let mut norm = 1.0;
            for n in 1..256 {
                let mut sum = 0.0;
                for k in 1..=n.min(32) {
                    sum += weighted[k - 1] * output[n - k];
                }
                output[n] = sum / n as f32;
                norm += output[n] * output[n];
            }
            norm = norm.sqrt();
            if norm.is_finite() && norm > 0.0 {
                for x in output {
                    *x /= norm;
                }
                return true;
            }
        }
        if absolute_bound <= 29.0 {
            self.work.fill(Complex::default());
            for (i, &x) in bounded.iter().enumerate() {
                self.work[i + 1].re = x;
            }
        } else {
            self.symmetric_spectrum(&bounded);
            for value in &mut self.work {
                *value = Complex {
                    re: value.re.clamp(-30.0, 30.0),
                    im: 0.0,
                };
            }
            self.fft(true);
            self.work[0] = Complex::default();
            for i in 1..256 {
                self.work[i].im = 0.0;
            }
            self.work[256] = Complex {
                re: self.work[256].re * 0.5,
                im: 0.0,
            };
            self.work[257..].fill(Complex::default());
        }
        self.fft(false);
        for value in &mut self.work {
            let radius = value.re.exp();
            *value = Complex {
                re: radius * value.im.cos(),
                im: radius * value.im.sin(),
            };
        }
        self.fft(true);
        let mut norm = 0.0;
        for (i, x) in output.iter_mut().enumerate() {
            *x = self.work[i].re;
            norm += *x * *x;
        }
        norm = norm.sqrt();
        if !norm.is_finite() || norm <= 0.0 {
            return false;
        }
        for x in output {
            *x /= norm;
        }
        true
    }
}

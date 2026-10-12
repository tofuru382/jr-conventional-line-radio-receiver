//! Source whitening, coherent history and spectral observations for synthesis.
use crate::predictor::Controls;
use crate::vocoder::VoiceNoise;
use std::sync::OnceLock;

struct Spectrum {
    window: [f64; 160],
    window_power: f64,
    bank: [[f64; 257]; 20],
    rotation: [[f64; 2]; 256],
}
fn spectrum() -> &'static Spectrum {
    static VALUE: OnceLock<Spectrum> = OnceLock::new();
    VALUE.get_or_init(|| {
        let window = std::array::from_fn(|i| {
            0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 159.0).cos()
        });
        let mut bank = [[0.0; 257]; 20];
        let edges: [f64; 22] = std::array::from_fn(|i| i as f64 * (4000.0 / 21.0));
        for b in 0..20 {
            for k in 0..257 {
                let f = k as f64 * (8000.0 / 512.0);
                bank[b][k] = ((f - edges[b]) / (edges[b + 1] - edges[b]))
                    .min((edges[b + 2] - f) / (edges[b + 2] - edges[b + 1]))
                    .max(0.0);
            }
            let sum: f64 = bank[b].iter().sum();
            for x in &mut bank[b] {
                *x /= sum;
            }
        }
        let rotation = std::array::from_fn(|i| {
            let t = -2.0 * std::f64::consts::PI * i as f64 / 512.0;
            [t.cos(), t.sin()]
        });
        Spectrum {
            window_power: window.iter().map(|x| x * x).sum(),
            window,
            bank,
            rotation,
        }
    })
}
pub fn fft(values: &mut [[f64; 2]; 512]) {
    let constants = spectrum();
    for i in 0_usize..512 {
        let j = i.reverse_bits() >> (usize::BITS - 9);
        if i < j {
            values.swap(i, j);
        }
    }
    for shift in 1..=9 {
        let size = 1 << shift;
        for begin in (0..512).step_by(size) {
            for k in 0..size / 2 {
                let w = constants.rotation[k * 512 / size];
                let b = values[begin + k + size / 2];
                let v = [b[0] * w[0] - b[1] * w[1], b[0] * w[1] + b[1] * w[0]];
                let a = values[begin + k];
                values[begin + k] = [a[0] + v[0], a[1] + v[1]];
                values[begin + k + size / 2] = [a[0] - v[0], a[1] - v[1]];
            }
        }
    }
}
pub fn describe(audio: &[f64; 320]) -> [[f64; 22]; 2] {
    let c = spectrum();
    let mut out = [[0.0; 22]; 2];
    for h in 0..2 {
        let mut bins = [[0.0; 2]; 512];
        for i in 0..160 {
            bins[i][0] = audio[h * 160 + i] * c.window[i];
        }
        fft(&mut bins);
        let power: [f64; 257] = std::array::from_fn(|i| {
            (bins[i][0] * bins[i][0] + bins[i][1] * bins[i][1]) / c.window_power
        });
        for b in 0..20 {
            let sum: f64 = (0..257).map(|i| power[i] * c.bank[b][i]).sum();
            out[h][b] = 10.0 * sum.max(1e-12).log10();
        }
        let mean = out[h][..20].iter().sum::<f64>() / 20.0;
        for b in 0..20 {
            out[h][b] -= mean;
        }
        for b in 0..2 {
            let offset = h * 160 + b * 80;
            let energy = audio[offset..offset + 80]
                .iter()
                .map(|x| x * x)
                .sum::<f64>()
                / 80.0;
            out[h][20 + b] = 10.0 * energy.max(1e-12).log10();
        }
    }
    out
}
pub fn pcm_descriptor(pcm: &[i16; 320]) -> [[f32; 22]; 2] {
    let x = pcm.map(|x| x as f64 / 32768.0);
    describe(&x).map(|r| r.map(|x| x as f32))
}
pub fn periodicity(pcm: &[i16; 320]) -> f64 {
    let mean = pcm.iter().map(|&x| x as f64).sum::<f64>() / 320.0;
    let x = pcm.map(|x| x as f64 - mean);
    let mut peak = -1.0_f64;
    for lag in 20..=160 {
        let (mut dot, mut a, mut b) = (0.0, 0.0, 0.0);
        for i in 0..320 - lag {
            dot += x[i] * x[i + lag];
            a += x[i] * x[i];
            b += x[i + lag] * x[i + lag];
        }
        peak = peak.max((dot / (a * b).max(1e-30).sqrt()).clamp(-1.0, 1.0));
    }
    peak
}
pub struct Components {
    pub white64: [f64; 320],
    pub native: [f32; 320],
    pub periodic: [f32; 320],
    pub noise: [f32; 320],
}
pub struct Sources {
    raw_past: [f64; 10],
    white_past: [f32; 480],
    valid: usize,
    noise: VoiceNoise,
    mass: Option<[f64; 3]>,
}
impl Default for Sources {
    fn default() -> Self {
        Self {
            raw_past: [0.0; 10],
            white_past: [0.0; 480],
            valid: 0,
            noise: VoiceNoise::default(),
            mass: None,
        }
    }
}
impl Sources {
    pub fn components(&mut self, raw: &[i16; 320], controls: &Controls) -> Components {
        let mut normalized = raw.map(|x| x as f64);
        for b in 0..4 {
            let x = &mut normalized[b * 80..(b + 1) * 80];
            let rms = (x.iter().map(|v| v * v).sum::<f64>() / 80.0)
                .sqrt()
                .max(1e-12);
            for v in x {
                *v /= rms;
            }
        }
        let mut r = [0.0; 11];
        for lag in 0..=10 {
            r[lag] = (0..320 - lag)
                .map(|i| normalized[i] * normalized[i + lag])
                .sum::<f64>()
                / 320.0;
        }
        r[0] = (1.01 * r[0]).max(1e-8);
        let mut matrix = [[0.0; 11]; 10];
        for i in 0_usize..10 {
            for j in 0_usize..10 {
                matrix[i][j] = r[i.abs_diff(j)];
            }
            matrix[i][10] = -r[i + 1];
        }
        for k in 0..10 {
            let pivot = (k..10)
                .max_by(|&a, &b| matrix[a][k].abs().total_cmp(&matrix[b][k].abs()))
                .unwrap();
            matrix.swap(k, pivot);
            for i in k + 1..10 {
                let ratio = matrix[i][k] / matrix[k][k];
                for j in k..=10 {
                    matrix[i][j] -= ratio * matrix[k][j];
                }
            }
        }
        let mut coef = [0.0; 10];
        for i in (0..10).rev() {
            let rhs = matrix[i][10] - (i + 1..10).map(|j| matrix[i][j] * coef[j]).sum::<f64>();
            coef[i] = rhs / matrix[i][i];
        }
        for k in 0..10 {
            coef[k] *= 0.98_f64.powi(k as i32 + 1);
        }
        let mut extended = [0.0; 330];
        extended[..10].copy_from_slice(&self.raw_past);
        extended[10..].copy_from_slice(&normalized);
        self.raw_past.copy_from_slice(&normalized[310..]);
        let mut white64 = [0.0; 320];
        for i in 0..320 {
            white64[i] = normalized[i];
            for k in 0..10 {
                white64[i] += coef[k] * extended[i + 9 - k];
            }
        }
        for b in 0..4 {
            let x = &mut white64[b * 80..(b + 1) * 80];
            let rms = (x.iter().map(|v| v * v).sum::<f64>() / 80.0)
                .sqrt()
                .max(1e-12);
            for v in x {
                *v /= rms;
            }
        }
        let native = white64.map(|x| x as f32);
        let mut history = [0.0_f64; 800];
        for i in 0..480 {
            history[i] = self.white_past[i] as f64;
        }
        for i in 0..320 {
            history[480 + i] = native[i] as f64;
        }
        let mut periodic64 = native.map(|x| x as f64);
        for i in 0..320 {
            let period = (controls[74 + i / 80] as f64).clamp(20.0, 160.0);
            for repeat in 1..=3 {
                let position = i as f64 - repeat as f64 * period;
                let delayed = if position < -(self.valid as f64) {
                    native[i] as f64
                } else {
                    let left = position.floor();
                    let fraction = position - left;
                    let index = (480.0 + left) as usize;
                    (1.0 - fraction) * history[index] + fraction * history[index + 1]
                };
                periodic64[i] += 0.7_f64.powi(repeat) * delayed;
            }
        }
        for b in 0..4 {
            let x = &mut periodic64[b * 80..(b + 1) * 80];
            let rms = (x.iter().map(|v| v * v).sum::<f64>() / 80.0)
                .sqrt()
                .max(1e-4);
            for v in x {
                *v /= rms;
            }
        }
        self.white_past
            .copy_from_slice(&std::array::from_fn::<_, 480, _>(|i| {
                history[320 + i] as f32
            }));
        self.valid = (self.valid + 320).min(480);
        Components {
            white64,
            native,
            periodic: periodic64.map(|x| x as f32),
            noise: std::array::from_fn(|_| self.noise.uniform()),
        }
    }
    pub fn mix(&mut self, c: &Controls, raw: &[[f32; 40]; 2], parts: &Components) -> [f32; 320] {
        let mut out = [0.0; 320];
        for b in 0..4 {
            let mapped = 1.0 - (1.0 - (c[70 + b] as f64).clamp(0.0, 1.0)).powi(4);
            let mut prior = [0.9_f64, (1.0 - 0.9) * mapped, (1.0 - 0.9) * (1.0 - mapped)];
            for p in &mut prior {
                *p = p.max(1e-5);
            }
            let sum: f64 = prior.iter().sum();
            let mut logits: [f64; 3] = std::array::from_fn(|j| {
                raw[b / 2][34 + 3 * (b % 2) + j] as f64 + ((prior[j] / sum).ln() as f32) as f64
            });
            let maximum = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            for v in &mut logits {
                *v = (*v - maximum).exp();
            }
            let sum: f64 = logits.iter().sum();
            let mass: [f64; 3] = std::array::from_fn(|j| {
                let v = logits[j] / sum;
                if let Some(old) = self.mass {
                    0.5 * old[j] + 0.5 * v
                } else {
                    v
                }
            });
            self.mass = Some(mass);
            let weights = mass.map(|v| v.max(1e-12).sqrt());
            let mixed: [f64; 80] = std::array::from_fn(|i| {
                let n = b * 80 + i;
                (parts.native[n] as f64 * weights[0] + parts.periodic[n] as f64 * weights[1])
                    + parts.noise[n] as f64 * weights[2]
            });
            let rms = (mixed.iter().map(|v| v * v).sum::<f64>() / 80.0)
                .sqrt()
                .max(1e-4);
            for i in 0..80 {
                out[b * 80 + i] = (mixed[i] / rms) as f32;
            }
        }
        out
    }
}
pub fn geometry(parts: &Components) -> [[f32; 12]; 2] {
    let signals = [&parts.native, &parts.periodic, &parts.noise];
    let mut out = [[0.0; 12]; 2];
    for b in 0..4 {
        let power: [f64; 3] = std::array::from_fn(|j| {
            (0..80)
                .map(|i| (signals[j][b * 80 + i] as f64).powi(2))
                .sum::<f64>()
                / 80.0
        });
        for (k, (a, c)) in [(0, 1), (0, 2), (1, 2)].into_iter().enumerate() {
            let dot = (0..80)
                .map(|i| signals[a][b * 80 + i] as f64 * signals[c][b * 80 + i] as f64)
                .sum::<f64>()
                / 80.0;
            out[b / 2][6 * (b % 2) + k] = (dot / (power[a] * power[c]).sqrt().max(1e-12)) as f32;
        }
        for j in 0..3 {
            out[b / 2][6 * (b % 2) + 3 + j] = power[j].max(1e-8).ln() as f32;
        }
    }
    out
}

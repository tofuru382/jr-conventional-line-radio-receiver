//! Shared bounded level, cepstral and gain controls; no signal-type routing.
use crate::excitation::{describe, fft};
use crate::predictor::Controls;

fn bound(cep: &mut [f32; 32], response_db: f64) {
    let mut symmetric = [[0.0; 2]; 512];
    for i in 0..32 {
        symmetric[i + 1][0] = cep[i] as f64;
        symmetric[511 - i][0] = cep[i] as f64;
    }
    fft(&mut symmetric);
    let peak = symmetric[..257]
        .iter()
        .map(|v| v[0].abs())
        .fold(0.0, f64::max);
    let scale = (response_db * std::f64::consts::LN_10 / 10.0 / peak.max(1e-20)).min(1.0);
    for x in cep {
        *x = (*x as f64 * scale) as f32;
    }
}
pub fn correct(controls: &Controls, raw: &[[f32; 34]; 2], strength: f32) -> Controls {
    let mut out = *controls;
    let maximum = controls[..4]
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let headroom = ((-3.0 - maximum) / 12.0).clamp(0.0, 1.0);
    for h in 0..2 {
        let quiet = [
            ((controls[2 * h] + 75.0) / 15.0).clamp(0.0, 1.0),
            ((controls[2 * h + 1] + 75.0) / 15.0).clamp(0.0, 1.0),
        ];
        for b in 0..2 {
            let delta = (raw[h][b] * strength).clamp(-12.0, 12.0) * quiet[b];
            out[2 * h + b] += delta.min(0.0) + delta.max(0.0) * headroom;
        }
        let mean = (quiet[0] + quiet[1]) / 2.0;
        let mut cep = std::array::from_fn(|i| (raw[h][i + 2] * strength).clamp(-0.75, 0.75) * mean);
        bound(&mut cep, 12.0);
        for i in 0..32 {
            out[6 + 32 * h + i] += cep[i];
        }
    }
    out
}
pub fn temporal(
    controls: &Controls,
    delta: &[f32; 4],
    raw: &[[f32; 34]; 2],
) -> (Controls, [f64; 4]) {
    let mut out = *controls;
    let mut total = [0.0; 4];
    for h in 0..2 {
        let mut cep = std::array::from_fn(|i| 1.5 * raw[h][i + 2].tanh() * 0.5);
        bound(&mut cep, 24.0);
        for i in 0..32 {
            out[6 + 32 * h + i] += cep[i];
        }
        for b in 0..2 {
            let k = h * 2 + b;
            let d = (delta[k] + 12.0 * raw[h][b].tanh() * 0.5) as f64;
            let old = (controls[k] as f64).clamp(-120.0, -3.0);
            total[k] = (old + d.clamp(-12.0, 12.0)).clamp(-120.0, -3.0) - old;
        }
    }
    (out, total)
}
pub fn gain_curve(current: &[f64; 4], previous: Option<f64>, following: Option<f64>) -> [f32; 320] {
    std::array::from_fn(|i| {
        let b = i / 80;
        let sample = i % 80;
        let (a, z, position) = if sample < 40 {
            (
                if b == 0 {
                    previous.unwrap_or(current[0])
                } else {
                    current[b - 1]
                },
                current[b],
                (sample as f64 + 40.5) / 80.0,
            )
        } else {
            (
                current[b],
                if b == 3 {
                    following.unwrap_or(current[3])
                } else {
                    current[b + 1]
                },
                (sample as f64 - 39.5) / 80.0,
            )
        };
        let u = 0.5 - 0.5 * (std::f64::consts::PI * position).cos();
        10.0_f64.powf((a + (z - a) * u) / 20.0) as f32
    })
}
pub fn gain_features(trace: &[[f32; 12]; 4]) -> [[f32; 24]; 2] {
    let mut out = [[0.0; 24]; 2];
    for b in 0..4 {
        let at = (b % 2) * 12;
        let x = trace[b];
        out[b / 2][at..at + 6].copy_from_slice(&x[1..7]);
        out[b / 2][at + 6] = (10.0 * (x[7] as f64).max(1.0).log10()) as f32;
        out[b / 2][at + 7] = (20.0 * (x[8] as f64).max(0.5).log10()) as f32;
        out[b / 2][at + 8] = x[9] / 32768.0;
        out[b / 2][at + 9] = x[10];
        out[b / 2][at + 10] = x[11];
        out[b / 2][at + 11] = if x[8] == 0.0 { 1.0 } else { 0.0 };
    }
    out
}
pub fn source_features(
    base: &[[f32; 570]; 2],
    controls: &Controls,
    pcm: &[i16; 320],
    raw: &[i16; 320],
    source: &[f64; 320],
) -> [[f32; 621]; 2] {
    let bands = describe(&pcm.map(|x| x as f64 / 32768.0));
    let source_bands = describe(source);
    let mut out = [[0.0; 621]; 2];
    for h in 0..2 {
        out[h][..570].copy_from_slice(&base[h]);
        for i in 0..20 {
            out[h][570 + i] = bands[h][i] as f32;
            out[h][592 + i] = source_bands[h][i] as f32;
        }
        for b in 0..2 {
            out[h][590 + b] = (bands[h][20 + b] - controls[2 * h + b] as f64) as f32;
            let at = h * 160 + b * 80;
            let energy = raw[at..at + 80]
                .iter()
                .map(|&v| (v as f64).powi(2))
                .sum::<f64>()
                / 80.0;
            out[h][619 + b] = (10.0 * energy.max(1.0).log10()) as f32;
        }
        let x = &source[h * 160..(h + 1) * 160];
        for (k, lag) in [1, 2, 4, 8, 16, 32, 64].into_iter().enumerate() {
            let (mut dot, mut a, mut b) = (0.0, 0.0, 0.0);
            for i in 0..160 - lag {
                dot += x[i] * x[i + lag];
                a += x[i] * x[i];
                b += x[i + lag] * x[i + lag];
            }
            out[h][612 + k] = (dot / (a * b).max(1e-20).sqrt()) as f32;
        }
    }
    out
}

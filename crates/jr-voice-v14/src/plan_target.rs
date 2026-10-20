//! Quantized fixed acoustic target inference with isotonic pitch distributions.
use crate::models::planner_data::learned_parameters as m;
use crate::predictor::Controls;
#[derive(Clone)]
pub struct Target {
    pub mean: [f32; 50],
    pub risk: [[f32; 141]; 3],
    pub missing: [f32; 3],
}
#[derive(Default)]
pub struct ModelState {
    previous: [f32; 4],
    older: [f32; 4],
}
struct Matrix {
    inputs: usize,
    weights: &'static [i8],
    scales: &'static [f32],
    bias: &'static [f32],
}
impl Matrix {
    fn affine(&self, input: &[f32], out: &mut [f32]) {
        for j in 0..out.len() {
            let mut sum = 0.0;
            for i in 0..self.inputs {
                sum += input[i] * self.weights[j * self.inputs + i] as f32;
            }
            out[j] = sum * self.scales[j] + self.bias[j];
        }
    }
}
macro_rules! matrix {
    ($inputs:expr,$weight:ident,$scale:ident,$bias:ident) => {
        Matrix {
            inputs: $inputs,
            weights: &m::$weight,
            scales: &m::$scale,
            bias: &m::$bias,
        }
    };
}
fn isotonic(values: &mut [f32; 140]) {
    let mut pooled = [0.0; 140];
    let mut counts = [0usize; 140];
    let mut blocks = 0;
    for &value in values.iter() {
        pooled[blocks] = value;
        counts[blocks] = 1;
        blocks += 1;
        while blocks >= 2 && pooled[blocks - 2] > pooled[blocks - 1] {
            let (a, b) = (blocks - 2, blocks - 1);
            let total = counts[a] + counts[b];
            pooled[a] =
                (pooled[a] * counts[a] as f32 + pooled[b] * counts[b] as f32) / total as f32;
            counts[a] = total;
            blocks -= 1;
        }
    }
    let mut at = 0;
    for b in 0..blocks {
        values[at..at + counts[b]].fill(pooled[b].clamp(0.0, 1.0));
        at += counts[b];
    }
}
impl ModelState {
    pub fn process(
        &mut self,
        features: &[f32; 220],
        controls: &Controls,
        reset: bool,
    ) -> Result<Target, ()> {
        let mut normalized = [0.0; 220];
        for i in 0..220 {
            if !features[i].is_finite() || m::V__SCALE[i] <= 0.0 {
                return Err(());
            }
            normalized[i] = ((features[i] - m::V__MEAN[i]) / m::V__SCALE[i]).clamp(-6.0, 6.0);
        }
        for i in 0..4 {
            if !controls[74 + i].is_finite() {
                return Err(());
            }
            if reset {
                self.previous[i] = controls[74 + i];
                self.older[i] = controls[74 + i];
            }
        }
        let windows = [
            (
                matrix!(220, Q__CURRENT_WEIGHT, S__CURRENT_WEIGHT, V__CURRENT_BIAS),
                matrix!(
                    220,
                    Q__CURRENT_CDF_WEIGHT,
                    S__CURRENT_CDF_WEIGHT,
                    V__CURRENT_CDF_BIAS
                ),
                matrix!(
                    16,
                    Q__CURRENT_CDF_BASIS,
                    S__CURRENT_CDF_BASIS,
                    V__CURRENT_CDF_MEAN
                ),
                m::V__CURRENT_VOICE_RATE[0],
            ),
            (
                matrix!(
                    220,
                    Q__PREVIOUS_WEIGHT,
                    S__PREVIOUS_WEIGHT,
                    V__PREVIOUS_BIAS
                ),
                matrix!(
                    220,
                    Q__PREVIOUS_CDF_WEIGHT,
                    S__PREVIOUS_CDF_WEIGHT,
                    V__PREVIOUS_CDF_BIAS
                ),
                matrix!(
                    16,
                    Q__PREVIOUS_CDF_BASIS,
                    S__PREVIOUS_CDF_BASIS,
                    V__PREVIOUS_CDF_MEAN
                ),
                m::V__PREVIOUS_VOICE_RATE[0],
            ),
            (
                matrix!(
                    220,
                    Q__TRAILING_WEIGHT,
                    S__TRAILING_WEIGHT,
                    V__TRAILING_BIAS
                ),
                matrix!(
                    220,
                    Q__TRAILING_CDF_WEIGHT,
                    S__TRAILING_CDF_WEIGHT,
                    V__TRAILING_CDF_BIAS
                ),
                matrix!(
                    16,
                    Q__TRAILING_CDF_BASIS,
                    S__TRAILING_CDF_BASIS,
                    V__TRAILING_CDF_MEAN
                ),
                m::V__TRAILING_VOICE_RATE[0],
            ),
        ];
        let mut out = Target {
            mean: [0.0; 50],
            risk: [[0.0; 141]; 3],
            missing: [0.0; 3],
        };
        const SIGMA: f32 = 0.057_762_265;
        const PITCH_SCALE: f32 = 7.213_475;
        for (kind, (mean_model, scores_model, cdf_model, voice_rate)) in windows.iter().enumerate()
        {
            let (offset, count) = match kind {
                0 => (0, 46),
                1 => (46, 2),
                _ => (48, 2),
            };
            let mean = &mut out.mean[offset..offset + count];
            mean_model.affine(&normalized, mean);
            if kind == 0 {
                for v in &mut mean[..4] {
                    *v = v.clamp(-120.0, 0.0);
                }
                for v in &mut mean[4..44] {
                    *v = v.clamp(-120.0, 20.0);
                }
            }
            for v in &mut mean[count - 2..] {
                *v = v.clamp(0.0, 1.0);
            }
            let mut scores = [0.0; 16];
            let mut cdf = [0.0; 140];
            scores_model.affine(&normalized, &mut scores);
            cdf_model.affine(&scores, &mut cdf);
            let mut periods = [0.0; 8];
            let size = if kind == 0 { 4 } else { 8 };
            if kind == 0 {
                periods[..4].copy_from_slice(&controls[74..78]);
            } else if kind == 1 {
                periods[..2].copy_from_slice(&self.older[2..]);
                periods[2..6].copy_from_slice(&self.previous);
                periods[6..].copy_from_slice(&controls[74..76]);
            } else {
                periods[..4].copy_from_slice(&self.previous);
                periods[4..].copy_from_slice(&controls[74..78]);
            }
            for v in &mut periods[..size] {
                *v = v.clamp(20.0, 160.0).ln();
            }
            for i in 0..140 {
                let mut prior = 0.0;
                for &p in &periods[..size] {
                    let z = ((m::EDGES[i] - p) / SIGMA).clamp(-50.0, 50.0);
                    prior += 1.0 / (1.0 + (-z).exp());
                }
                cdf[i] += prior / size as f32;
            }
            isotonic(&mut cdf);
            let mut previous = 0.0;
            let mut cumulative = 0.0;
            let mut sums = [0.0; 141];
            for i in 0..141 {
                let value = if i == 140 { 1.0 } else { cdf[i] };
                cumulative += (value - previous) * m::LOG_LAGS[i];
                previous = value;
                sums[i] = cumulative;
            }
            let voice = mean[count - 1] / voice_rate;
            out.missing[kind] = 5.0 * voice;
            for i in 0..141 {
                let c = if i == 140 { 1.0 } else { cdf[i] };
                let risk = m::LOG_LAGS[i] * (2.0 * c - 1.0) + cumulative - 2.0 * sums[i];
                out.risk[kind][i] = risk.max(0.0) * PITCH_SCALE * voice;
            }
        }
        let mut hidden = [0.0; 16];
        let mut hidden2 = [0.0; 8];
        let mut residual = [0.0];
        matrix!(220, Q__NET_W1, S__NET_W1, V__NET_B1).affine(&normalized, &mut hidden);
        for v in &mut hidden {
            *v = v.tanh();
        }
        matrix!(16, Q__NET_W2, S__NET_W2, V__NET_B2).affine(&hidden, &mut hidden2);
        for v in &mut hidden2 {
            *v = v.tanh();
        }
        matrix!(8, Q__NET_W3, S__NET_W3, V__NET_B3).affine(&hidden2, &mut residual);
        let parent = out.mean[44].clamp(1e-4, 1.0 - 1e-4);
        let logit = ((parent / (1.0 - parent)).ln() + residual[0]).clamp(-50.0, 50.0);
        out.mean[44] = 1.0 / (1.0 + (-logit).exp());
        self.older = self.previous;
        self.previous.copy_from_slice(&controls[74..78]);
        Ok(out)
    }
}

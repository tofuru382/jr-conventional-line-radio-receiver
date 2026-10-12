use super::fixed_core::{lsp, tables};
use super::{bits::HalfParams, config};

#[derive(Clone, Debug)]
pub struct Spectral {
    pub history: [[f32; 10]; 3],
}
impl Default for Spectral {
    fn default() -> Self {
        let mean = std::array::from_fn(|i| tables::LSF_MEAN[i] as f32);
        Self { history: [mean; 3] }
    }
}
impl Spectral {
    pub fn residual(p: &HalfParams) -> [f32; 10] {
        std::array::from_fn(|k| {
            tables::LSF_CB1[p.stage1 as usize * 10 + k] as f32
                + if k < 5 {
                    config::low(p.stage2_low as usize, k)
                } else {
                    config::high(p.stage2_high as usize, k - 5)
                }
        })
    }
    pub fn predict(history: &[[f32; 10]; 3], residual: &[f32; 10], mode: u8) -> [f32; 10] {
        // Match the SSE multiply/add order: current, history 0, 1, 2.
        let mut out = std::array::from_fn(|k| {
            let m = mode as usize;
            ((residual[k] * config::ma(m, 0, k) + history[0][k] * config::ma(m, 1, k))
                + history[1][k] * config::ma(m, 2, k))
                + history[2][k] * config::ma(m, 3, k)
        });
        out.sort_by(f32::total_cmp);
        out[0] = out[0].max(41.0);
        for k in 1..10 {
            out[k] = out[k].max(out[k - 1] + 321.0);
        }
        out[9] = out[9].min(25682.0);
        out
    }
    pub fn decode(&mut self, p: &HalfParams) -> [i16; 10] {
        let residual = Self::residual(p);
        let lsf = Self::predict(&self.history, &residual, p.mode);
        self.push(residual);
        to_lsp(&lsf)
    }
    pub fn push(&mut self, residual: [f32; 10]) {
        self.history[2] = self.history[1];
        self.history[1] = self.history[0];
        self.history[0] = residual;
    }
}

pub fn to_lsp(lsf: &[f32; 10]) -> [i16; 10] {
    // 0x140d71340 rounds ties to even. Then monotone clamp (not LSF_GAP).
    let mut prior = 0i16;
    let quantized = std::array::from_fn(|i| {
        prior = (lsf[i].round_ties_even() as i16).max(prior).clamp(0, 25682);
        prior
    });
    lsp::lsf_to_lsp(&quantized)
}

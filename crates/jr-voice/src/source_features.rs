//! Causal half-frame bit, parameter and physical-state features.
use crate::plan_features::Features;
use crate::predictor::Controls;

pub struct SourceFeatures {
    physical: Features,
    previous: [[f32; 190]; 2],
    age: usize,
}
impl Default for SourceFeatures {
    fn default() -> Self {
        Self {
            physical: Features::default(),
            previous: [[0.0; 190]; 2],
            age: 0,
        }
    }
}
impl SourceFeatures {
    pub fn process(&mut self, payload: &[u8; 20], c: &Controls) -> Result<[[f32; 570]; 2], ()> {
        let physical = self.physical.process(payload, c, self.age == 0)?;
        let bits: [u8; 158] = std::array::from_fn(|i| (payload[i / 8] >> (i % 8)) & 1);
        let mut out = [[0.0; 570]; 2];
        for h in 0..2 {
            let mut x = [0.0; 190];
            x[..32].copy_from_slice(&c[6 + 32 * h..38 + 32 * h]);
            x[32..34].copy_from_slice(&c[2 * h..2 * h + 2]);
            x[34..36].copy_from_slice(&c[70 + 2 * h..72 + 2 * h]);
            x[36] = c[74 + 2 * h].clamp(20.0, 160.0).ln();
            x[37] = c[75 + 2 * h].clamp(20.0, 160.0).ln();
            x[38] = c[4];
            x[39] = c[5].clamp(20.0, 160.0).ln();
            x[40] = h as f32;
            x[41] = self.age.min(20) as f32 / 20.0;
            x[42] = if self.age == 0 { 1.0 } else { 0.0 };
            let mut group = [0; 79];
            group[..39].copy_from_slice(&bits[h * 39..h * 39 + 39]);
            group[39..59].copy_from_slice(&bits[78 + 20 * h..98 + 20 * h]);
            group[59..].copy_from_slice(&bits[118 + 20 * h..138 + 20 * h]);
            for j in 0..79 {
                x[43 + j] = 1.0 - 2.0 * group[j] as f32;
            }
            x[122..].copy_from_slice(&physical[84 + 68 * h..152 + 68 * h]);
            if self.age == 0 {
                self.previous.fill(x);
            }
            out[h][..190].copy_from_slice(&self.previous[0]);
            out[h][190..380].copy_from_slice(&self.previous[1]);
            out[h][380..].copy_from_slice(&x);
            self.previous[0] = self.previous[1];
            self.previous[1] = x;
            self.age = self.age.saturating_add(1);
        }
        Ok(out)
    }
}

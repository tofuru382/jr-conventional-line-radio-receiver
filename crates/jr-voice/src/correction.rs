//! Learned native-bit-conditioned gain and spectral corrections.
use crate::models::correction as d;
use crate::numeric::Numeric;
use crate::predictor::Controls;

fn unit(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}
fn group(bits: &[u8; 158], half: usize) -> [u8; 79] {
    let mut out = [0; 79];
    out[..39].copy_from_slice(&bits[half * 39..half * 39 + 39]);
    out[39..59].copy_from_slice(&bits[78 + half * 20..98 + half * 20]);
    out[59..].copy_from_slice(&bits[118 + half * 20..138 + half * 20]);
    out
}
fn read(group: &[u8; 79], positions: &[usize]) -> usize {
    positions.iter().fold(0, |r, &p| 2 * r + group[p] as usize)
}
fn features(values: &Controls, half: usize, x: &mut [f32]) {
    x[..32].copy_from_slice(&values[6 + half * 32..38 + half * 32]);
    x[32] = values[4];
    x[33] = values[5].clamp(20.0, 160.0).ln();
    x[34] = values[half * 2];
    x[35] = values[half * 2 + 1];
}
#[derive(Default)]
pub struct Correction {
    gain_history: [usize; 4],
    spectral_history: [[usize; 3]; 4],
    main_history: [usize; 3],
    previous_level: f32,
    has_gain: bool,
    has_spectrum: bool,
}
impl Correction {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn process(
        &mut self,
        bits: &[u8; 158],
        input: &Controls,
        numeric: &mut Numeric,
    ) -> Result<Controls, ()> {
        if bits.iter().any(|&x| x > 1) || !input.iter().all(|x| x.is_finite()) {
            return Err(());
        }
        let mut values = *input;
        let frame_level = (input[0] + input[1] + input[2] + input[3]) / 4.0;
        let positive_gate =
            unit((-10.0 - input[..4].iter().copied().fold(f32::NEG_INFINITY, f32::max)) / 8.0);
        const GAIN_WEIGHTS: [f32; 4] = [
            1.0 / 2.734375,
            0.75 / 2.734375,
            0.5625 / 2.734375,
            0.421875 / 2.734375,
        ];
        for slot in 0..4 {
            let group = group(bits, slot / 2);
            let mut code = 0;
            for j in 0..7 {
                code = 2 * code + group[20 + 12 * (slot % 2) + j] as usize;
            }
            if !self.has_gain {
                self.gain_history.fill(code);
            } else {
                self.gain_history.copy_within(0..3, 1);
                self.gain_history[0] = code;
            }
            let level = input[slot];
            let mut x = [
                level,
                (if self.has_gain {
                    self.previous_level
                } else {
                    level
                }) - level,
                (if slot == 3 { level } else { input[slot + 1] }) - level,
                input[70 + slot],
                input[74 + slot].clamp(20.0, 160.0).ln(),
                (slot % 2) as f32,
                input[70 + slot] * level,
                frame_level,
                0.0,
                0.0,
                0.0,
                0.0,
            ];
            x[8..].copy_from_slice(&input[6 + (slot / 2) * 32..10 + (slot / 2) * 32]);
            let mut delta = 0.0;
            for t in 0..4 {
                delta += GAIN_WEIGHTS[t] * d::GAIN_WEIGHT[self.gain_history[t]];
            }
            for j in 0..12 {
                delta += ((x[j] - d::GAIN_MEAN[j]) / d::GAIN_SCALE[j]).clamp(-6.0, 6.0)
                    * d::GAIN_WEIGHT[128 + j];
            }
            delta += d::GAIN_WEIGHT[140];
            delta = unit((level + 60.0) / 10.0) * delta.clamp(-6.0, 6.0);
            values[slot] += delta.min(0.0) + delta.max(0.0) * positive_gate;
            self.previous_level = level;
            self.has_gain = true;
        }
        const SPECTRAL_WEIGHTS: [f32; 4] = [1.0 / 1.875, 0.5 / 1.875, 0.25 / 1.875, 0.125 / 1.875];
        for half in 0..2 {
            let group = group(bits, half);
            let main = read(&group, &[1, 2, 3, 4, 5, 6, 7]);
            let categories = [
                group[0] as usize * 256 + main,
                group[0] as usize * 256 + 128 + read(&group, &[8, 9, 10, 11, 40, 42]),
                group[0] as usize * 256 + 192 + read(&group, &[39, 41, 59, 60, 61, 62]),
            ];
            if !self.has_spectrum {
                self.spectral_history.fill(categories);
                self.main_history.fill(main);
            } else {
                self.spectral_history.copy_within(0..3, 1);
                self.spectral_history[0] = categories;
                self.main_history.copy_within(0..2, 1);
                self.main_history[0] = main;
            }
            let gate = unit(((values[half * 2] + values[half * 2 + 1]) / 2.0 + 60.0) / 10.0);
            let mut x = [0.0; 145];
            let mut delta = [0.0; 32];
            features(&values, half, &mut x);
            for j in 0..16 {
                let mut value = 0.0;
                for t in 0..4 {
                    for field in 0..3 {
                        value += SPECTRAL_WEIGHTS[t]
                            * d::LINEAR_COEFFICIENT.get(self.spectral_history[t][field] * 16 + j);
                    }
                }
                for i in 0..36 {
                    value += ((x[i] - d::LINEAR_MEAN[i]) / d::LINEAR_SCALE[i]).clamp(-6.0, 6.0)
                        * d::LINEAR_COEFFICIENT.get((512 + i) * 16 + j);
                }
                value += d::LINEAR_COEFFICIENT.get(548 * 16 + j);
                delta[j] = 0.5 * value * gate;
            }
            numeric.bound_correction(&mut delta);
            for j in 0..32 {
                values[6 + half * 32 + j] += delta[j];
            }
            features(&values, half, &mut x);
            for j in 0..79 {
                x[36 + j] = 1.0 - 2.0 * group[j] as f32;
            }
            for t in 0..3 {
                for j in 0..10 {
                    x[115 + t * 10 + j] =
                        d::MAIN_TABLE[self.main_history[t] * 10 + j] as f32 / 8192.0;
                }
            }
            for j in 0..145 {
                x[j] = (x[j] - d::STUDENT_X_MEAN[j]) / d::STUDENT_X_SCALE[j];
            }
            let mut first = [0.0; 24];
            let mut second = [0.0; 12];
            let mut prediction = [0.0; 16];
            d::STUDENT_LAYER_0_WEIGHT.dense(&x, &d::STUDENT_LAYER_0_BIAS, &mut first);
            for x in &mut first {
                *x = x.tanh();
            }
            d::STUDENT_LAYER_1_WEIGHT.dense(&first, &d::STUDENT_LAYER_1_BIAS, &mut second);
            for x in &mut second {
                *x = x.tanh();
            }
            d::STUDENT_LAYER_2_WEIGHT.dense(&second, &d::STUDENT_LAYER_2_BIAS, &mut prediction);
            delta.fill(0.0);
            for j in 0..16 {
                delta[j] = (prediction[j] * d::STUDENT_Y_SCALE[j] + d::STUDENT_Y_MEAN[j]) * gate;
            }
            numeric.bound_correction(&mut delta);
            for j in 0..32 {
                values[6 + half * 32 + j] += delta[j];
            }
            self.has_spectrum = true;
        }
        if values.iter().all(|x| x.is_finite()) {
            Ok(values)
        } else {
            Err(())
        }
    }
}

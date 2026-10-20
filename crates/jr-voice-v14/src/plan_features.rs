//! Feature hypotheses used by the fixed planner, not normative bit assignments.
use crate::models::planner_data::input_parameters as t;
use crate::predictor::Controls;
#[derive(Clone, Copy, Default)]
struct Half {
    mode: usize,
    stage1: usize,
    low: usize,
    high: usize,
    gain: [usize; 2],
    word: [usize; 2],
}
fn read(w: &[u8], begin: usize, count: usize) -> usize {
    w[begin..begin + count]
        .iter()
        .fold(0, |a, &b| 2 * a + b as usize)
}
fn pick(w: &[u8], positions: &[usize]) -> usize {
    positions.iter().fold(0, |a, &i| 2 * a + w[i] as usize)
}
fn half(w: &[u8], h: usize) -> Half {
    let (q, a, b) = (84 + 39 * h, 20 * h, 40 + 20 * h);
    let low = read(w, q + 23, 4);
    let mut p = Half {
        mode: w[q] as usize,
        stage1: read(w, q + 1, 7),
        low: pick(w, &[q + 8, q + 9, q + 10, q + 11, a + 3, a + 1]),
        high: pick(w, &[b + 3, b + 1, a + 2, a, b + 2, b]),
        gain: [
            16 * read(w, q + 20, 3) + low,
            16 * read(w, q + 32, 3) + if h == 1 { low } else { read(w, q + 35, 4) },
        ],
        word: [0; 2],
    };
    for s in 0..2 {
        let k = 8 * s;
        let j = if s == 1 { 30 } else { 18 };
        p.word[s] = pick(
            w,
            &[
                q + j,
                q + j + 1,
                b + 7 + k,
                b + 5 + k,
                a + 6 + k,
                a + 4 + k,
                b + 6 + k,
                b + 4 + k,
                a + 11 + k,
                a + 9 + k,
                b + 11 + k,
                b + 9 + k,
                a + 10 + k,
                a + 8 + k,
                b + 10 + k,
                b + 8 + k,
            ],
        );
    }
    p
}
fn innovation(index: usize) -> ([i16; 80], usize) {
    let index = index.min(t::INDEX_LIMIT[0] as usize - 1);
    let mut cls = 5;
    while cls > 0 && index < (t::CLASS_BASE[cls] as usize) {
        cls -= 1;
    }
    let mut raw = [0i16; 80];
    let mut rest = index - t::CLASS_BASE[cls] as usize;
    if cls == 5 {
        for pair in [
            &t::ALT_A[5 * (rest / 128)..][..5],
            &t::ALT_B[5 * (rest % 128)..][..5],
        ] {
            for &entry in pair {
                let pos = entry.unsigned_abs() as usize - 1;
                raw[pos] = (raw[pos] as i32 + if entry > 0 { 8192 } else { -8192 })
                    .clamp(-32768, 32767) as i16;
            }
        }
    } else {
        let count = t::PULSES[cls] as usize;
        let mut signs = [0; 3];
        let mut position = [0; 3];
        for sign in &mut signs[..count] {
            *sign = rest & 1;
            rest /= 2;
        }
        let mut value = rest as i64 * 65536;
        for k in (0..count).rev() {
            let quotient = ((value / 65536) * t::RADIX_INVERSE[3 * cls + k] as i64 * 2)
                .clamp(i32::MIN as i64, i32::MAX as i64)
                / 262144;
            value = (value - quotient * t::RADIX[3 * cls + k] as i64 * 65536)
                .clamp(i32::MIN as i64, i32::MAX as i64);
            position[k] = (value / 65536) as usize;
            value = quotient * 65536;
        }
        let first = t::SHAPE_SELECT[3 * cls] as usize;
        for k in 0..count {
            let pos = t::POSITIONS[40 * (3 * cls + k) + position[k]] as usize;
            for n in pos..80.min(pos + 20) {
                let v = t::SHAPES[20 * (first + k) + n - pos] as i32;
                raw[n] = (raw[n] as i32 + if signs[k] != 0 { v } else { -v }).clamp(-32768, 32767)
                    as i16;
            }
        }
    }
    (raw, cls)
}
fn spectral(p: Half, history: &mut [[f32; 10]; 3]) -> ([f32; 10], [f32; 10]) {
    let mut residual = [0.0; 10];
    let mut lsf = [0.0; 10];
    for k in 0..10 {
        residual[k] = t::SPECTRAL[p.stage1 * 10 + k]
            + t::SPECTRAL[if k < 5 {
                1280 + p.low * 5 + k
            } else {
                1600 + p.high * 5 + k - 5
            }];
        let w = &t::MA[p.mode * 40..];
        lsf[k] = ((residual[k] * w[k] + history[0][k] * w[10 + k]) + history[1][k] * w[20 + k])
            + history[2][k] * w[30 + k];
    }
    lsf.sort_by(f32::total_cmp);
    lsf[0] = lsf[0].max(41.0);
    for k in 1..10 {
        lsf[k] = lsf[k].max(lsf[k - 1] + 321.0);
    }
    lsf[9] = lsf[9].min(25682.0);
    history[2] = history[1];
    history[1] = history[0];
    history[0] = residual;
    (residual, lsf)
}
#[derive(Default)]
pub struct Features {
    spectral_history: [[[f32; 10]; 3]; 2],
    gain_history: [f32; 4],
    previous: [f32; 28],
    average: [f32; 28],
}
impl Features {
    pub fn process(
        &mut self,
        payload: &[u8; 20],
        controls: &Controls,
        reset: bool,
    ) -> Result<[f32; 220], ()> {
        if payload[19] & 0xc0 != 0 || !controls.iter().all(|v| v.is_finite()) {
            return Err(());
        }
        let bits: [u8; 158] = std::array::from_fn(|i| (payload[i / 8] >> (i % 8)) & 1);
        let mut wire = [0; 158];
        wire[..80].copy_from_slice(&bits[78..]);
        for i in [1, 5, 7] {
            wire[i] ^= 1;
        }
        wire[84..].copy_from_slice(&bits[..74]);
        let mut normal = [half(&wire, 0), half(&wire, 1)];
        normal[1].gain[1] = (normal[1].gain[1] / 16) * 16 + read(&bits, 74, 4);
        wire[1] ^= 1;
        let alternate = [half(&wire, 0), half(&wire, 1)];
        if reset {
            self.spectral_history = [[t::LSF_MEAN; 3]; 2];
            self.gain_history = [t::INITIAL_GAIN; 4];
        }
        let mut current = [0.0; 28];
        let mut out = [0.0; 220];
        for i in 0..4 {
            current[i] = controls[i].clamp(-120.0, -3.0);
            current[4 + i] = controls[70 + i].clamp(0.0, 1.0);
            current[8 + i] = controls[74 + i].clamp(20.0, 160.0).ln();
        }
        for i in 0..8 {
            current[12 + i] = controls[6 + i].clamp(-4.0, 4.0);
            current[20 + i] = controls[38 + i].clamp(-4.0, 4.0);
        }
        for i in 0..28 {
            out[i] = current[i];
            out[28 + i] = if reset {
                0.0
            } else {
                current[i] - self.previous[i]
            };
            self.average[i] = if reset {
                current[i]
            } else {
                0.75 * self.average[i] + 0.25 * current[i]
            };
            out[56 + i] = self.average[i];
            self.previous[i] = current[i];
        }
        for h in 0..2 {
            let p = normal[h];
            let (residual, lsf) = spectral(p, &mut self.spectral_history[0]);
            let (_, other) = spectral(alternate[h], &mut self.spectral_history[1]);
            let o = &mut out[84 + 68 * h..84 + 68 * (h + 1)];
            for k in 0..10 {
                o[k] = lsf[k] / 8192.0;
                o[10 + k] = other[k] / 8192.0;
                o[20 + k] = residual[k] / 8192.0;
            }
            for s in 0..2 {
                let correction = t::GAIN_LOG[p.gain[s]];
                o[30 + s] = correction;
                o[32 + 4 * s..36 + 4 * s].copy_from_slice(&self.gain_history);
                for tap in (1..4).rev() {
                    self.gain_history[tap] = self.gain_history[tap - 1];
                }
                self.gain_history[0] = correction;
                o[40 + s] = t::GAIN_ADAPTIVE[p.gain[s]] / 16384.0;
                let (raw, cls) = innovation(p.word[s]);
                let energy =
                    (raw.iter().map(|&v| v as i64 * v as i64).sum::<i64>() as f32 / 80.0).max(1.0);
                let start = 42 + 13 * s;
                o[start] = energy.ln();
                for (j, lag) in [1, 2, 4, 8].into_iter().enumerate() {
                    let mut dot = 0i64;
                    for i in 0..80 - lag {
                        dot += raw[i] as i64 * raw[i + lag] as i64;
                    }
                    o[start + 1 + j] = (dot as f32 / (80 - lag) as f32) / energy;
                }
                o[start + 5 + cls] = 1.0;
            }
        }
        Ok(out)
    }
}

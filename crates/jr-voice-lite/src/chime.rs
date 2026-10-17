//! Classify and synthesize the two production chime families.
use crate::models::chime_data as m;
use crate::numeric::{round_even, sample, PI};
use crate::predictor::{native_bit, Controls, FeatureRecord};
use crate::vocoder::{Pcm, Vocoder, VoiceNoise};

fn evidence(records: &mut [FeatureRecord], index: usize) {
    if records[index].chime_ready {
        return;
    }
    let mut logits = m::GATE_BIAS;
    for offset in -1..=1 {
        let features =
            &records[(index as i32 + offset).clamp(0, records.len() as i32 - 1) as usize].features;
        for j in 0..198 {
            let k = (offset + 1) as usize * 198 + j;
            let x = ((features[j] - m::GATE_MEAN[k]) / m::GATE_SCALE[k]).clamp(-6.0, 6.0);
            for f in 0..2 {
                logits[f] += x * m::GATE_WEIGHT[k * 2 + f];
            }
        }
    }
    let mut votes = logits.map(|x| x > 9.0_f32.ln());
    const POSITIONS: [usize; 28] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 39, 40, 41, 42, 44, 46, 59,
        60, 61, 62,
    ];
    let pattern = (0..2).all(|half| {
        let mut word = 0_u32;
        for (j, &local) in POSITIONS.iter().enumerate() {
            word |= u32::from(records[index].features[40 + native_bit(half, local)] < 0.0) << j;
        }
        m::RY_PATTERNS
            .iter()
            .any(|&target| (word ^ target).count_ones() <= 2)
    });
    votes[0] |= pattern;
    records[index].chime_logits = logits;
    records[index].chime_evidence = votes;
    records[index].chime_ready = index + 1 < records.len();
}
pub fn select_chime(records: &mut [FeatureRecord], index: usize) -> usize {
    let mut votes = [0; 2];
    for j in index.saturating_sub(2)..records.len().min(index + 3) {
        evidence(records, j);
        for f in 0..2 {
            votes[f] += usize::from(records[j].chime_evidence[f]);
        }
    }
    let mut best = -1e6_f32;
    let mut selected = 0;
    for f in 0..2 {
        let score = 100.0 * votes[f] as f32 + records[index].chime_logits[f].clamp(-40.0, 40.0);
        if votes[f] >= 3 && score > best {
            selected = f + 1;
            best = score;
        }
    }
    selected
}
fn features(controls: &Controls, periods: &[f32; 4]) -> [f32; 76] {
    let mut output = [0.0; 76];
    for h in 0..2 {
        let x = &mut output[h * 38..h * 38 + 38];
        x[..32].copy_from_slice(&controls[6 + h * 32..38 + h * 32]);
        x[32..34].copy_from_slice(&controls[h * 2..h * 2 + 2]);
        x[34..36].copy_from_slice(&controls[70 + h * 2..72 + h * 2]);
        x[36] = periods[h * 2].ln();
        x[37] = periods[h * 2 + 1].ln();
    }
    output
}
pub struct ChimeSynth {
    ry: Vocoder,
    noise: VoiceNoise,
    noise_history: [f32; 64],
    phase: [f32; 11],
    previous: [f32; 12],
    replacement: Pcm,
    family: usize,
    old_family: usize,
    noise_cursor: usize,
}
impl Default for ChimeSynth {
    fn default() -> Self {
        Self {
            ry: Vocoder::default(),
            noise: VoiceNoise::default(),
            noise_history: [0.0; 64],
            phase: [0.0; 11],
            previous: [0.0; 12],
            replacement: [0; 320],
            family: 0,
            old_family: 0,
            noise_cursor: 0,
        }
    }
}
impl ChimeSynth {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    fn dch(&mut self, input: &[f32; 76]) -> Result<Pcm, ()> {
        let mut pcm = [0; 320];
        for h in 0..2 {
            let normalized: [f32; 38] = std::array::from_fn(|j| {
                ((input[h * 38 + j] - m::D_MEAN[j]) / m::D_SCALE[j]).clamp(-6.0, 6.0)
            });
            let mut target = m::D_BIAS;
            for k in 0..24 {
                let mut distance = 0.0;
                for j in 0..38 {
                    let delta = normalized[j] - m::D_CENTERS[k * 38 + j];
                    distance += delta * delta;
                }
                let weight = (-distance / m::D_WIDTH[0]).exp();
                for j in 0..12 {
                    target[j] += weight * m::D_WEIGHT[k * 12 + j];
                }
            }
            for x in &mut target {
                *x = x.clamp(0.0, 0.4);
            }
            let mut fundamental = [0.0; 4];
            let mut peak = 0.0_f32;
            for j in 0..11 {
                if m::D_HARMONICS[j] == 1 {
                    fundamental[m::D_BANKS[j] as usize] = target[j];
                    peak = peak.max(target[j]);
                }
            }
            for j in 0..11 {
                if fundamental[m::D_BANKS[j] as usize] < 0.12 * peak {
                    target[j] = 0.0;
                }
            }
            if self.family != 2 && h == 0 {
                self.phase.fill(0.0);
                self.previous = target;
            }
            let mut cosine = [0.0; 11];
            let mut sine = [0.0; 11];
            let mut step_cos = [0.0; 11];
            let mut step_sin = [0.0; 11];
            for j in 0..11 {
                cosine[j] = self.phase[j].cos();
                sine[j] = self.phase[j].sin();
                let step = 2.0 * PI * m::D_FREQUENCY[j] / 8000.0;
                step_cos[j] = step.cos();
                step_sin[j] = step.sin();
                self.phase[j] = (self.phase[j] + 160.0 * step) % (2.0 * PI);
            }
            for i in 0..160 {
                let ramp = (i as f32 / 40.0).min(1.0);
                let mut value = 0.0;
                for j in 0..11 {
                    let amplitude = self.previous[j] + (target[j] - self.previous[j]) * ramp;
                    value += cosine[j] * amplitude;
                    let next = cosine[j] * step_cos[j] - sine[j] * step_sin[j];
                    sine[j] = sine[j] * step_cos[j] + cosine[j] * step_sin[j];
                    cosine[j] = next;
                }
                self.noise_history[self.noise_cursor] =
                    (self.noise.uniform() + self.noise.uniform() + self.noise.uniform())
                        / 3.0_f32.sqrt();
                let mut noise = 0.0;
                for j in 0..64 {
                    noise += self.noise_history[self.noise_cursor.wrapping_sub(j) & 63]
                        * m::D_IMPULSE[j];
                }
                self.noise_cursor = (self.noise_cursor + 1) & 63;
                value += noise * (self.previous[11] + (target[11] - self.previous[11]) * ramp);
                if !value.is_finite() {
                    return Err(());
                }
                pcm[h * 160 + i] = sample(value);
            }
            self.previous = target;
        }
        Ok(pcm)
    }
    pub fn render(
        &mut self,
        controls: &Controls,
        periods: &[f32; 4],
        family: usize,
    ) -> Result<(), ()> {
        self.old_family = self.family;
        if family == 0 {
            self.family = 0;
            return Ok(());
        }
        let x = features(controls, periods);
        self.replacement = if family == 1 {
            if self.family != 1 {
                self.ry.reset();
            }
            let mut modified = *controls;
            for h in 0..2 {
                let mut y = m::RY_BIAS;
                for k in 0..38 {
                    let normalized =
                        ((x[h * 38 + k] - m::RY_MEAN[k]) / m::RY_SCALE[k]).clamp(-6.0, 6.0);
                    for j in 0..34 {
                        y[j] += normalized * m::RY_WEIGHT[k * 34 + j];
                    }
                }
                for j in 0..2 {
                    modified[h * 2 + j] = y[j].clamp(-80.0, -3.0);
                    modified[70 + h * 2 + j] = 1.0;
                    modified[74 + h * 2 + j] = periods[h * 2 + j];
                }
                for j in 0..32 {
                    modified[6 + h * 32 + j] = y[j + 2].clamp(-4.0, 4.0);
                }
            }
            self.ry.process(&modified, true, false)?
        } else {
            self.dch(&x)?
        };
        self.family = family;
        Ok(())
    }
    pub fn blend(&self, end: bool, pcm: &mut Pcm) {
        if self.family == 0 {
            return;
        }
        for (i, x) in pcm.iter_mut().enumerate() {
            let mut gain = 1.0;
            if self.old_family != self.family && i < 40 {
                gain = 0.5 - 0.5 * (PI * i as f32 / 39.0).cos();
            }
            if end && i >= 280 {
                gain = 0.5 + 0.5 * (PI * (i - 280) as f32 / 39.0).cos();
            }
            *x = round_even(self.replacement[i] as f32 * gain + *x as f32 * (1.0 - gain)) as i16;
        }
    }
}

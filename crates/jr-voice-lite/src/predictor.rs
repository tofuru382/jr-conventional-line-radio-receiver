//! Bounded lookahead feature prediction and recurrent speech controls.
use crate::chime::select_chime;
use crate::models::{model_data as m, Mlp};
use crate::numeric::{affine, affine_columns, normalize, normalize_in_place, Numeric};
pub type Controls = [f32; 82];
pub type Payload = [u8; 20];
pub const LOOKAHEAD: usize = 3;

#[derive(Clone, Copy)]
pub struct FeatureRecord {
    pub features: [f32; 198],
    pub periods: [f32; 4],
    pub chime_logits: [f32; 2],
    pub chime_evidence: [bool; 2],
    pub chime_ready: bool,
}
impl Default for FeatureRecord {
    fn default() -> Self {
        Self {
            features: [0.0; 198],
            periods: [0.0; 4],
            chime_logits: [0.0; 2],
            chime_evidence: [false; 2],
            chime_ready: false,
        }
    }
}
pub struct Prediction {
    pub controls: Controls,
    pub bits: [u8; 158],
    pub periods: [f32; 4],
    pub chime: usize,
    pub chime_end: bool,
}
fn read(bits: &[u8], positions: &[usize]) -> usize {
    positions
        .iter()
        .fold(0, |result, &p| 2 * result + bits[p] as usize)
}
fn encode_features(payload: &Payload) -> FeatureRecord {
    let mut result = FeatureRecord::default();
    let bits: [u8; 158] = std::array::from_fn(|i| (payload[i / 8] >> (i % 8)) & 1);
    for half in 0..2 {
        let mut local = [0; 79];
        local[..39].copy_from_slice(&bits[half * 39..half * 39 + 39]);
        local[39..59].copy_from_slice(&bits[78 + half * 20..98 + half * 20]);
        local[59..].copy_from_slice(&bits[118 + half * 20..138 + half * 20]);
        let i = read(&local, &[12, 13, 14, 15, 16, 17, 46, 44]) as i32;
        let j = read(&local, &[27, 28, 29, 54, 52]) as i32;
        let anchor = if i < 197 { (i + 2) / 3 + 19 } else { i - 112 };
        let pitch = [
            if i < 197 {
                (i + 58) as f32 / 3.0
            } else {
                i as f32 - 112.0
            },
            (anchor - 5).clamp(20, 134) as f32 + (j - 2) as f32 / 3.0,
        ];
        let mut features = [0.0; 17];
        for field in 0..7 {
            let row = m::PROFILE_OFFSETS[field]
                + read(
                    &local,
                    &m::PROFILE_GROUPS[field][..m::PROFILE_LENGTHS[field]],
                );
            for k in 0..17 {
                features[k] += m::PROFILE_WEIGHT.get(row * 17 + k);
            }
        }
        for slot in 0..2 {
            result.periods[half * 2 + slot] = pitch[slot];
            let log_period = pitch[slot].clamp(20.0, 160.0).ln();
            result.features[34 + half * 2 + slot] = log_period;
            for k in 0..17 {
                features[k] += log_period * m::PROFILE_WEIGHT.get((648 + slot) * 17 + k);
            }
        }
        for k in 0..17 {
            features[k] += m::PROFILE_WEIGHT.get(650 * 17 + k);
        }
        result.features[half * 17..half * 17 + 17].copy_from_slice(&features);
        result.features[38 + half] = local[0] as f32;
    }
    for (i, &bit) in bits.iter().enumerate() {
        result.features[40 + i] = 1.0 - 2.0 * bit as f32;
    }
    result
}
pub struct Predictor {
    buffer: [FeatureRecord; 8],
    state: [f32; 64],
    previous_context: Option<[f32; 17]>,
    numeric: Numeric,
    count: usize,
    cursor: usize,
}
impl Default for Predictor {
    fn default() -> Self {
        Self {
            buffer: [FeatureRecord::default(); 8],
            state: [0.0; 64],
            previous_context: None,
            numeric: Numeric::default(),
            count: 0,
            cursor: 0,
        }
    }
}
impl Predictor {
    pub fn reset(&mut self) {
        self.count = 0;
        self.cursor = 0;
        self.state.fill(0.0);
        self.previous_context = None;
    }
    pub fn pending(&self) -> usize {
        self.count - self.cursor
    }
    fn at(&self, index: i32) -> &FeatureRecord {
        &self.buffer[index.clamp(0, self.count as i32 - 1) as usize]
    }
    fn baseline(&self, index: i32) -> [f32; 17] {
        let mut input = [0.0; 280];
        for offset in -3..=3 {
            let pos = (offset + 3) as usize * 40;
            input[pos..pos + 40].copy_from_slice(&self.at(index + offset).features[..40]);
        }
        let mut out = [0.0; 17];
        affine(&input, m::BASELINE_WEIGHT, m::BASELINE_BIAS.data, &mut out);
        out
    }
    fn mlp(model: &Mlp, input: &[f32]) -> ([f32; 17], [f32; 32]) {
        let mut normalized = [0.0; 1007];
        normalize(input, model.x_mean, model.x_scale, &mut normalized, false);
        let mut first = [0.0; 64];
        affine(&normalized, model.weight[0], model.bias[0].data, &mut first);
        for x in &mut first[..model.weight[0].cols] {
            *x = x.tanh();
        }
        let mut second = [0.0; 32];
        affine(&first, model.weight[1], model.bias[1].data, &mut second);
        for x in &mut second {
            *x = x.tanh();
        }
        let mut output = [0.0; 17];
        affine(&second, model.weight[2], model.bias[2].data, &mut output);
        for (i, x) in output.iter_mut().enumerate() {
            *x = *x * model.y_scale.get(i) + model.y_mean.get(i);
        }
        (output, second)
    }
    fn gru(&mut self, features: &[f32; 81]) -> [f32; 70] {
        let mut input = [0.0; 81];
        let mut projected = [0.0; 192];
        let mut gates = [0.0; 128];
        normalize(
            features,
            m::RECURRENT_X_MEAN,
            m::RECURRENT_X_SCALE,
            &mut input,
            false,
        );
        affine(
            &input,
            m::RECURRENT_INPUT_WEIGHT,
            m::RECURRENT_GATE_BIAS.data,
            &mut projected,
        );
        affine_columns(
            &self.state,
            m::RECURRENT_RECURRENT_WEIGHT,
            &[],
            &mut gates,
            0,
            128,
        );
        for j in 0..128 {
            gates[j] = 0.5 * (1.0 + (0.5 * (projected[j] + gates[j])).tanh());
        }
        let reset_state: [f32; 64] = std::array::from_fn(|i| gates[64 + i] * self.state[i]);
        let mut next = [0.0; 64];
        affine_columns(
            &reset_state,
            m::RECURRENT_RECURRENT_WEIGHT,
            &[],
            &mut next,
            128,
            64,
        );
        for j in 0..64 {
            next[j] =
                gates[j] * self.state[j] + (1.0 - gates[j]) * (projected[128 + j] + next[j]).tanh();
        }
        self.state = next;
        let mut output = [0.0; 70];
        affine(
            &self.state,
            m::RECURRENT_OUTPUT_WEIGHT,
            m::RECURRENT_OUTPUT_BIAS.data,
            &mut output,
        );
        for (i, x) in output.iter_mut().enumerate() {
            *x = *x * m::RECURRENT_Y_SCALE.get(i) + m::RECURRENT_Y_MEAN.get(i);
        }
        output
    }
    fn stabilize_pitch(&self, index: i32, values: &mut Controls) {
        let octave = 2.0_f32.ln();
        for block in 0..4 {
            let confidence = values[70 + block];
            if confidence >= 0.8 {
                continue;
            }
            let mut nearby: [f32; 5] = std::array::from_fn(|j| {
                let sub =
                    (index * 4 + block as i32 + j as i32 - 2).clamp(0, self.count as i32 * 4 - 1);
                self.at(sub / 4).periods[sub as usize % 4].clamp(20.0, 160.0)
            });
            nearby.sort_by(f32::total_cmp);
            let original = values[74 + block].ln();
            let correction = (nearby[2].ln() - original).clamp(-octave, octave);
            values[74 + block] = (original + (1.0 - confidence / 0.8) * correction).exp();
        }
    }
    fn refine(&mut self, index: i32, values: &mut Controls) {
        if !m::REFINEMENT_ENABLED {
            return;
        }
        for half in 0..2 {
            let features = &self.at(index).features;
            let state = usize::from(features[38 + half] != 0.0);
            let category = |field: usize| {
                let mut code = 0;
                for j in usize::from(field == 0)..m::PROFILE_LENGTHS[field] {
                    let local = m::PROFILE_GROUPS[field][j];
                    let bit = native_bit(half, local);
                    code = 2 * code + usize::from(features[40 + bit] < 0.0);
                }
                code
            };
            let rows = [
                state * 256 + category(0),
                state * 256 + 128 + category(1),
                state * 256 + 192 + category(2),
            ];
            let mut input = [0.0; 36];
            input[..32].copy_from_slice(&values[6 + half * 32..38 + half * 32]);
            input[32] = values[4];
            input[33] = values[5].clamp(20.0, 160.0).ln();
            input[34] = values[half * 2];
            input[35] = values[half * 2 + 1];
            let gate = (((input[34] + input[35]) * 0.5 + 60.0) / 10.0).clamp(0.0, 1.0);
            normalize_in_place(
                &mut input,
                m::REFINEMENT_SPECTRAL_MEAN,
                m::REFINEMENT_SPECTRAL_SCALE,
                true,
            );
            let mut delta = [0.0; 32];
            affine(
                &input,
                m::REFINEMENT_SPECTRAL_WEIGHT,
                m::REFINEMENT_SPECTRAL_BIAS.data,
                &mut delta,
            );
            for j in 0..16 {
                for row in rows {
                    delta[j] += m::REFINEMENT_SPECTRAL_TABLE.get(row * 16 + j);
                }
                delta[j] *= gate;
            }
            self.numeric.bound_correction(&mut delta);
            for j in 0..32 {
                values[6 + half * 32 + j] += delta[j];
            }
        }
        for block in 0..4 {
            let mut input = [0.0; 37];
            input[0] = values[74 + block].clamp(20.0, 160.0).ln();
            input[1] = values[5].clamp(20.0, 160.0).ln();
            input[2] = values[70 + block];
            input[3] = values[block];
            input[4] = (block % 2) as f32;
            for j in 0..9 {
                let pos =
                    (index * 4 + block as i32 + j as i32 - 4).clamp(0, self.count as i32 * 4 - 1);
                let period = self.at(pos / 4).periods[pos as usize % 4];
                input[5 + j] = period.clamp(20.0, 160.0).ln() - input[0];
                input[28 + j] = input[2] * input[5 + j];
            }
            let features = &self.at(index).features;
            input[14..21].copy_from_slice(&features[17 * (block / 2)..17 * (block / 2) + 7]);
            input[21..28]
                .copy_from_slice(&features[17 * (1 - block / 2)..17 * (1 - block / 2) + 7]);
            normalize_in_place(
                &mut input,
                m::REFINEMENT_PITCH_MEAN,
                m::REFINEMENT_PITCH_SCALE,
                true,
            );
            let mut delta = [0.0];
            affine(
                &input,
                m::REFINEMENT_PITCH_WEIGHT,
                m::REFINEMENT_PITCH_BIAS.data,
                &mut delta,
            );
            let gate = ((values[block] + 60.0) / 10.0).clamp(0.0, 1.0);
            let limit = 2.0_f32.ln() * 0.5;
            values[74 + block] = (values[74 + block].ln() + gate * delta[0].clamp(-limit, limit))
                .exp()
                .clamp(20.0, 160.0);
        }
    }
    pub fn emit(&mut self) -> Result<Prediction, ()> {
        let index = self.cursor as i32;
        let baseline = self.baseline(index);
        let mut context_input = [0.0; 1007];
        context_input[..17].copy_from_slice(&baseline);
        for offset in -2..=2 {
            let p = 17 + (offset + 2) as usize * 198;
            context_input[p..p + 198].copy_from_slice(&self.at(index + offset).features);
        }
        let (spectrum, spectral_hidden) = Self::mlp(&m::SPECTRUM, &context_input);
        let (delta, _) = Self::mlp(&m::CONTEXT, &context_input);
        let current: [f32; 17] =
            std::array::from_fn(|j| baseline[j] + m::CONTEXT_BLEND[j] * delta[j]);
        let before = self.previous_context.unwrap_or(current);
        self.previous_context = Some(current);
        let mut level_input = [0.0; 51];
        level_input[..17].copy_from_slice(&baseline);
        level_input[17..34].copy_from_slice(&before);
        level_input[34..].copy_from_slice(&current);
        let (level, level_hidden) = Self::mlp(&m::LEVELS, &level_input);
        let mut features = [0.0; 81];
        features[17..49].copy_from_slice(&level_hidden);
        features[49..].copy_from_slice(&spectral_hidden);
        for j in 0..17 {
            features[j] =
                baseline[j] + m::LEVEL_BLEND[j] * level[j] + m::SPECTRAL_BLEND[j] * spectrum[j];
        }
        let mut values = [0.0; 82];
        for i in 0..4 {
            values[i] = features[0] + features[i + 1];
        }
        values[4] = ((features[5] - 0.35) / 0.65).clamp(0.0, 1.0);
        values[5] = features[6];
        self.numeric
            .lpc_cepstrum(&features[7..], &mut values[6..38]);
        values.copy_within(6..38, 38);
        let residual = self.gru(&features);
        for i in 0..70 {
            values[i] += m::RECURRENT_BLEND[i] * residual[i];
        }
        values[4] = values[4].clamp(0.0, 1.0);
        values[5] = values[5].clamp(20.0_f32.ln(), 160.0_f32.ln()).exp();
        for i in 0..4 {
            values[70 + i] = values[4];
            values[74 + i] = (m::PITCH_BLEND * self.at(index).periods[i].clamp(20.0, 160.0).ln()
                + (1.0 - m::PITCH_BLEND) * values[5].ln())
            .exp();
        }
        for half in 0..2 {
            let mut input = [0.0; 67];
            input[..32].copy_from_slice(&values[6 + half * 32..38 + half * 32]);
            input[32] = values[4];
            input[33] = values[5].clamp(20.0, 160.0).ln();
            let a = values[half * 2].clamp(-120.0, -3.0) * (10.0_f32.ln() / 10.0);
            let b = values[half * 2 + 1].clamp(-120.0, -3.0) * (10.0_f32.ln() / 10.0);
            input[34] = (a.max(b) + (1.0 + (-(a - b).abs()).exp()).ln() - 2.0_f32.ln())
                * (10.0 / 10.0_f32.ln());
            for i in 0..32 {
                input[35 + i] = values[4] * input[i];
            }
            normalize_in_place(&mut input, m::CALIBRATION_MEAN, m::CALIBRATION_SCALE, true);
            let mut delta = [0.0; 32];
            affine(
                &input,
                m::CALIBRATION_WEIGHT,
                m::CALIBRATION_BIAS.data,
                &mut delta,
            );
            self.numeric.bound_correction(&mut delta);
            for i in 0..32 {
                values[6 + half * 32 + i] += delta[i];
            }
        }
        self.stabilize_pitch(index, &mut values);
        self.refine(index, &mut values);
        if !values.iter().all(|x| x.is_finite()) {
            return Err(());
        }
        let chime = select_chime(&mut self.buffer[..self.count], self.cursor);
        let chime_end = chime != 0
            && (self.cursor + 1 == self.count
                || select_chime(&mut self.buffer[..self.count], self.cursor + 1) != chime);
        let prediction = Prediction {
            controls: values,
            chime,
            chime_end,
            periods: self.at(index).periods,
            bits: std::array::from_fn(|i| u8::from(self.at(index).features[40 + i] < 0.0)),
        };
        self.cursor += 1;
        if self.cursor > 4 {
            let discard = self.cursor - 4;
            self.buffer.copy_within(discard..self.count, 0);
            self.count -= discard;
            self.cursor -= discard;
        }
        Ok(prediction)
    }
    pub fn decode(&mut self, payload: &Payload) -> Result<Option<Prediction>, ()> {
        if payload[19] & 0xc0 != 0 || self.count == self.buffer.len() {
            return Err(());
        }
        self.buffer[self.count] = encode_features(payload);
        self.count += 1;
        if self.pending() > LOOKAHEAD {
            self.emit().map(Some)
        } else {
            Ok(None)
        }
    }
}
pub fn native_bit(half: usize, local: usize) -> usize {
    if local < 39 {
        half * 39 + local
    } else if local < 59 {
        78 + half * 20 + local - 39
    } else {
        118 + half * 20 + local - 59
    }
}

use super::config;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gains {
    pub pitch: i16,
    pub code: i16,
}
#[derive(Clone, Debug)]
pub struct GainState {
    pub history: [f32; 4],
}
fn quantize(x: f32) -> i16 {
    x.round_ties_even().clamp(0.0, 32767.0) as i16
}
impl GainState {
    pub fn decode(&mut self, index: usize, code: &[i16; 80]) -> Gains {
        let energy = code.iter().map(|&c| (c as i64) * (c as i64)).sum::<i64>() as f32 / 80.0;
        let prediction = (((self.history[0] * 0.68 + self.history[1] * 0.5)
            + self.history[2] * 0.33)
            + self.history[3] * 0.16)
            - 0.5 * energy.max(1.0).ln()
            + config::gain_bias();
        let correction = config::log_gain(index);
        let gains = Gains {
            pitch: quantize(config::pitch_gain(index)),
            code: quantize((prediction + correction).exp()),
        };
        self.history.rotate_right(1);
        self.history[0] = correction;
        gains
    }
}

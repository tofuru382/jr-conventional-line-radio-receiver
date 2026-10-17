//! Stateful mixed-excitation synthesis, using only Rust and immutable models.
use crate::models::model_data as m;
use crate::numeric::{affine, normalize_in_place, sample, Numeric};
use crate::predictor::Controls;
pub type Pcm = [i16; 320];

pub struct VoiceNoise {
    state: u128,
}
impl Default for VoiceNoise {
    fn default() -> Self {
        Self {
            state: 0xd335243fe783c82b844bb47f168b1cd2,
        }
    }
}
impl VoiceNoise {
    pub fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(0x2360ed051fc65da44385df649fccf645)
            .wrapping_add(0x8206c549833fe2ef4a7c4f547dcdc3af);
        let hi = (self.state >> 64) as u64;
        (hi ^ self.state as u64).rotate_right((hi >> 58) as u32)
    }
    pub fn uniform(&mut self) -> f32 {
        -3.0_f32.sqrt()
            + 2.0 * 3.0_f32.sqrt() * ((self.next_u64() >> 11) as f32 * (1.0 / 9007199254740992.0))
    }
}
pub struct Vocoder {
    pub numeric: Numeric,
    noise: VoiceNoise,
    previous_features: [f32; 28],
    feature_average: [f32; 28],
    spectrum_average: [f32; 32],
    history: [f32; 255],
    phase: f32,
    previous_pulse: f32,
    period: f32,
    fraction: f32,
    power: f32,
    level: f32,
    has_features: bool,
    has_spectrum: bool,
    has_excitation: bool,
    has_power: bool,
    has_level: bool,
}
impl Default for Vocoder {
    fn default() -> Self {
        Self {
            numeric: Numeric::default(),
            noise: VoiceNoise::default(),
            previous_features: [0.0; 28],
            feature_average: [0.0; 28],
            spectrum_average: [0.0; 32],
            history: [0.0; 255],
            phase: 0.0,
            previous_pulse: 0.0,
            period: 0.0,
            fraction: 0.0,
            power: 0.0,
            level: 0.0,
            has_features: false,
            has_spectrum: false,
            has_excitation: false,
            has_power: false,
            has_level: false,
        }
    }
}
impl Vocoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    fn limit(values: &mut [f32]) -> bool {
        const CEILING: f32 = 32767.0 / 32768.0;
        let mut peak = 0.0_f32;
        for &x in values.iter() {
            if !x.is_finite() {
                return false;
            }
            peak = peak.max(x.abs());
        }
        if peak > CEILING {
            for x in values {
                *x = (*x * (CEILING / peak)).clamp(-CEILING, CEILING);
            }
        }
        true
    }
    fn excitation(
        &mut self,
        fraction: f32,
        mut period: f32,
        output: &mut [f32],
        native_period: bool,
    ) {
        let mut mapped = if fraction == 0.0 || fraction == 1.0 {
            fraction
        } else {
            -(4.0 * (-fraction).ln_1p()).exp_m1()
        };
        if self.has_excitation {
            mapped = 0.0625 * self.fraction + (1.0 - 0.0625) * mapped;
            if !native_period {
                period = (0.5 * self.period.ln() + 0.5 * period.ln())
                    .exp()
                    .clamp(20.0, 160.0);
            }
        }
        self.fraction = mapped;
        self.period = period;
        self.has_excitation = true;
        let root_period = period.sqrt();
        let normalization = (1.0 - 1.0 / period).sqrt();
        for (i, x) in output.iter_mut().take(80).enumerate() {
            let pulse = (((self.phase + (i + 1) as f32 / period).floor()
                - (self.phase + i as f32 / period).floor())
                * root_period
                - 1.0 / root_period)
                / normalization;
            let smoothed = 0.875 * pulse + 0.125 * self.previous_pulse;
            self.previous_pulse = pulse;
            let noise = self.noise.uniform();
            *x = if mapped == 0.0 {
                noise
            } else {
                mapped.sqrt() * smoothed + (1.0 - mapped).sqrt() * noise
            };
        }
        self.phase = (self.phase + 80.0 / period) % 1.0;
    }
    fn apply_policy(&mut self, values: &mut Controls) {
        if m::VOICING_ALWAYS_NEUTRAL {
            return;
        }
        let mut current = [0.0; 28];
        for i in 0..4 {
            current[i] = values[i].clamp(-120.0, -3.0);
            current[4 + i] = values[70 + i].clamp(0.0, 1.0);
            current[8 + i] = values[74 + i].clamp(20.0, 160.0).ln();
        }
        for i in 0..8 {
            current[12 + i] = values[6 + i].clamp(-4.0, 4.0);
            current[20 + i] = values[38 + i].clamp(-4.0, 4.0);
        }
        let mut input = [0.0; 84];
        for i in 0..28 {
            input[i] = current[i];
            input[28 + i] = if self.has_features {
                current[i] - self.previous_features[i]
            } else {
                0.0
            };
            input[56 + i] = if self.has_features {
                0.75 * self.feature_average[i] + 0.25 * current[i]
            } else {
                current[i]
            };
            self.previous_features[i] = current[i];
            self.feature_average[i] = input[56 + i];
        }
        self.has_features = true;
        normalize_in_place(&mut input, m::VOICING_X_MEAN, m::VOICING_X_SCALE, true);
        let mut costs = [0.0; 2];
        affine(&input, m::VOICING_WEIGHT, m::VOICING_BIAS.data, &mut costs);
        let (low, high) = (costs[0].clamp(-40.0, 40.0), costs[1].clamp(-40.0, 40.0));
        let exponent = if low < 0.0 && low <= high {
            0.5
        } else if high < 0.0 {
            2.0
        } else {
            return;
        };
        for x in &mut values[70..74] {
            *x = (-(exponent * (-x.clamp(0.0, 1.0)).ln_1p()).exp_m1()).clamp(0.0, 1.0);
        }
    }
    fn apply_spectrum(&mut self, values: &mut Controls, separate: bool) {
        let original: [f32; 64] = values[6..70].try_into().unwrap();
        for half in 0..2 {
            let mut proposal = [0.0; 32];
            let mut delta = [0.0; 32];
            for i in 0..32 {
                let middle = 0.5 * original[i] + 0.5 * original[32 + i];
                let normalized = (i as i32 + 1 - 8).max(0) as f32 / 24.0;
                let value = (if separate {
                    original[half * 32 + i]
                } else {
                    0.5 * original[half * 32 + i] + 0.5 * middle
                }) * (-0.5 * normalized * normalized).exp();
                proposal[i] = if self.has_spectrum {
                    0.75 * value + 0.25 * self.spectrum_average[i]
                } else {
                    value
                };
                delta[i] = proposal[i] - original[half * 32 + i];
            }
            self.numeric.bound_correction(&mut delta);
            for i in 0..32 {
                values[6 + half * 32 + i] = original[half * 32 + i] + delta[i];
            }
            self.spectrum_average = proposal;
            self.has_spectrum = true;
        }
    }
    pub fn process(
        &mut self,
        parameters: &Controls,
        native_period: bool,
        separate_spectra: bool,
    ) -> Result<Pcm, ()> {
        if !parameters.iter().all(|x| x.is_finite()) {
            return Err(());
        }
        let mut values = *parameters;
        self.apply_policy(&mut values);
        self.apply_spectrum(&mut values, separate_spectra);
        let levels: [f32; 4] = std::array::from_fn(|i| values[i].clamp(-120.0, -3.0));
        let mut pcm = [0.0; 320];
        let mut impulse = [0.0; 256];
        for block in 0..4 {
            if block % 2 == 0
                && !self
                    .numeric
                    .envelope_impulse(&values[6 + (block / 2) * 32..], &mut impulse)
            {
                return Err(());
            }
            let slope = (values[78 + block] + levels[block]
                - if self.has_level {
                    self.level
                } else {
                    levels[block]
                })
            .clamp(-18.0, 18.0);
            let mut extended = [0.0; 335];
            extended[..255].copy_from_slice(&self.history);
            self.excitation(
                values[70 + block].clamp(0.0, 1.0),
                values[74 + block].clamp(20.0, 160.0),
                &mut extended[255..],
                native_period,
            );
            let mut slope_gain = 10.0_f32.powf(-slope / 40.0);
            let slope_step = 10.0_f32.powf(slope / (20.0 * 79.0));
            let mut shaped = [0.0; 80];
            for i in 0..80 {
                for j in 0..128 {
                    shaped[i] += extended[255 + i - j] * impulse[j];
                }
            }
            for sample in &mut shaped {
                *sample *= slope_gain;
                slope_gain *= slope_step;
            }
            self.history.copy_from_slice(&extended[80..]);
            let power = shaped.iter().fold(0.0, |sum, x| sum + x * x) / 80.0;
            if !power.is_finite() || power <= 0.0 {
                return Err(());
            }
            self.power = if self.has_power {
                0.95 * self.power + (1.0 - 0.95) * power
            } else {
                power
            };
            self.has_power = true;
            let bound = 10.0_f32.powf(0.6);
            let denominator = self.power.clamp(power / bound, power * bound);
            let gain = 10.0_f32.powf(levels[block] / 20.0) / denominator.sqrt();
            for x in &mut shaped {
                *x *= gain;
            }
            if !Self::limit(&mut shaped) {
                return Err(());
            }
            pcm[block * 80..block * 80 + 80].copy_from_slice(&shaped);
            self.level = levels[block];
            self.has_level = true;
        }
        let own = pcm.iter().fold(0.0, |sum, x| sum + x * x) / 320.0;
        let target = levels
            .iter()
            .fold(0.0, |sum, level| sum + 10.0_f32.powf(level / 10.0))
            / 4.0;
        let restoration = (target / own).sqrt();
        for x in &mut pcm {
            *x *= restoration;
        }
        if !Self::limit(&mut pcm) {
            return Err(());
        }
        Ok(pcm.map(sample))
    }
}

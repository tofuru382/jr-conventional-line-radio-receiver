//! Integer excitation and decoded spectral/gain state for shared synthesis.
//!
//! This module reuses and adapts parts of
//! [mcelp-codec](https://github.com/soltia48/mcelp-codec), including the integer DSP
//! code and tables in `fixed_core`. They are integrated with Modified RL-CELP
//! parameter decoding and gain/spectral state handling.
//! The reused code is Copyright (c) 2026 KIRISHIKI Yudai and is licensed under the
//! MIT License; the original notice and license text are retained in `src/celp/LICENSE`.
mod bits;
mod config;
mod core;
mod fixed_core;
mod gain;
mod post_fec;
mod spectral;

pub struct Observation {
    pub excitation: [i16; 320],
    pub pcm: [i16; 320],
    pub gain: [[f32; 12]; 4],
}
pub struct Native {
    core: core::Core,
    spectrum: spectral::Spectral,
    gain: gain::GainState,
}
impl Default for Native {
    fn default() -> Self {
        let mut initial = 0.0_f32;
        for i in 0..128 {
            initial += config::log_gain(i);
        }
        Self {
            core: core::Core::new(),
            spectrum: spectral::Spectral::default(),
            gain: gain::GainState {
                history: [initial / 128.0; 4],
            },
        }
    }
}
impl Native {
    pub fn decode(&mut self, payload: &[u8; 20]) -> Result<Observation, ()> {
        let mut params = post_fec::unpack(payload).map_err(|_| ())?;
        let mut low = 0;
        for k in 74..78 {
            low = 2 * low + ((payload[k / 8] >> (k % 8)) & 1);
        }
        // These two interpretation choices are empirically qualified model
        // assumptions, not assertions of a fully recovered bit specification.
        params.halves[1].gain_low[1] = Some(low);
        params.halves[0].absolute_pitch ^= 3;
        let mut result = Observation {
            excitation: [0; 320],
            pcm: [0; 320],
            gain: [[0.0; 12]; 4],
        };
        for h in 0..2 {
            let lsp = self.spectrum.decode(&params.halves[h]);
            let (pcm, traces) = self.core.decode(&lsp, &params.halves[h], &mut self.gain);
            result.pcm[h * 160..(h + 1) * 160].copy_from_slice(&pcm);
            for s in 0..2 {
                let t = &traces[s];
                result.excitation[h * 160 + s * 80..h * 160 + (s + 1) * 80]
                    .copy_from_slice(&t.mixed_excitation);
                let index = t.gain_index;
                let v = t.gain_history;
                let correction = config::log_gain(index);
                let amplitude = (((v[0] * 0.68 + v[1] * 0.5) + v[2] * 0.33) + v[3] * 0.16)
                    + config::gain_bias()
                    + correction;
                let energy = t
                    .innovation
                    .iter()
                    .map(|&x| (x as i64) * (x as i64))
                    .sum::<i64>() as f32
                    / 80.0;
                result.gain[h * 2 + s] = [
                    index as f32,
                    correction,
                    v[0],
                    v[1],
                    v[2],
                    v[3],
                    amplitude,
                    energy,
                    t.gains.code as f32,
                    t.gains.pitch as f32,
                    t.lag.integer as f32,
                    t.code_class as f32,
                ];
            }
        }
        Ok(result)
    }
}

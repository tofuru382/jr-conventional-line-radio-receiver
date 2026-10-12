use super::fixed_core::{
    codebook, excitation,
    fixed::{acc, hi, mul, sat, shift},
    lsp, pitch, postfilter, synth, tables,
};
use super::{
    bits::HalfParams,
    gain::{GainState, Gains},
};

#[derive(Clone)]
pub struct Core {
    pub previous_lsp: [i16; 10],
    pub excitation: [i16; 314],
    pub synthesis_memory: [i16; 10],
    pub sharpen_fixed: i16,
    pub sharpen_voiced: i16,
    pub last_half_lag: i16,
    pub previous_lag: i16,
    pub output_filter: postfilter::OutputFilter,
}
#[derive(Clone, Debug)]
pub struct SubframeTrace {
    pub lag: pitch::Lag,
    pub code_class: usize,
    pub gain_index: usize,
    pub gains: Gains,
    pub gain_history: [f32; 4],
    pub innovation: [i16; 80],
    pub mixed_excitation: [i16; 80],
}
impl Core {
    pub fn new() -> Self {
        Self {
            previous_lsp: tables::LSP_MEAN[..10].try_into().unwrap(),
            excitation: [0; 314],
            synthesis_memory: [0; 10],
            sharpen_fixed: 3277,
            sharpen_voiced: 3277,
            last_half_lag: 60,
            previous_lag: 60,
            output_filter: postfilter::OutputFilter::default(),
        }
    }
    pub fn decode(
        &mut self,
        lsp: &[i16; 10],
        params: &HalfParams,
        gain: &mut GainState,
    ) -> ([i16; 160], [SubframeTrace; 2]) {
        let (lpc0, lpc1) = lsp::interpolate_pair(&self.previous_lsp, lsp);
        self.previous_lsp = *lsp;
        let mut out = [0i16; 160];
        let traces = std::array::from_fn(|sub| {
            let lpc = if sub == 0 { lpc0 } else { lpc1 };
            let lag = if sub == 0 {
                pitch::decode_absolute(params.absolute_pitch as i16)
            } else {
                pitch::decode_relative(params.relative_pitch as i16, self.previous_lag)
            };
            self.previous_lag = lag.integer;
            let at = 154 + 80 * sub;
            pitch::predict(&mut self.excitation, at, &lag);
            let reflection = lsp::reflection_coefficients(&lpc);
            (self.sharpen_fixed, self.sharpen_voiced) = excitation::interpolation_gains(
                reflection[0],
                self.sharpen_fixed,
                lag.integer,
                self.last_half_lag,
            );
            let innovation = codebook::decode(params.code[sub]);
            let mut code = innovation.code;
            if lag.integer < 80 {
                let g = if innovation.class == 5 || (1..3).contains(&innovation.class) {
                    self.sharpen_voiced
                } else {
                    self.sharpen_fixed
                };
                pitch::sharpen(&mut code, &lag, g);
            }
            let gain_history = gain.history;
            let gains = gain.decode(params.gain_index(sub), &code);
            self.sharpen_fixed = gains.pitch.clamp(3277, 13017);
            for (n, &c) in code.iter().enumerate() {
                let adaptive = mul(self.excitation[at + n], gains.pitch);
                let fixed = mul(c, gains.code);
                self.excitation[at + n] =
                    hi(sat(acc(shift(acc(adaptive + shift(fixed, 1)), 1) + 0x8000)));
            }
            synth::synthesis(
                &lpc,
                &self.excitation[at..at + 80],
                &mut out[80 * sub..80 * (sub + 1)],
                &mut self.synthesis_memory,
            );
            SubframeTrace {
                lag,
                code_class: innovation.class,
                gain_index: params.gain_index(sub),
                gains,
                gain_history,
                innovation: code,
                mixed_excitation: self.excitation[at..at + 80].try_into().unwrap(),
            }
        });
        self.excitation.copy_within(160.., 0);
        self.last_half_lag = self.previous_lag;
        self.output_filter.run(&mut out);
        (out, traces)
    }
}

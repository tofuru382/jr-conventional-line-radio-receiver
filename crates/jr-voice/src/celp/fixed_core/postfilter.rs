//! Two cascaded output reconstruction sections.

use crate::celp::fixed_core::fixed::{acc, hi, mul, mul32x16, sat, shift, trunc32};

/// State of the output reconstruction filter.
#[derive(Clone, Default)]
pub struct OutputFilter {
    /// Feed-forward delay lines, three samples per section.
    fir: [i16; 6],
    /// Feedback delay lines, three 32-bit values per section.
    iir: [i64; 6],
}

/// Final output scaling, Q15.
const OUT_SCALE: i16 = 19661;

impl OutputFilter {
    /// Advance one of the two cascaded reconstruction sections.
    fn filter_section(&mut self, section: usize, value: i16) -> i16 {
        let ff = &crate::celp::fixed_core::tables::OUT_FIR[..4];
        let fb = &crate::celp::fixed_core::tables::OUT_IIR[..3];
        let at = section * 3;
        let fir = &mut self.fir[at..at + 3];
        let iir = &mut self.iir[at..at + 3];

        let mut b = acc((value as i64) * (ff[0] as i64) * 2);
        for k in 0..3 {
            b = acc(b + mul(fir[k], ff[k + 1]));
        }
        fir[2] = fir[1];
        fir[1] = fir[0];
        fir[0] = value;

        for k in 0..3 {
            b = acc(b - shift(mul32x16(iir[k], fb[k]), 1));
        }
        iir[2] = iir[1];
        iir[1] = iir[0];
        iir[0] = trunc32(shift(b, 1));

        hi(sat(acc(shift(b, 2) + (1 << 15))))
    }

    /// Advance both cascaded sections and apply the final output scaling.
    fn filter_sample(&mut self, mut value: i16) -> i16 {
        for section in 0..2 {
            value = self.filter_section(section, value);
        }
        let scaled = acc((value as i64) * (OUT_SCALE as i64) * 2);
        hi(sat(acc(shift(scaled, 1) + (1 << 15))))
    }

    /// Run one half-frame through the two cascaded sections.
    pub fn run(&mut self, block: &mut [i16]) {
        for sample in block.iter_mut() {
            *sample = self.filter_sample(*sample);
        }
    }
}

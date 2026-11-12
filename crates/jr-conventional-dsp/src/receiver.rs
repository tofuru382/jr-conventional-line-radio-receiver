//! Safe, owned interface for the standalone receiver.
use super::{Complex32, Demodulator, Snapshot};

pub struct Dsp(Demodulator);
impl Dsp {
    pub fn new(rate: f64, offset: f64) -> Result<Self, String> {
        if !rate.is_finite() || !(9600.0..=20_000_000.0).contains(&rate) {
            return Err("IQ sample rate must be 9,600–20,000,000 Hz".into());
        }
        if !offset.is_finite() || offset.abs() + 4200.0 >= rate / 2.0 {
            return Err("Channel lies outside the IQ sample bandwidth".into());
        }
        Ok(Self(Demodulator::new(rate, offset, 0)))
    }
    pub fn process(&mut self, iq: &[Complex32], bits: &mut Vec<u8>) -> Result<Snapshot, String> {
        if iq
            .iter()
            .any(|s| !s.real.is_finite() || !s.imaginary.is_finite())
        {
            return Err("IQ contains non-finite samples".into());
        }
        bits.resize(iq.len() * 2 + 4, 0);
        let count = self.0.process(iq, bits);
        bits.truncate(count);
        Ok(self.0.snapshot())
    }
    /// As [`Self::process`], plus two bit reliabilities per dibit (MSB first,
    /// 0 = erased or contradicted, 255 = strongest) in `soft`.
    pub fn process_soft(
        &mut self,
        iq: &[Complex32],
        bits: &mut Vec<u8>,
        soft: &mut Vec<[u8; 2]>,
    ) -> Result<Snapshot, String> {
        if iq
            .iter()
            .any(|s| !s.real.is_finite() || !s.imaginary.is_finite())
        {
            return Err("IQ contains non-finite samples".into());
        }
        bits.resize(iq.len() * 2 + 4, 0);
        soft.resize(iq.len() * 2 + 4, [0, 0]);
        let count = self.0.process_soft(iq, bits, Some(soft));
        bits.truncate(count);
        soft.truncate(count);
        Ok(self.0.snapshot())
    }
    /// Also run the alternate detector (drift RLS tracking for fast
    /// time-varying simulcast channels) for [`Self::process_dual`].
    pub fn enable_alternate(&mut self) {
        self.0.enable_alternate();
    }
    /// Primary (LMS) and alternate (drift RLS) decision streams with
    /// reliabilities, aligned index for index.
    pub fn process_dual(
        &mut self,
        iq: &[Complex32],
        bits: &mut [Vec<u8>; 2],
        soft: &mut [Vec<[u8; 2]>; 2],
    ) -> Result<Snapshot, String> {
        if iq
            .iter()
            .any(|s| !s.real.is_finite() || !s.imaginary.is_finite())
        {
            return Err("IQ contains non-finite samples".into());
        }
        for b in bits.iter_mut() {
            b.resize(iq.len() * 2 + 4, 0);
        }
        for q in soft.iter_mut() {
            q.resize(iq.len() * 2 + 4, [0, 0]);
        }
        let [b0, b1] = bits;
        let [q0, q1] = soft;
        let count = self.0.process_dual(iq, [b0, b1], [q0, q1]);
        for b in bits.iter_mut() {
            b.truncate(count);
        }
        for q in soft.iter_mut() {
            q.truncate(count);
        }
        Ok(self.0.snapshot())
    }
    /// Differential constellation and EVM after receive filtering, timing,
    /// carrier and survivor-channel correction; preserves received scatter.
    pub fn signal(&self) -> (Vec<[f32; 2]>, f64) {
        let mut symbols = [Complex32::default(); 256];
        let (count, evm) = self.0.read_signal(&mut symbols);
        (
            symbols[..count]
                .iter()
                .map(|s| [s.real, s.imaginary])
                .collect(),
            evm,
        )
    }
}

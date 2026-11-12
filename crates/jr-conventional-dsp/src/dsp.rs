//! Receiver DSP. All IQ arithmetic lives in this Rust module.
//! Preserve the receiver filters and JR label convention; equalize before decisions.

use crate::equalizer::{Equalizer, TAPS};
use crate::sequence::{Sequence, Tracking};
use crate::signal::SignalView;
use std::f64::consts::PI;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Complex32 {
    pub real: f32,
    pub imaginary: f32,
}

fn low_pass(count: usize, cutoff: f64, rate: f64) -> Vec<f32> {
    let mut taps = vec![0.0; count];
    let mut sum = 0.0;
    for (i, tap) in taps.iter_mut().enumerate() {
        let x = i as f64 - (count - 1) as f64 / 2.0;
        let omega = 2.0 * PI * cutoff / rate;
        *tap = ((if x == 0.0 {
            omega
        } else {
            (omega * x).sin() / x
        }) * (0.54 - 0.46 * (2.0 * PI * i as f64 / (count - 1) as f64).cos()))
            as f32;
        sum += *tap as f64;
    }
    for tap in &mut taps {
        *tap /= sum as f32;
    }
    taps
}

fn root_raised_cosine(mut count: usize, sps: f64, gain: f64) -> Vec<f32> {
    const A: f64 = 0.2;
    if count & 1 == 0 {
        count += 1;
    }
    let mut taps = vec![0.0; count];
    let mut sum = 0.0;
    for (i, tap) in taps.iter_mut().enumerate() {
        let t = (i as f64 - (count - 1) as f64 / 2.0) / sps;
        let value = if t == 0.0 {
            1.0 - A + 4.0 * A / PI
        } else if ((4.0 * A * t).abs() - 1.0).abs() < 1e-7 {
            A / 2.0_f64.sqrt()
                * ((1.0 + 2.0 / PI) * (PI / (4.0 * A)).sin()
                    + (1.0 - 2.0 / PI) * (PI / (4.0 * A)).cos())
        } else {
            ((PI * t * (1.0 - A)).sin() + 4.0 * A * t * (PI * t * (1.0 + A)).cos())
                / (PI * t * (1.0 - 16.0 * A * A * t * t))
        };
        *tap = value as f32;
        sum += *tap as f64;
    }
    for tap in &mut taps {
        *tap *= (gain / sum) as f32;
    }
    taps
}

fn arms(taps: &[f32], phases: usize) -> Vec<Vec<f32>> {
    let count = taps.len().div_ceil(phases);
    let mut result = vec![vec![0.0; count]; phases];
    for (phase, arm) in result.iter_mut().enumerate() {
        for j in 0..count {
            if phase + j * phases < taps.len() {
                arm[count - 1 - j] = taps[phase + j * phases];
            }
        }
    }
    result
}

struct Resampler {
    decimation: usize,
    integrators: [i64; 6],
    delays: [i64; 6],
    cic_scale: f64,
    counter: usize,
    arms: Vec<Vec<f32>>,
    ring: Vec<Complex32>,
    half: i64,
    step: f64,
    input_index: i64,
    next_output: f64,
    write: usize,
}

impl Resampler {
    fn new(input_rate: f64) -> Self {
        let decimation = ((input_rate / 256000.0).round() as usize).max(1);
        let d = decimation as f64;
        let rate = input_rate / d;
        let count = ((0.0015 * rate).round() as usize).max(8);
        let half = count / 2;
        let mut prototype = low_pass(
            32 * count + 1,
            4200.0_f64.min(0.45 * rate.min(19200.0)),
            32.0 * rate,
        );
        prototype.pop();
        for tap in &mut prototype {
            *tap *= 32.0;
        }
        Self {
            decimation,
            integrators: [0; 6],
            delays: [0; 6],
            cic_scale: 1.0 / (32768.0 * d * d * d),
            counter: 0,
            arms: arms(&prototype, 32),
            ring: vec![Complex32::default(); count + 16],
            half: half as i64,
            step: rate / 19200.0,
            input_index: 0,
            next_output: half as f64,
            write: 0,
        }
    }

    fn process(&mut self, mut value: Complex32, sink: &mut impl FnMut(Complex32)) {
        if self.decimation > 1 {
            let mut real = (value.real as f64 * 32768.0).round() as i64;
            let mut imaginary = (value.imaginary as f64 * 32768.0).round() as i64;
            for i in 0..3 {
                self.integrators[i] = self.integrators[i].wrapping_add(real);
                self.integrators[i + 3] = self.integrators[i + 3].wrapping_add(imaginary);
                real = self.integrators[i];
                imaginary = self.integrators[i + 3];
            }
            self.counter += 1;
            if self.counter < self.decimation {
                return;
            }
            self.counter = 0;
            for i in 0..3 {
                let previous_real = real;
                let previous_imaginary = imaginary;
                real = real.wrapping_sub(self.delays[i]);
                imaginary = imaginary.wrapping_sub(self.delays[i + 3]);
                self.delays[i] = previous_real;
                self.delays[i + 3] = previous_imaginary;
            }
            value = Complex32 {
                real: (real as f64 * self.cic_scale) as f32,
                imaginary: (imaginary as f64 * self.cic_scale) as f32,
            };
        }
        self.ring[self.write] = value;
        self.write = (self.write + 1) % self.ring.len();
        self.input_index += 1;
        loop {
            let mut position = self.next_output.floor() as i64;
            let mut phase = ((self.next_output - position as f64) * 32.0).round() as usize;
            if phase == 32 {
                phase = 0;
                position += 1;
            }
            let first = position - self.half;
            let taps = &self.arms[phase];
            if first + taps.len() as i64 > self.input_index {
                return;
            }
            let mut offset = (self.write as i64 + first - self.input_index)
                .rem_euclid(self.ring.len() as i64) as usize;
            let mut real = 0.0;
            let mut imaginary = 0.0;
            for tap in taps {
                real += self.ring[offset].real * tap;
                imaginary += self.ring[offset].imaginary * tap;
                offset = (offset + 1) % self.ring.len();
            }
            sink(Complex32 { real, imaginary });
            self.next_output += self.step;
        }
    }
}

struct ClockRecovery {
    arms: Vec<Vec<f32>>,
    derivatives: Vec<Vec<f32>>,
    ring: [Complex32; 256],
    start: usize,
    count: usize,
    position: i32,
    warm: bool,
    phase: f32,
    rate: f32,
}

impl ClockRecovery {
    fn new() -> Self {
        let taps = root_raised_cosine(1024, 128.0, 32.0);
        let mut derivatives = vec![0.0_f32; taps.len()];
        let mut sum = 0.0_f32;
        for i in 0..taps.len() - 2 {
            derivatives[i + 1] = taps[i + 2] - taps[i];
            sum += derivatives[i + 1].abs();
        }
        for value in &mut derivatives {
            *value *= 32.0 / sum;
        }
        Self {
            arms: arms(&taps, 32),
            derivatives: arms(&derivatives, 32),
            ring: [Complex32::default(); 256],
            start: 0,
            count: 0,
            position: 0,
            warm: false,
            phase: 16.0,
            rate: 0.0,
        }
    }

    fn process(&mut self, sample: Complex32) -> Option<([Complex32; TAPS], [Complex32; TAPS])> {
        assert!(
            self.count < self.ring.len(),
            "Clock recovery buffer overflow"
        );
        self.ring[(self.start + self.count) % self.ring.len()] = sample;
        self.count += 1;
        let length = self.arms[0].len();
        if !self.warm {
            if self.count < 6 + length + 4 + 2 * (TAPS - 1) {
                return None;
            }
            self.position = 6;
            self.warm = true;
        }
        let saved = self.phase;
        let mut arm = self.phase.floor() as i32;
        let mut shift = 0;
        while arm >= 32 {
            self.phase -= 32.0;
            arm -= 32;
            shift += 1;
        }
        while arm < 0 {
            self.phase += 32.0;
            arm += 32;
            shift -= 1;
        }
        let first = self.position + shift;
        if first < 0 || first as usize + length + 2 * (TAPS - 1) > self.count {
            self.phase = saved;
            return None;
        }
        let mut values = [Complex32::default(); TAPS];
        let mut derivatives = [Complex32::default(); TAPS];
        // Evaluate one common fractional phase on a half-symbol-spaced window.
        // Keeping the window complex preserves the amplitude needed by both
        // the fixed front end and the subsequent adaptive sequence detector.
        for (k, matched) in values.iter_mut().enumerate() {
            for i in 0..length {
                let value = self.ring[(self.start + first as usize + 2 * k + i) % self.ring.len()];
                matched.real += value.real * self.arms[arm as usize][i];
                matched.imaginary += value.imaginary * self.arms[arm as usize][i];
                derivatives[k].real += value.real * self.derivatives[arm as usize][i];
                derivatives[k].imaginary += value.imaginary * self.derivatives[arm as usize][i];
            }
        }
        self.position = (self.position + shift + 4).max(0);
        while self.position > 12 {
            self.position -= 1;
            self.count -= 1;
            self.start = (self.start + 1) % self.ring.len();
        }
        Some((values, derivatives))
    }

    fn update(&mut self, error: f32) {
        // Loop bandwidth. When two paths about half a symbol apart fade in
        // turn, the timing error swings between them and a wide loop slips
        // whole symbols. 0.01 halved slips with slow echoes in synthetic
        // channels and gained frames in the field capture without losing
        // flat-fading or frequency-offset margin; 0.005 cost fast-fading margin.
        const BW: f32 = 0.01;
        const DENOMINATOR: f32 = 1.0 + 128.0 * BW + BW * BW;
        const ALPHA: f32 = 256.0 * BW / DENOMINATOR;
        const BETA: f32 = 4.0 * BW * BW / DENOMINATOR;
        self.phase += self.rate;
        for _ in 0..4 {
            self.rate += BETA * error;
            self.phase += self.rate + ALPHA * error;
        }
        self.rate = self.rate.clamp(-0.1, 0.1);
    }
}

// The primary carrier loop and detector use LMS tracking (low noise) and
// drive the diagnostic constellation and lock state. When enabled, an
// alternate carrier loop and detector with drift RLS tracking (fast
// time-varying simulcast ISI) see the same matched-filter output; each loop
// follows its own channel estimate. Both decision streams leave at the same
// latency and the frame decoders use whichever passes their checks.
struct Carrier {
    phase: f64,
    frequency: f64,
    alternation: i32,
    locked: bool,
    error: f64,
    sequence: Sequence,
}
impl Carrier {
    fn new(tracking: Tracking) -> Self {
        Self {
            phase: 0.0,
            frequency: 0.0,
            alternation: 0,
            locked: false,
            error: 0.0,
            sequence: Sequence::new(tracking),
        }
    }
}
impl Carrier {
    fn process(&mut self, sample: Complex32, reliable: bool) -> Option<(u8, [u8; 2])> {
        let rotation = self.phase + self.alternation as f64 * PI / 4.0;
        let (sine, cosine) = rotation.sin_cos();
        let corrected = Complex32 {
            real: (sample.real as f64 * cosine + sample.imaginary as f64 * sine) as f32,
            imaginary: (sample.imaginary as f64 * cosine - sample.real as f64 * sine) as f32,
        };
        // The surviving channel predicts the received waveform. Its innovation
        // drives carrier recovery without assuming an undistorted QPSK point.
        let (output, phase_error, error_power) = self.sequence.process(corrected, reliable);
        let phase_error = phase_error.clamp(-0.5, 0.5) as f64;
        self.frequency = (self.frequency + 0.003 * phase_error).clamp(-0.65, 0.65);
        self.phase += self.frequency + 0.2 * phase_error;
        self.phase -= (self.phase / (2.0 * PI)).round_ties_even() * (2.0 * PI);
        self.error = 0.98 * self.error + 0.02 * error_power.sqrt() as f64;
        self.locked = reliable && self.error < 0.4;
        self.alternation = (self.alternation + 1) % 8;
        output
    }
}

struct SymbolChain {
    signal: SignalView,
    clock: ClockRecovery,
    equalizer: Equalizer,
    carrier: Carrier,
    alternate: Option<Carrier>,
    agc: [Complex32; 64],
    head: usize,
    fill: usize,
    reference_peak: f32,
    profile: i32,
    symbols: i64,
    locked_symbols: i64,
}

impl SymbolChain {
    fn new(profile: i32) -> Self {
        Self {
            signal: SignalView::default(),
            clock: ClockRecovery::new(),
            equalizer: Equalizer::default(),
            carrier: Carrier::new(Tracking::Lms),
            alternate: None,
            agc: [Complex32::default(); 64],
            head: 0,
            fill: 0,
            reference_peak: 0.0,
            profile,
            symbols: 0,
            locked_symbols: 0,
        }
    }

    fn process(&mut self, value: Complex32) -> Option<[(u8, [u8; 2]); 2]> {
        self.agc[self.head] = value;
        self.head = (self.head + 1) % self.agc.len();
        if self.fill < self.agc.len() {
            self.fill += 1;
            if self.fill < self.agc.len() {
                return None;
            }
        }
        let mut peak = 0.0_f32;
        for sample in &self.agc {
            let real = sample.real.abs();
            let imaginary = sample.imaginary.abs();
            peak = peak.max(real.max(imaginary) + 0.4 * real.min(imaginary));
        }
        let oldest = self.agc[self.head];
        // A roughly one-second decaying peak prevents a short deep gap from
        // training the equalizers on fully amplified receiver noise. Symbol
        // cadence and the existing clock/carrier estimates continue through it.
        self.reference_peak = peak.max(self.reference_peak * 0.99995);
        let reliable = peak > 1e-12 && peak >= self.reference_peak * 0.08;
        let gain = 1.0 / peak.max(self.reference_peak * 0.08).max(1e-12);
        let (matched, derivatives) = self.clock.process(Complex32 {
            real: oldest.real * gain,
            imaginary: oldest.imaginary * gain,
        })?;
        let (symbol, timing_error) = self.equalizer.process(&matched, &derivatives, reliable);
        self.clock.update(timing_error);
        let primary = self.carrier.process(symbol, reliable);
        // Equal traceback depths make both detectors emit on the same symbol.
        let dibit = match self.alternate.as_mut() {
            Some(alternate) => primary.zip(alternate.process(symbol, reliable)),
            None => primary.map(|p| (p, p)),
        }
        .map(|(a, b)| [a, b]);
        // Observe only after all waveform, timing, carrier and survivor-channel
        // corrections. Keep the received scatter rather than plotting dibits.
        self.signal.push(self.carrier.sequence.equalized());
        let streams = dibit?;
        self.symbols += 1;
        if self.carrier.locked {
            self.locked_symbols += 1;
        }
        Some(streams.map(|(dibit, soft)| match self.profile {
            // Conjugated labels: MSB' = LSB, LSB' = MSB xor LSB.
            1 => (
                [0, 3, 1, 2][dibit as usize],
                [soft[1], soft[0].min(soft[1])],
            ),
            _ => (dibit, soft),
        }))
    }
}

pub struct Demodulator {
    resampler: Resampler,
    symbols: SymbolChain,
    oscillator_real: f64,
    oscillator_imaginary: f64,
    step_real: f64,
    step_imaginary: f64,
    renormalize: usize,
    pub failed: bool,
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub symbols: i64,
    pub locked_symbols: i64,
    pub carrier_error: f64,
    pub frequency_offset_hz: f64,
    pub carrier_locked: i32,
    pub reserved: i32,
}

impl Demodulator {
    pub fn new(rate: f64, offset: f64, profile: i32) -> Self {
        let angle = -2.0 * PI * offset / rate;
        Self {
            resampler: Resampler::new(rate),
            symbols: SymbolChain::new(profile),
            oscillator_real: 1.0,
            oscillator_imaginary: 0.0,
            step_real: angle.cos(),
            step_imaginary: angle.sin(),
            renormalize: 0,
            failed: false,
        }
    }

    /// Also run the alternate (drift RLS) detector; see [`Self::process_dual`].
    pub fn enable_alternate(&mut self) {
        if self.symbols.alternate.is_none() {
            self.symbols.alternate = Some(Carrier::new(Tracking::Drift));
        }
    }

    pub fn process(&mut self, input: &[Complex32], output: &mut [u8]) -> usize {
        self.process_soft(input, output, None)
    }

    /// As [`Self::process`], also writing two bit reliabilities per dibit
    /// (MSB first) when `soft` is given; it needs the same capacity in pairs.
    pub fn process_soft(
        &mut self,
        input: &[Complex32],
        output: &mut [u8],
        mut soft: Option<&mut [[u8; 2]]>,
    ) -> usize {
        self.run(input, |written, streams| {
            output[written] = streams[0].0;
            if let Some(soft) = soft.as_deref_mut() {
                soft[written] = streams[0].1;
            }
        })
    }

    /// Primary and alternate decision streams with reliabilities, index for
    /// index. Without [`Self::enable_alternate`] both are the primary stream.
    pub fn process_dual(
        &mut self,
        input: &[Complex32],
        output: [&mut [u8]; 2],
        soft: [&mut [[u8; 2]]; 2],
    ) -> usize {
        let [first, second] = output;
        let [first_soft, second_soft] = soft;
        self.run(input, |written, streams| {
            first[written] = streams[0].0;
            first_soft[written] = streams[0].1;
            second[written] = streams[1].0;
            second_soft[written] = streams[1].1;
        })
    }

    fn run(
        &mut self,
        input: &[Complex32],
        mut sink: impl FnMut(usize, [(u8, [u8; 2]); 2]),
    ) -> usize {
        let mut written = 0;
        for sample in input {
            let real = sample.real as f64 * self.oscillator_real
                - sample.imaginary as f64 * self.oscillator_imaginary;
            let imaginary = sample.real as f64 * self.oscillator_imaginary
                + sample.imaginary as f64 * self.oscillator_real;
            let next = self.oscillator_real * self.step_real
                - self.oscillator_imaginary * self.step_imaginary;
            self.oscillator_imaginary = self.oscillator_real * self.step_imaginary
                + self.oscillator_imaginary * self.step_real;
            self.oscillator_real = next;
            self.renormalize += 1;
            if self.renormalize == 4096 {
                let magnitude = (self.oscillator_real * self.oscillator_real
                    + self.oscillator_imaginary * self.oscillator_imaginary)
                    .sqrt();
                self.oscillator_real /= magnitude;
                self.oscillator_imaginary /= magnitude;
                self.renormalize = 0;
            }
            let sample = Complex32 {
                real: real as f32,
                imaginary: (if self.symbols.profile == 1 {
                    -imaginary
                } else {
                    imaginary
                }) as f32,
            };
            let symbols = &mut self.symbols;
            self.resampler.process(sample, &mut |value| {
                if let Some(streams) = symbols.process(value) {
                    sink(written, streams);
                    written += 1;
                }
            });
        }
        written
    }

    pub fn read_signal(&self, output: &mut [Complex32]) -> (usize, f64) {
        self.symbols.signal.read(output)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            symbols: self.symbols.symbols,
            locked_symbols: self.symbols.locked_symbols,
            carrier_error: self.symbols.carrier.error,
            frequency_offset_hz: self.symbols.carrier.frequency * 4800.0 / (2.0 * PI),
            carrier_locked: i32::from(self.symbols.carrier.locked),
            reserved: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constellation_tracks_filtered_carrier_and_isi_corrected_iq() {
        // Synthetic pi/4-DQPSK at four samples/symbol: pulse shaping, a delayed
        // reflection, a carrier offset and deterministic noise. No recordings.
        let taps = root_raised_cosine(81, 4.0, 4.0);
        let mut shaped = vec![Complex32::default(); 6000 * 4 + taps.len()];
        let mut rng = 0x8123_4567_u32;
        for i in 0..6000 {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let angle = (rng >> 30) as f64 * PI / 2.0 + PI / 4.0 + i as f64 * PI / 4.0;
            for (j, &tap) in taps.iter().enumerate() {
                shaped[i * 4 + j].real += angle.cos() as f32 * tap;
                shaped[i * 4 + j].imaginary += angle.sin() as f32 * tap;
            }
        }
        let input: Vec<_> = shaped
            .iter()
            .enumerate()
            .map(|(i, &direct)| {
                let echo = shaped[i.saturating_sub(4)];
                let real = direct.real + 0.45 * echo.real;
                let imaginary = direct.imaginary + 0.45 * echo.imaginary;
                let phase = 2.0 * PI * 90.0 * i as f64 / 19200.0;
                // Two independent, repeatable bounded noise components.
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                Complex32 {
                    real: real * phase.cos() as f32 - imaginary * phase.sin() as f32
                        + 0.03 * ((rng & 0xffff) as f32 / 32768.0 - 1.0),
                    imaginary: real * phase.sin() as f32
                        + imaginary * phase.cos() as f32
                        + 0.03 * ((rng >> 16) as f32 / 32768.0 - 1.0),
                }
            })
            .collect();
        let mut receiver = Demodulator::new(19200.0, 0.0, 0);
        let mut output = [0; 1024];
        let mut points = [Complex32::default(); 256];
        for chunk in input.chunks(509) {
            receiver.process(chunk, &mut output);
        }
        let (count, evm) = receiver.read_signal(&mut points);
        assert_eq!(count, points.len());
        assert!(receiver.snapshot().carrier_locked != 0);
        assert!(evm > 1.0 && evm < 20.0, "post-correction EVM: {evm}%");
        // Hard-decided corners or per-point amplitude normalization would hide
        // the scatter that operators use to assess reception quality.
        assert!(points
            .iter()
            .any(|p| (p.real * p.real + p.imaginary * p.imaginary - 1.0).abs() > 0.02));
    }
}

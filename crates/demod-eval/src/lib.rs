//! Demodulator evaluation with synthetic signals and channels. No recordings.
//! A development tool: it is not part of the receiver or its releases.
//!
//! Random voice frames are modulated as pi/4-DQPSK (4,800 symbols/s, root
//! raised cosine, roll-off 0.2), passed through a tapped-delay-line channel
//! with optional Rayleigh/Rician fading, carrier offset and white Gaussian
//! noise, then received by the production DSP, synchronizer and voice decoder.
//! Results count symbol errors before FEC and voice frames accepted after it.
use jr_conventional_dsp::receiver::Dsp;
use receiver_core::{
    data::{decode_frame, decode_frame_soft},
    frame::{Frame, Merger, Synchronizer},
    synthesis, voice_frame, Complex32,
};
use serde::Serialize;
use std::collections::HashMap;
use std::f64::consts::PI;

pub const SYMBOL_RATE: f64 = 4800.0;
const ROLL_OFF: f64 = 0.2;
const SPAN_SYMBOLS: usize = 16;
const SINUSOIDS: usize = 32;
/// Leading frames that only train acquisition; excluded from every count.
const WARMUP_FRAMES: usize = 3;
const FRAME_SYMBOLS: usize = 192;

#[derive(Clone, Debug, Serialize)]
pub struct Path {
    pub delay_us: f64,
    pub power_db: f64,
    /// Carrier offset of this path relative to the others. Simulcast base
    /// stations transmit the same channel with slightly different carriers,
    /// so their signals beat at the difference frequency.
    #[serde(default)]
    pub frequency_hz: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Receiver {
    /// The production DSP (timing, carrier and adaptive sequence detection).
    #[default]
    Dsp,
    /// Reference only: matched filter at the known symbol timing of the first
    /// path, then conventional differential detection. It needs no tracking,
    /// so it bounds what a non-coherent receiver achieves on the same channel.
    Differential,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Traffic {
    /// Voice frames (convolutional code, CRC-8, Reed–Solomon tail).
    #[default]
    Voice,
    /// Data frames (Reed–Solomon (42, 30) and CRC-16).
    Data,
}

#[derive(Clone, Debug, Serialize)]
pub struct Condition {
    pub receiver: Receiver,
    pub traffic: Traffic,
    /// Use the demodulator's bit reliabilities in the frame decoders.
    pub soft: bool,
    /// Also decode the alternate (drift RLS) detector's frames when the
    /// primary frame fails, as the receiver pipeline does.
    pub alternate: bool,
    /// Average received symbol energy over noise density. `None` is noiseless.
    pub es_n0_db: Option<f64>,
    /// Maximum Doppler shift. Zero keeps every path static.
    pub doppler_hz: f64,
    pub paths: Vec<Path>,
    /// Rician K factor of the first path in dB; `None` is Rayleigh.
    pub rician_k_db: Option<f64>,
    pub frequency_offset_hz: f64,
    pub sample_rate: f64,
    pub frames: usize,
    pub seed: u64,
}

impl Default for Condition {
    fn default() -> Self {
        Self {
            receiver: Receiver::Dsp,
            traffic: Traffic::Voice,
            soft: true,
            alternate: true,
            es_n0_db: None,
            doppler_hz: 0.0,
            paths: vec![Path {
                delay_us: 0.0,
                power_db: 0.0,
                frequency_hz: 0.0,
            }],
            rician_k_db: None,
            frequency_offset_hz: 0.0,
            sample_rate: 48000.0,
            frames: 200,
            seed: 1,
        }
    }
}

impl Condition {
    pub fn validate(&self) -> Result<(), String> {
        let sps = self.sample_rate / SYMBOL_RATE;
        if !(9600.0..=1_000_000.0).contains(&self.sample_rate) || sps.fract() != 0.0 {
            return Err(
                "sample rate must be a multiple of 4,800 S/s from 9,600 to 1,000,000".into(),
            );
        }
        if self.paths.is_empty() || self.paths.len() > 8 {
            return Err("use 1–8 channel paths".into());
        }
        if self.paths.iter().any(|p| {
            !p.delay_us.is_finite()
                || !(0.0..=2000.0).contains(&p.delay_us)
                || !p.power_db.is_finite()
                || !p.frequency_hz.is_finite()
                || p.frequency_hz.abs() > 500.0
        }) {
            return Err(
                "path delays must be 0–2000 µs with finite power and offsets within ±500 Hz".into(),
            );
        }
        if !self.doppler_hz.is_finite() || !(0.0..=500.0).contains(&self.doppler_hz) {
            return Err("Doppler must be 0–500 Hz".into());
        }
        if !self.frequency_offset_hz.is_finite() || self.frequency_offset_hz.abs() > 2000.0 {
            return Err("frequency offset must be within ±2,000 Hz".into());
        }
        if self.es_n0_db.is_some_and(|v| !v.is_finite())
            || self.rician_k_db.is_some_and(|v| !v.is_finite())
        {
            return Err("Es/N0 and K factor must be finite".into());
        }
        if !(WARMUP_FRAMES + 2..=100_000).contains(&self.frames) {
            return Err(format!("frames must be {}–100,000", WARMUP_FRAMES + 2));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Report {
    pub frames_sent: u64,
    /// Synchronizer frames whose markers matched.
    pub frames_synchronized: u64,
    pub voice_accepted: u64,
    /// Accepted frames whose FEC-protected bits match no transmitted frame:
    /// errors the convolutional code and CRC failed to detect.
    pub voice_false: u64,
    /// Errors in the 80 payload bits the radio format leaves unprotected,
    /// counted over correctly accepted frames.
    pub unprotected_bit_errors: u64,
    pub unprotected_bits: u64,
    /// Data frames whose 28 information bytes were recovered exactly.
    pub data_accepted: u64,
    /// Data frames accepted with information never transmitted.
    pub data_false: u64,
    pub symbols_compared: u64,
    pub symbol_errors: u64,
    pub bit_errors: u64,
    /// Received symbols that could not be aligned with the transmission.
    pub symbols_unaligned: u64,
    /// Symbols the receiver inserted or dropped (clock slips), from changes in
    /// the transmit/receive alignment between frame-sized blocks.
    pub clock_slips: u64,
    pub corrected_bits: u64,
    pub carrier_locked_symbols: u64,
    pub demodulated_symbols: u64,
    /// Accepted frames that only the alternate detector decoded.
    pub alternate_accepted: u64,
    /// Compared symbols and their errors by instantaneous channel power
    /// relative to its average, at each symbol's centre. Shows whether errors
    /// concentrate in fades (noise-limited) or spread out (tracking-limited).
    pub fade_bins: Vec<FadeBin>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct FadeBin {
    /// Upper edge in dB; `None` is the open top bin.
    pub below_db: Option<f64>,
    pub symbols: u64,
    pub errors: u64,
}
const FADE_EDGES_DB: [f64; 4] = [-20.0, -10.0, -5.0, 0.0];
fn fade_bins() -> Vec<FadeBin> {
    FADE_EDGES_DB
        .iter()
        .map(|&e| Some(e))
        .chain([None])
        .map(|below_db| FadeBin {
            below_db,
            ..FadeBin::default()
        })
        .collect()
}

impl Report {
    pub fn add(&mut self, other: &Report) {
        self.frames_sent += other.frames_sent;
        self.frames_synchronized += other.frames_synchronized;
        self.voice_accepted += other.voice_accepted;
        self.voice_false += other.voice_false;
        self.unprotected_bit_errors += other.unprotected_bit_errors;
        self.unprotected_bits += other.unprotected_bits;
        self.data_accepted += other.data_accepted;
        self.data_false += other.data_false;
        self.symbols_compared += other.symbols_compared;
        self.symbol_errors += other.symbol_errors;
        self.bit_errors += other.bit_errors;
        self.symbols_unaligned += other.symbols_unaligned;
        self.clock_slips += other.clock_slips;
        self.corrected_bits += other.corrected_bits;
        self.carrier_locked_symbols += other.carrier_locked_symbols;
        self.demodulated_symbols += other.demodulated_symbols;
        self.alternate_accepted += other.alternate_accepted;
        if self.fade_bins.is_empty() {
            self.fade_bins = fade_bins();
        }
        for (mine, theirs) in self.fade_bins.iter_mut().zip(&other.fade_bins) {
            mine.symbols += theirs.symbols;
            mine.errors += theirs.errors;
        }
    }
    pub fn voice_acceptance(&self) -> f64 {
        ratio(self.voice_accepted, self.frames_sent)
    }
    pub fn data_acceptance(&self) -> f64 {
        ratio(self.data_accepted, self.frames_sent)
    }
    pub fn symbol_error_rate(&self) -> f64 {
        ratio(self.symbol_errors, self.symbols_compared)
    }
    pub fn bit_error_rate(&self) -> f64 {
        ratio(self.bit_errors, 2 * self.symbols_compared)
    }
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        f64::NAN
    } else {
        a as f64 / b as f64
    }
}

/// xorshift64* with Box–Muller; deterministic per seed and platform.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Circular complex Gaussian with unit total variance.
    pub fn complex_gaussian(&mut self) -> (f64, f64) {
        let radius = (-(1.0 - self.uniform()).ln()).sqrt();
        let angle = 2.0 * PI * self.uniform();
        (radius * angle.cos(), radius * angle.sin())
    }
}

fn root_raised_cosine(sps: usize) -> Vec<f64> {
    let count = SPAN_SYMBOLS * sps + 1;
    let mut taps: Vec<f64> = (0..count)
        .map(|i| {
            let t = (i as f64 - (count - 1) as f64 / 2.0) / sps as f64;
            if t == 0.0 {
                1.0 - ROLL_OFF + 4.0 * ROLL_OFF / PI
            } else if ((4.0 * ROLL_OFF * t).abs() - 1.0).abs() < 1e-9 {
                ROLL_OFF / 2f64.sqrt()
                    * ((1.0 + 2.0 / PI) * (PI / (4.0 * ROLL_OFF)).sin()
                        + (1.0 - 2.0 / PI) * (PI / (4.0 * ROLL_OFF)).cos())
            } else {
                ((PI * t * (1.0 - ROLL_OFF)).sin()
                    + 4.0 * ROLL_OFF * t * (PI * t * (1.0 + ROLL_OFF)).cos())
                    / (PI * t * (1.0 - 16.0 * ROLL_OFF * ROLL_OFF * t * t))
            }
        })
        .collect();
    // Unit energy per symbol at the output sample rate.
    let energy: f64 = taps.iter().map(|t| t * t).sum();
    let scale = (sps as f64 / energy).sqrt();
    for t in &mut taps {
        *t *= scale;
    }
    taps
}

/// Dibits as the receiver emits them, MSB first within each envelope byte.
pub fn envelope_dibits(envelope: &[u8; 48]) -> impl Iterator<Item = u8> + '_ {
    envelope
        .iter()
        .flat_map(|b| [6, 4, 2, 0].map(move |shift| (b >> shift) & 3))
}

/// pi/4-DQPSK baseband at `sps` samples per symbol with unit average power.
/// The phase increment for each dibit is the inverse of the receiver's
/// differential label map, so a clean loopback reproduces the dibits.
pub fn modulate(dibits: &[u8], sps: usize) -> Vec<Complex32> {
    const INCREMENT: [u8; 4] = [0, 1, 3, 2];
    let taps = root_raised_cosine(sps);
    let mut output = vec![(0.0f64, 0.0f64); dibits.len() * sps + taps.len()];
    let mut quadrant = 0u8;
    for (i, &dibit) in dibits.iter().enumerate() {
        quadrant = (quadrant + INCREMENT[dibit as usize]) % 4;
        let angle = quadrant as f64 * PI / 2.0 + PI / 4.0 + i as f64 * PI / 4.0;
        let (sin, cos) = angle.sin_cos();
        for (j, &tap) in taps.iter().enumerate() {
            let o = &mut output[i * sps + j];
            o.0 += cos * tap;
            o.1 += sin * tap;
        }
    }
    output
        .into_iter()
        .map(|(re, im)| Complex32 {
            real: re as f32,
            imaginary: im as f32,
        })
        .collect()
}

struct FadingPath {
    gain: f64,
    delay: usize,
    // Per-path carrier offset in rad/sample (simulcast beat).
    rotation: f64,
    // Static complex gain, or sum-of-sinusoid parameters when fading.
    fixed: (f64, f64),
    scatter: Vec<(f64, f64)>,     // (Doppler rad/sample, phase)
    los: Option<(f64, f64, f64)>, // (amplitude, Doppler rad/sample, phase)
    scatter_amplitude: f64,
}

impl FadingPath {
    fn at(&self, n: usize) -> (f64, f64) {
        if self.scatter.is_empty() && self.los.is_none() {
            return self.rotate(n, (self.gain * self.fixed.0, self.gain * self.fixed.1));
        }
        let (mut re, mut im) = (0.0, 0.0);
        for &(w, phase) in &self.scatter {
            let (s, c) = (w * n as f64 + phase).sin_cos();
            re += c;
            im += s;
        }
        re *= self.scatter_amplitude;
        im *= self.scatter_amplitude;
        if let Some((a, w, phase)) = self.los {
            let (s, c) = (w * n as f64 + phase).sin_cos();
            re += a * c;
            im += a * s;
        }
        self.rotate(n, (self.gain * re, self.gain * im))
    }
    fn rotate(&self, n: usize, (re, im): (f64, f64)) -> (f64, f64) {
        if self.rotation == 0.0 {
            return (re, im);
        }
        let (s, c) = (self.rotation * n as f64).sin_cos();
        (re * c - im * s, re * s + im * c)
    }
}

/// Tapped delay line with Clarke-model sum-of-sinusoid fading per path,
/// normalized to unit average power, followed by a carrier offset and AWGN.
pub fn channel(signal: &[Complex32], condition: &Condition, rng: &mut Rng) -> Vec<Complex32> {
    channel_with_power(signal, condition, rng).0
}

/// [`channel`] plus, for each transmitted sample, the total power of the
/// path gains it experiences (unit on average).
pub fn channel_with_power(
    signal: &[Complex32],
    condition: &Condition,
    rng: &mut Rng,
) -> (Vec<Complex32>, Vec<f64>) {
    let fs = condition.sample_rate;
    let total: f64 = condition
        .paths
        .iter()
        .map(|p| 10f64.powf(p.power_db / 10.0))
        .sum();
    let doppler = 2.0 * PI * condition.doppler_hz / fs;
    let paths: Vec<FadingPath> = condition
        .paths
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let gain = (10f64.powf(p.power_db / 10.0) / total).sqrt();
            let delay = (p.delay_us * 1e-6 * fs).round() as usize;
            let fixed_phase = if index == 0 {
                0.0
            } else {
                2.0 * PI * rng.uniform()
            };
            let fading = condition.doppler_hz > 0.0;
            let k = match (index, condition.rician_k_db) {
                (0, Some(k)) => 10f64.powf(k / 10.0),
                _ => 0.0,
            };
            let scatter = if fading {
                (0..SINUSOIDS)
                    .map(|_| {
                        let arrival = 2.0 * PI * rng.uniform();
                        (doppler * arrival.cos(), 2.0 * PI * rng.uniform())
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let los = (fading && k > 0.0).then(|| {
                let arrival = 2.0 * PI * rng.uniform();
                (
                    (k / (k + 1.0)).sqrt(),
                    doppler * arrival.cos(),
                    2.0 * PI * rng.uniform(),
                )
            });
            FadingPath {
                gain,
                delay,
                rotation: 2.0 * PI * p.frequency_hz / fs,
                fixed: (fixed_phase.cos(), fixed_phase.sin()),
                scatter,
                los,
                scatter_amplitude: (1.0 / ((k + 1.0) * SINUSOIDS as f64)).sqrt(),
            }
        })
        .collect();
    let max_delay = paths.iter().map(|p| p.delay).max().unwrap_or(0);
    let length = signal.len() + max_delay;
    let mut output = vec![(0.0f64, 0.0f64); length];
    let mut power = vec![0.0; signal.len()];
    for path in &paths {
        for (i, s) in signal.iter().enumerate() {
            let n = i + path.delay;
            let (hr, hi) = path.at(n);
            power[i] += hr * hr + hi * hi;
            let (sr, si) = (s.real as f64, s.imaginary as f64);
            output[n].0 += hr * sr - hi * si;
            output[n].1 += hr * si + hi * sr;
        }
    }
    let rotation = 2.0 * PI * condition.frequency_offset_hz / fs;
    let sps = fs / SYMBOL_RATE;
    // Unit transmit power and unit average channel power: Es = sps per symbol.
    let noise = condition
        .es_n0_db
        .map(|db| (sps / 10f64.powf(db / 10.0)).sqrt());
    let output = output
        .into_iter()
        .enumerate()
        .map(|(n, (re, im))| {
            let (s, c) = (rotation * n as f64).sin_cos();
            let (mut re, mut im) = (re * c - im * s, re * s + im * c);
            if let Some(sigma) = noise {
                let (nr, ni) = rng.complex_gaussian();
                re += sigma * nr;
                im += sigma * ni;
            }
            Complex32 {
                real: re as f32,
                imaginary: im as f32,
            }
        })
        .collect();
    (output, power)
}

/// Genie-timed matched filter and differential detection (see [`Receiver`]).
fn differential(received: &[Complex32], sps: usize, symbols: usize) -> Vec<u8> {
    const LABEL: [u8; 4] = [0, 1, 3, 2];
    let taps = root_raised_cosine(sps);
    let delay = taps.len() - 1;
    let mut previous: Option<(f64, f64)> = None;
    let mut output = Vec::with_capacity(symbols);
    for i in 0..symbols {
        let n = i * sps + delay;
        if n >= received.len() {
            break;
        }
        let (mut re, mut im) = (0.0, 0.0);
        for (j, &h) in taps.iter().enumerate() {
            let s = received[n - j];
            re += s.real as f64 * h;
            im += s.imaginary as f64 * h;
        }
        if let Some((pr, pi)) = previous {
            let (dr, di) = (re * pr + im * pi, im * pr - re * pi);
            let angle = di.atan2(dr) - PI / 4.0;
            let k = (angle / (PI / 2.0)).round().rem_euclid(4.0) as usize;
            output.push(LABEL[k]);
        } else {
            output.push(0);
        }
        previous = Some((re, im));
    }
    output
}

/// Run one condition end to end and count errors after the warm-up frames.
pub fn run(condition: &Condition) -> Result<Report, String> {
    condition.validate()?;
    let mut rng = Rng::new(condition.seed);
    let sps = (condition.sample_rate / SYMBOL_RATE) as usize;
    let mut payloads = Vec::with_capacity(condition.frames);
    let mut infos: HashMap<[u8; 28], usize> = HashMap::new();
    let mut dibits = Vec::with_capacity(condition.frames * FRAME_SYMBOLS);
    for i in 0..condition.frames {
        let envelope = match condition.traffic {
            Traffic::Voice => {
                let mut payload = [0u8; 20];
                for chunk in payload.chunks_mut(8) {
                    let value = rng.next_u64().to_le_bytes();
                    chunk.copy_from_slice(&value[..chunk.len()]);
                }
                payload[19] &= 0x3f;
                payloads.push(payload);
                synthesis::voice(&payload, i % 2 == 1)
            }
            Traffic::Data => {
                let mut info = [0u8; 28];
                for chunk in info.chunks_mut(8) {
                    let value = rng.next_u64().to_le_bytes();
                    chunk.copy_from_slice(&value[..chunk.len()]);
                }
                infos.insert(info, i);
                synthesis::data(&info)
            }
        };
        dibits.extend(envelope_dibits(&envelope));
    }
    // Trailing idle symbols flush the receive filters and traceback.
    dibits.extend(std::iter::repeat_n(0, 2 * FRAME_SYMBOLS));
    let (received, power) = channel_with_power(&modulate(&dibits, sps), condition, &mut rng);
    // Symbol i is centred on transmit sample i * sps + (filter length - 1) / 2.
    let centre = SPAN_SYMBOLS * sps / 2;
    let symbol_power: Vec<f64> = (0..dibits.len())
        .map(|i| power.get(i * sps + centre).copied().unwrap_or(1.0))
        .collect();

    let mut alternate: Option<(Vec<u8>, Vec<[u8; 2]>)> = None;
    let (demodulated, soft, snapshot) = match condition.receiver {
        Receiver::Dsp => {
            let mut dsp = Dsp::new(condition.sample_rate, 0.0)?;
            if condition.alternate {
                dsp.enable_alternate();
            }
            let mut output = [Vec::new(), Vec::new()];
            let mut reliability = [Vec::new(), Vec::new()];
            let mut streams = [Vec::new(), Vec::new()];
            let mut softs = [Vec::new(), Vec::new()];
            let mut snapshot = None;
            for block in received.chunks(4096) {
                snapshot = Some(dsp.process_dual(block, &mut output, &mut reliability)?);
                for k in 0..2 {
                    streams[k].extend_from_slice(&output[k]);
                    softs[k].extend_from_slice(&reliability[k]);
                }
            }
            let [primary, second] = streams;
            let [primary_soft, second_soft] = softs;
            if condition.alternate {
                alternate = Some((second, second_soft));
            }
            (primary, primary_soft, snapshot.unwrap_or_default())
        }
        Receiver::Differential => {
            let demodulated = differential(&received, sps, dibits.len());
            let soft = vec![[u8::MAX; 2]; demodulated.len()];
            (demodulated, soft, Default::default())
        }
    };

    let counted = WARMUP_FRAMES..condition.frames;
    let mut report = Report {
        frames_sent: counted.len() as u64,
        carrier_locked_symbols: snapshot.locked_symbols as u64,
        demodulated_symbols: snapshot.symbols as u64,
        fade_bins: fade_bins(),
        ..Report::default()
    };
    let index: HashMap<[u8; 10], usize> = payloads
        .iter()
        .enumerate()
        .map(|(i, p)| (protected_bits(p), i))
        .collect();
    let mut sync = Synchronizer::default();
    let mut frames: Vec<Frame> = Vec::new();
    let use_soft = condition.soft && condition.receiver == Receiver::Dsp;
    for (&d, &q) in demodulated.iter().zip(&soft) {
        if use_soft {
            sync.push_soft(d, q, &mut frames);
        } else {
            sync.push(d, &mut frames);
        }
    }
    let decode_data = |frame: &Frame| {
        if use_soft {
            decode_frame_soft(&frame.received, &frame.confidence)
        } else {
            decode_frame(&frame.received)
        }
    };
    let decode_voice = |frame: &Frame| {
        if use_soft {
            voice_frame::decode_soft(&frame.received, &frame.confidence)
        } else {
            voice_frame::decode(&frame.received)
        }
    };
    // Merge both detectors' frames exactly as the receiver pipeline does.
    let merged: Vec<(Frame, bool)> = match alternate {
        Some((stream, reliability)) => {
            let mut alternate_sync = Synchronizer::default();
            let mut alternate_frames = Vec::new();
            for (&d, &q) in stream.iter().zip(&reliability) {
                if use_soft {
                    alternate_sync.push_soft(d, q, &mut alternate_frames);
                } else {
                    alternate_sync.push(d, &mut alternate_frames);
                }
            }
            let mut merger = Merger::default();
            for frame in frames.drain(..) {
                merger.push(0, frame);
            }
            for frame in alternate_frames {
                merger.push(1, frame);
            }
            let traffic = condition.traffic;
            merger.release([None, None], true, |f| match traffic {
                Traffic::Data => decode_data(f).is_some(),
                Traffic::Voice => decode_voice(f).is_some(),
            })
        }
        None => frames.drain(..).map(|f| (f, false)).collect(),
    };
    let mut accepted = vec![false; condition.frames];
    for (frame, via_alternate) in &merged {
        let (frame, via_alternate) = (frame, *via_alternate);
        report.frames_synchronized += u64::from(frame.sync_matched);
        if condition.traffic == Traffic::Data {
            if let Some((info, _)) = decode_data(frame) {
                match infos.get(&info) {
                    Some(&i) if counted.contains(&i) && !accepted[i] => {
                        accepted[i] = true;
                        report.data_accepted += 1;
                        report.alternate_accepted += u64::from(via_alternate);
                    }
                    Some(_) => {}
                    None => report.data_false += 1,
                }
            }
            continue;
        }
        if let Some(speech) = decode_voice(frame) {
            match index.get(&protected_bits(&speech.payload)) {
                Some(&i) if counted.contains(&i) && !accepted[i] => {
                    accepted[i] = true;
                    report.voice_accepted += 1;
                    report.alternate_accepted += u64::from(via_alternate);
                    report.corrected_bits += speech.corrected_bits as u64;
                    report.unprotected_bits += UNPROTECTED_BITS;
                    report.unprotected_bit_errors += speech
                        .payload
                        .iter()
                        .zip(&payloads[i])
                        .map(|(a, b)| u64::from((a ^ b).count_ones()))
                        .sum::<u64>();
                }
                Some(_) => {}
                None => report.voice_false += 1,
            }
        }
    }
    // Synchronized frames before the counted range are not part of the score.
    let warmup_synced = merged
        .iter()
        .map(|(f, _)| f)
        .take_while(|f| (f.bit_offset as usize) < 2 * WARMUP_FRAMES * FRAME_SYMBOLS)
        .filter(|f| f.sync_matched)
        .count() as u64;
    report.frames_synchronized = report.frames_synchronized.saturating_sub(warmup_synced);
    count_symbol_errors(
        &dibits[..condition.frames * FRAME_SYMBOLS],
        &demodulated,
        &symbol_power,
        &mut report,
    );
    Ok(report)
}

/// Payload bits 0..78 are carried by the convolutional code and CRC; the
/// remaining 80 travel raw. Accepted frames are identified by the former.
const UNPROTECTED_BITS: u64 = 80;
fn protected_bits(payload: &[u8; 20]) -> [u8; 10] {
    let mut key = [0; 10];
    key.copy_from_slice(&payload[..10]);
    key[9] &= 0x3f;
    key
}

/// Align the demodulated stream with the transmission in frame-sized blocks.
/// The receiver delay is constant except for clock slips, so each block
/// searches a few symbols around the previous offset.
fn count_symbol_errors(sent: &[u8], received: &[u8], power: &[f64], report: &mut Report) {
    let start = WARMUP_FRAMES * FRAME_SYMBOLS;
    let errors_at = |offset: isize, from: usize, to: usize| -> Option<(u64, u64)> {
        let mut symbols = 0;
        let mut bits = 0;
        for (i, &s) in sent.iter().enumerate().take(to).skip(from) {
            let r = *received.get(usize::try_from(i as isize + offset).ok()?)?;
            if r != s {
                symbols += 1;
                bits += u64::from((r ^ s).count_ones());
            }
        }
        Some((symbols, bits))
    };
    // Initial delay: best match over the first counted frame. Start-up drops
    // symbols while the filters fill, so the offset may be negative.
    let mut offset = (-512..512isize)
        .filter_map(|o| errors_at(o, start, start + FRAME_SYMBOLS).map(|e| (e.0, o)))
        .min()
        .map(|(_, o)| o)
        .unwrap_or(0);
    let mut from = start;
    while from < sent.len() {
        let to = (from + FRAME_SYMBOLS).min(sent.len());
        let best = (-3..=3)
            .filter_map(|d| errors_at(offset + d, from, to).map(|e| (e, d)))
            .min_by_key(|((symbols, _), d)| (*symbols, d.abs()));
        match best {
            // Above 60% the block carries no alignment information.
            Some(((symbols, bits), d)) if symbols * 10 <= (to - from) as u64 * 6 => {
                offset += d;
                report.clock_slips += d.unsigned_abs() as u64;
                report.symbols_compared += (to - from) as u64;
                report.symbol_errors += symbols;
                report.bit_errors += bits;
                for i in from..to {
                    let db = 10.0 * power[i].max(1e-12).log10();
                    let bin = FADE_EDGES_DB
                        .iter()
                        .position(|&edge| db < edge)
                        .unwrap_or(FADE_EDGES_DB.len());
                    let wrong = received
                        .get((i as isize + offset) as usize)
                        .is_some_and(|&r| r != sent[i]);
                    report.fade_bins[bin].symbols += 1;
                    report.fade_bins[bin].errors += u64::from(wrong);
                }
            }
            _ => report.symbols_unaligned += (to - from) as u64,
        }
        from = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn condition(es_n0_db: Option<f64>) -> Condition {
        Condition {
            es_n0_db,
            frames: 24,
            ..Condition::default()
        }
    }

    #[test]
    fn clean_loopback_reproduces_every_dibit_and_frame() {
        let report = run(&condition(None)).unwrap();
        assert_eq!(report.frames_sent, 21);
        assert_eq!(report.voice_accepted, 21, "{report:?}");
        assert_eq!(report.voice_false, 0);
        assert_eq!(report.symbol_errors, 0, "{report:?}");
        assert_eq!(report.symbols_unaligned, 0);
        assert!(report.symbols_compared >= 21 * FRAME_SYMBOLS as u64);
    }

    #[test]
    fn noise_degrades_symbols_before_frames_and_results_are_repeatable() {
        let good = run(&condition(Some(20.0))).unwrap();
        let poor = run(&condition(Some(6.0))).unwrap();
        assert!(
            good.symbol_error_rate() < poor.symbol_error_rate(),
            "{good:?} {poor:?}"
        );
        assert!(good.voice_acceptance() >= poor.voice_acceptance());
        assert_eq!(poor, run(&condition(Some(6.0))).unwrap());
    }

    #[test]
    fn differential_reference_decodes_a_clean_loopback() {
        let report = run(&Condition {
            receiver: Receiver::Differential,
            ..condition(None)
        })
        .unwrap();
        assert_eq!(report.voice_accepted, 21, "{report:?}");
        assert_eq!(report.symbol_errors, 0);
    }

    #[test]
    fn fading_channel_has_unit_average_power() {
        let condition = Condition {
            doppler_hz: 40.0,
            paths: vec![
                Path {
                    delay_us: 0.0,
                    power_db: 0.0,
                    frequency_hz: 0.0,
                },
                Path {
                    delay_us: 50.0,
                    power_db: -3.0,
                    frequency_hz: 0.0,
                },
            ],
            ..Condition::default()
        };
        let tone = vec![
            Complex32 {
                real: 1.0,
                imaginary: 0.0
            };
            192_000
        ];
        let mut total = 0.0;
        let seeds = 8;
        for seed in 0..seeds {
            let faded = channel(&tone, &condition, &mut Rng::new(seed));
            total += faded[2400..]
                .iter()
                .map(|s| (s.real * s.real + s.imaginary * s.imaginary) as f64)
                .sum::<f64>()
                / (faded.len() - 2400) as f64;
        }
        let mean = total / seeds as f64;
        assert!((mean - 1.0).abs() < 0.25, "mean power {mean}");
    }

    fn acceptance(condition: Condition) -> Report {
        let mut total = Report::default();
        for seed in 1..=4 {
            total.add(
                &run(&Condition {
                    seed,
                    ..condition.clone()
                })
                .unwrap(),
            );
        }
        total
    }

    #[test]
    fn soft_decisions_and_survivor_steps_keep_their_gains() {
        // Guards jr_conventional_dsp's per-tap channel steps and the soft
        // decoding path. Measured: AWGN 7 dB hard 54 % / soft 79 %; 40 Hz
        // Rayleigh at 30 dB hard 81 % / soft 96 % (63 % with one detector and
        // a single LMS step for every tap). Bounds sit between old and new.
        let awgn = Condition {
            es_n0_db: Some(7.0),
            frames: 80,
            ..Condition::default()
        };
        let soft = acceptance(awgn.clone());
        let hard = acceptance(Condition {
            soft: false,
            ..awgn
        });
        assert!(soft.voice_acceptance() > 0.62, "{soft:?}");
        assert!(soft.voice_acceptance() > hard.voice_acceptance() + 0.15);
        assert_eq!(soft.voice_false, 0);
        // 40 Hz is about 123 km/h at 350 MHz.
        let fading = acceptance(Condition {
            es_n0_db: Some(30.0),
            doppler_hz: 40.0,
            frames: 80,
            ..Condition::default()
        });
        assert!(fading.voice_acceptance() > 0.90, "{fading:?}");
    }

    #[test]
    fn half_symbol_echoes_slip_less_and_keep_frame_lock() {
        // Two paths 104 µs (half a symbol) apart fading at 2 Hz make the clock
        // slip as they trade dominance. The wider timing loop with the old
        // synchronizer accepted 89 %; the narrower loop and slip-following
        // synchronizer reach 96 %.
        let echo = acceptance(Condition {
            es_n0_db: Some(30.0),
            doppler_hz: 2.0,
            paths: vec![
                Path {
                    delay_us: 0.0,
                    power_db: 0.0,
                    frequency_hz: 0.0,
                },
                Path {
                    delay_us: 104.0,
                    power_db: -3.0,
                    frequency_hz: 0.0,
                },
            ],
            frames: 80,
            ..Condition::default()
        });
        assert!(echo.voice_acceptance() > 0.92, "{echo:?}");
        assert!(echo.clock_slips < 9, "{echo:?}");
    }

    #[test]
    fn simulcast_with_a_one_symbol_transmit_offset_survives_carrier_beat() {
        // Two base stations on one channel, the second delayed by one symbol
        // (transmit time diversity) and 20 Hz off in carrier. Equal power
        // beats to silence without the delay; with it the trellis resolves
        // both copies.
        let simulcast = acceptance(Condition {
            es_n0_db: Some(22.0),
            paths: vec![
                Path {
                    delay_us: 0.0,
                    power_db: 0.0,
                    frequency_hz: 0.0,
                },
                Path {
                    delay_us: 208.0,
                    power_db: 0.0,
                    frequency_hz: 20.0,
                },
            ],
            frames: 40,
            ..Condition::default()
        });
        assert!(simulcast.voice_acceptance() > 0.97, "{simulcast:?}");
        assert_eq!(simulcast.clock_slips, 0, "{simulcast:?}");
    }

    #[test]
    fn simulcast_during_train_motion_needs_the_drift_tracking_detector() {
        // Two stations one symbol apart and 5 Hz off in carrier, each path
        // Rayleigh fading at 40 Hz (about 123 km/h). The LMS detector alone
        // accepts about 37 %; merging the drift RLS detector's frames reaches
        // about 92 %.
        let condition = Condition {
            es_n0_db: Some(30.0),
            doppler_hz: 40.0,
            paths: vec![
                Path {
                    delay_us: 0.0,
                    power_db: 0.0,
                    frequency_hz: 0.0,
                },
                Path {
                    delay_us: 208.0,
                    power_db: 0.0,
                    frequency_hz: 5.0,
                },
            ],
            frames: 80,
            ..Condition::default()
        };
        let both = acceptance(condition.clone());
        let single = acceptance(Condition {
            alternate: false,
            ..condition
        });
        assert!(both.voice_acceptance() > 0.85, "{both:?}");
        assert!(
            both.voice_acceptance() > single.voice_acceptance() + 0.3,
            "{single:?}"
        );
        assert!(both.alternate_accepted > 0);
        // Undetected voice errors stay rare (one 40 ms frame here): trying a
        // second candidate per position adds chances, the CRCs still gate them.
        assert!(both.voice_false * 100 <= both.voice_accepted, "{both:?}");
    }

    #[test]
    fn data_frames_round_trip_and_soft_retries_never_lose_frames() {
        let clean = acceptance(Condition {
            traffic: Traffic::Data,
            frames: 24,
            ..Condition::default()
        });
        assert_eq!(clean.data_accepted, 4 * 21, "{clean:?}");
        let faded = Condition {
            traffic: Traffic::Data,
            es_n0_db: Some(22.0),
            doppler_hz: 40.0,
            frames: 80,
            ..Condition::default()
        };
        let soft = acceptance(faded.clone());
        let hard = acceptance(Condition {
            soft: false,
            ..faded
        });
        assert!(
            soft.data_accepted >= hard.data_accepted,
            "{soft:?} {hard:?}"
        );
        assert_eq!(soft.data_false, 0);
    }

    #[test]
    fn invalid_conditions_are_rejected() {
        for c in [
            Condition {
                sample_rate: 50000.0,
                ..Condition::default()
            },
            Condition {
                paths: vec![],
                ..Condition::default()
            },
            Condition {
                doppler_hz: -1.0,
                ..Condition::default()
            },
            Condition {
                frames: 2,
                ..Condition::default()
            },
            Condition {
                es_n0_db: Some(f64::NAN),
                ..Condition::default()
            },
        ] {
            assert!(run(&c).is_err());
        }
    }
}

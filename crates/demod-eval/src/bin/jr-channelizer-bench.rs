//! Channelizer benchmark (development tool): the per-channel front end (each
//! channel mixes and filters the full-rate IQ) against the shared polyphase
//! channelizer (jr_conventional_dsp::channelizer), on
//!
//! - CPU: 22 channels of noise at 240 kS/s and 1.024 MS/s;
//! - reception: a wideband capture with a voice channel between two
//!   neighbours 10 dB stronger, through static and fading paths.
use jr_conventional_dsp::{channelizer::Channelizer, receiver::Dsp, Complex32};
use jr_demod_eval::{channel, envelope_dibits, modulate, protected_bits, Condition, Rng};
use receiver_core::{
    data::Format,
    pipeline::{ChannelConfig, Pipeline},
    synthesis,
};
use std::{collections::HashMap, f64::consts::PI, time::Instant};

const CENTRE: f64 = 352_603_125.0;

fn offsets() -> Vec<f64> {
    // The 22 JR East channels: 6.25 kHz apart, the centre between two.
    (-11..11).map(|k| (k as f64 + 0.5) * 6250.0).collect()
}

fn cpu(rate: f64, count: usize) {
    if std::env::args().any(|a| a == "--reception-only") {
        return;
    }
    let seconds = 3.0;
    let n = (rate * seconds) as usize;
    let mut rng = Rng::new(7);
    let iq: Vec<Complex32> = (0..n)
        .map(|_| {
            let (a, b) = rng.complex_gaussian();
            Complex32 {
                real: (a * 0.05) as f32,
                imaginary: (b * 0.05) as f32,
            }
        })
        .collect();
    let block = (rate * 0.04) as usize;
    let offsets: Vec<f64> = offsets().into_iter().take(count).collect();
    let mut bits = [Vec::new(), Vec::new()];
    let mut soft = [Vec::new(), Vec::new()];
    // Per-channel front end.
    let mut dsps: Vec<Dsp> = offsets
        .iter()
        .map(|&f| Dsp::new(rate, f).unwrap())
        .collect();
    let t = Instant::now();
    for chunk in iq.chunks(block) {
        for dsp in &mut dsps {
            dsp.process_dual(chunk, &mut bits, &mut soft).unwrap();
        }
    }
    let direct = t.elapsed().as_secs_f64() / seconds;
    let mut row = format!(
        "| {:.3} MS/s | {count} | {:.1} % |",
        rate / 1e6,
        100.0 * direct
    );
    let mut alone_row = String::new();
    for mode in ["bins=direct", "bins=fft"] {
        jr_conventional_dsp::tuning::parse(mode).unwrap();
        // Shared channelizer, alone and with the demodulators.
        let mut c = Channelizer::new(rate).unwrap();
        let placements: Vec<_> = offsets.iter().map(|&f| c.place(f)).collect();
        let mut out = Vec::new();
        let t = Instant::now();
        for chunk in iq.chunks(block) {
            out.clear();
            c.process(chunk, &mut out);
        }
        let alone = t.elapsed().as_secs_f64() / seconds;
        let mut dsps: Vec<Dsp> = placements
            .iter()
            .map(|p| Dsp::new(p.rate, p.residual_hz).unwrap())
            .collect();
        let t = Instant::now();
        for chunk in iq.chunks(block) {
            out.clear();
            c.process(chunk, &mut out);
            for (dsp, p) in dsps.iter_mut().zip(&placements) {
                let samples = &out.iter().find(|(k, _)| *k == p.bin).unwrap().1;
                dsp.process_dual(samples, &mut bits, &mut soft).unwrap();
            }
        }
        let shared = t.elapsed().as_secs_f64() / seconds;
        row += &format!(" {:.1} % |", 100.0 * shared);
        alone_row += &format!(" {:.2} % |", 100.0 * alone);
    }
    jr_conventional_dsp::tuning::parse("bins=fft").unwrap();
    println!("{row}{alone_row}");
}

fn shifted(signal: &[Complex32], hz: f64, rate: f64, gain: f64) -> Vec<Complex32> {
    signal
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (sin, cos) = (2.0 * PI * hz * i as f64 / rate).sin_cos();
            Complex32 {
                real: ((s.real as f64 * cos - s.imaginary as f64 * sin) * gain) as f32,
                imaginary: ((s.real as f64 * sin + s.imaginary as f64 * cos) * gain) as f32,
            }
        })
        .collect()
}

fn reception(name: &str, sps: usize, doppler: f64, es_n0_db: f64, seeds: u64) {
    let rate = 4800.0 * sps as f64;
    let target = 3125.0 + 4.0 * 6250.0;
    let (mut direct, mut shared, mut sent) = (0u64, 0u64, 0u64);
    for seed in 1..=seeds {
        let mut rng = Rng::new(seed);
        let frames = 150;
        let mut payloads = Vec::new();
        let mut dibits = Vec::new();
        for i in 0..frames {
            let mut payload = [0u8; 20];
            for b in &mut payload {
                *b = rng.next_u64() as u8;
            }
            payload[19] &= 0x3f;
            payloads.push(payload);
            dibits.extend(envelope_dibits(&synthesis::voice(&payload, i % 2 == 1)));
        }
        dibits.extend(std::iter::repeat_n(0, 384));
        let condition = Condition {
            sample_rate: rate,
            doppler_hz: doppler,
            seed,
            ..Condition::default()
        };
        let wanted = channel(&modulate(&dibits, sps), &condition, &mut rng);
        let noise = (sps as f64 / 10f64.powf(es_n0_db / 10.0)).sqrt();
        let mut iq = shifted(&wanted, target, rate, 1.0);
        for neighbour in [-6250.0, 6250.0] {
            let other: Vec<u8> = (0..dibits.len())
                .map(|_| (rng.next_u64() & 3) as u8)
                .collect();
            let other = modulate(&other, sps);
            for (s, o) in
                iq.iter_mut()
                    .zip(shifted(&other, target + neighbour, rate, 10f64.powf(0.5)))
            {
                s.real += o.real;
                s.imaginary += o.imaginary;
            }
        }
        for s in &mut iq {
            let (a, b) = rng.complex_gaussian();
            s.real = ((s.real as f64 + noise * a) * 0.05) as f32;
            s.imaginary = ((s.imaginary as f64 + noise * b) * 0.05) as f32;
        }
        let index: HashMap<[u8; 10], usize> = payloads
            .iter()
            .enumerate()
            .map(|(i, p)| (protected_bits(p), i))
            .collect();
        let count = |events: &[[u8; 20]]| {
            let mut seen = vec![false; frames];
            for p in events {
                if let Some(&i) = index.get(&protected_bits(p)) {
                    if i >= 3 {
                        seen[i] = true;
                    }
                }
            }
            seen.iter().filter(|s| **s).count() as u64
        };
        let config = |rate_centre: f64| ChannelConfig {
            id: "t".into(),
            label: "t".into(),
            frequency_hz: rate_centre,
            format: Format::Slotted,
        };
        // Per-channel front end at the full rate.
        let mut p = Pipeline::new(config(CENTRE + target), rate, CENTRE).unwrap();
        let mut got = Vec::new();
        for chunk in iq.chunks((rate * 0.04) as usize) {
            p.process_observed(chunk, |e| got.extend(e.speech)).unwrap();
        }
        p.flush_observed(|e| got.extend(e.speech)).unwrap();
        direct += count(&got);
        // Channelizer, then the same pipeline at the bin's rate.
        let mut c = Channelizer::new(rate).unwrap();
        let place = c.place(target);
        let mut p = Pipeline::new(
            config(CENTRE + target),
            place.rate,
            CENTRE + target - place.residual_hz,
        )
        .unwrap();
        let mut got = Vec::new();
        let mut out = Vec::new();
        for chunk in iq.chunks((rate * 0.04) as usize) {
            out.clear();
            c.process(chunk, &mut out);
            p.process_observed(&out[0].1, |e| got.extend(e.speech))
                .unwrap();
        }
        p.flush_observed(|e| got.extend(e.speech)).unwrap();
        shared += count(&got);
        sent += frames as u64 - 3;
    }
    println!(
        "| {name} | {:.3} MS/s | {:.2} % | {:.2} % |",
        rate / 1e6,
        100.0 * direct as f64 / sent as f64,
        100.0 * shared as f64 / sent as f64
    );
}

fn main() {
    jr_conventional_dsp::tuning::parse("alternate=off").unwrap();
    println!("CPU (% of one core; second detector off)\n");
    println!("| input | channels | per-channel | channelizer, dot products | channelizer, FFT | channelizer alone, dot products | alone, FFT |\n|---|---:|---:|---:|---:|---:|---:|");
    for rate in [240_000.0, 1_024_000.0, 2_400_000.0] {
        for count in [1, 2, 4, 8, 22] {
            cpu(rate, count);
        }
    }
    jr_conventional_dsp::tuning::parse("alternate=gated").unwrap();
    println!("\nVoice frames received, neighbours 10 dB stronger\n");
    println!("| condition | input | per-channel | channelizer |\n|---|---|---:|---:|");
    // `--seeds N` (default 6).
    let args: Vec<String> = std::env::args().collect();
    let seeds = args
        .iter()
        .position(|a| a == "--seeds")
        .and_then(|i| args.get(i + 1)?.parse().ok())
        .unwrap_or(6);
    let rates: &[usize] = if args.iter().any(|a| a == "--240k") {
        &[50]
    } else {
        &[50, 211]
    };
    for &sps in rates {
        reception("static 7 dB", sps, 0.0, 7.0, seeds);
        reception("static 10 dB", sps, 0.0, 10.0, seeds);
        reception("Rayleigh 10 Hz 16 dB", sps, 10.0, 16.0, seeds);
        reception("Rayleigh 33 Hz 16 dB", sps, 33.0, 16.0, seeds);
    }
}

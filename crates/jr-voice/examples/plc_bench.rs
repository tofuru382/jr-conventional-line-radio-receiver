//! Packet loss concealment benchmark (development tool). No speech
//! recordings exist here, so payload sequences stand in for speech: each
//! frame flips a few bits of the previous one (consecutive speech frames
//! share most parameters), or frames are independent (the worst case).
//! Frames are lost independently or in pairs, reported as missing slots, and
//! the decoded audio is compared with decoding every frame:
//!
//! - lost frames: log-spectral distance to the true frame (dB; silence
//!   scores as far as the floor allows), lower is better;
//! - the 7 frames after each loss (the decoder's lookahead): segmental SNR
//!   to the true audio (dB), higher is better.
//!
//!   cargo run --release -p jr_conventional_voice --example plc_bench
use jr_conventional_voice::{plc, receiver::Voice};
use std::collections::HashMap;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
}

fn payloads(n: usize, flip: f64, rng: &mut Rng) -> Vec<[u8; 20]> {
    let mut current = [0u8; 20];
    for b in &mut current {
        *b = rng.next() as u8;
    }
    current[19] &= 0x3f;
    (0..n)
        .map(|_| {
            if flip >= 1.0 {
                for b in &mut current {
                    *b = rng.next() as u8;
                }
            } else {
                for bit in 0..158 {
                    if rng.chance(flip) {
                        current[bit / 8] ^= 1 << (bit % 8);
                    }
                }
            }
            current[19] &= 0x3f;
            current
        })
        .collect()
}

fn decode(events: &[(u32, Option<[u8; 20]>)]) -> HashMap<u32, [i16; 320]> {
    let mut voice = Voice::default();
    let mut out = HashMap::new();
    for &(index, payload) in events {
        voice.push(index, payload).unwrap();
        while let Some(frame) = voice.read() {
            out.insert(frame.index, frame.pcm);
        }
    }
    voice.flush().unwrap();
    while let Some(frame) = voice.read() {
        out.insert(frame.index, frame.pcm);
    }
    out
}

fn log_spectrum(pcm: &[i16; 320]) -> Vec<f64> {
    // 80 bands of a Hann-windowed DFT, 25 Hz to 4 kHz.
    (1..=80)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, &x) in pcm.iter().enumerate() {
                let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / 319.0).cos();
                let phase = 2.0 * std::f64::consts::PI * (2 * k) as f64 * n as f64 / 320.0;
                re += w * x as f64 * phase.cos();
                im -= w * x as f64 * phase.sin();
            }
            // Floor about 60 dB below a loud frame.
            20.0 * (re.hypot(im) + 100.0).log10()
        })
        .collect()
}

fn lsd(a: &[i16; 320], b: &[i16; 320]) -> f64 {
    let (x, y) = (log_spectrum(a), log_spectrum(b));
    (x.iter().zip(&y).map(|(p, q)| (p - q).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn snr(reference: &[i16; 320], test: &[i16; 320]) -> Option<f64> {
    let signal: f64 = reference.iter().map(|&x| (x as f64).powi(2)).sum();
    if signal < 320.0 * 100.0 {
        return None;
    }
    let noise: f64 = reference
        .iter()
        .zip(test)
        .map(|(&r, &t)| (r as f64 - t as f64).powi(2))
        .sum();
    Some((10.0 * (signal / noise.max(1e-9)).log10()).clamp(-10.0, 40.0))
}

fn main() {
    let silence = [0i16; 320];
    let seeds = 3u64;
    let n = 300;
    println!("| sequence | loss | concealed slots | lost: LSD dB | after: segSNR dB |");
    println!("|---|---|---:|---:|---:|");
    for (name, flip) in [
        ("smooth (2 % bits change)", 0.02),
        ("independent frames", 1.0),
    ] {
        // (loss, variant) -> sums
        let mut sums: HashMap<(usize, u32), (f64, usize, f64, usize)> = HashMap::new();
        let losses = [
            ("5 % single", 0.05, false),
            ("10 % single", 0.10, false),
            ("5 % in pairs", 0.05, true),
        ];
        for seed in 1..=seeds {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x1234_5678_9abc_def1));
            let sent = payloads(n, flip, &mut rng);
            let all: Vec<_> = sent
                .iter()
                .enumerate()
                .map(|(i, p)| (i as u32, Some(*p)))
                .collect();
            let reference = decode(&all);
            for (l, &(_, p, pairs)) in losses.iter().enumerate() {
                let mut dropped = vec![false; n];
                let mut i = 8;
                while i < n - 8 {
                    if rng.chance(if pairs { p / 2.0 } else { p }) {
                        dropped[i] = true;
                        if pairs {
                            dropped[i + 1] = true;
                            i += 1;
                        }
                    }
                    i += 1;
                }
                let events: Vec<_> = all
                    .iter()
                    .map(|&(i, p)| (i, if dropped[i as usize] { None } else { p }))
                    .collect();
                for frames in [0, 1, 2, 3] {
                    plc::set(frames);
                    let test = decode(&events);
                    let entry = sums.entry((l, frames)).or_default();
                    for i in 0..n {
                        let Some(r) = reference.get(&(i as u32)) else {
                            continue;
                        };
                        let t = test.get(&(i as u32)).unwrap_or(&silence);
                        if dropped[i] {
                            entry.0 += lsd(r, t);
                            entry.1 += 1;
                        } else if (1..=7).any(|k| i >= k && dropped[i - k]) {
                            if let Some(v) = snr(r, t) {
                                entry.2 += v;
                                entry.3 += 1;
                            }
                        }
                    }
                }
            }
        }
        for (l, (loss, _, _)) in losses.iter().enumerate() {
            for frames in [0, 1, 2, 3] {
                let e = sums[&(l, frames)];
                println!(
                    "| {name} | {loss} | {frames} | {:.2} | {:.2} |",
                    e.0 / e.1.max(1) as f64,
                    e.2 / e.3.max(1) as f64
                );
            }
        }
    }
}

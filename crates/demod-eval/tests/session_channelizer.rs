//! The whole session (wave input, channelizer, channel workers) receives a
//! channel between two stronger neighbours as well with the shared
//! channelizer as with a front end per channel.
use jr_conventional_dsp::Complex32;
use jr_demod_eval::{channel, envelope_dibits, modulate, Condition, Rng};
use receiver_core::{
    data::Format,
    pipeline::ChannelConfig,
    session::{Config, Input, Session},
    synthesis, tuning,
};
use std::{
    f64::consts::PI,
    fs,
    io::Write,
    time::{Duration, Instant},
};

const RATE: f64 = 240_000.0;
const CENTRE: f64 = 352_603_125.0;
const FRAMES: usize = 100;

fn write_wave(path: &std::path::Path, iq: &[Complex32]) {
    let mut f = fs::File::create(path).unwrap();
    let size = iq.len() as u32 * 8;
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + size).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    for b in [
        16u32.to_le_bytes(),
        [3, 0, 2, 0],
        (RATE as u32).to_le_bytes(),
        (RATE as u32 * 8).to_le_bytes(),
        [8, 0, 32, 0],
    ] {
        f.write_all(&b).unwrap();
    }
    f.write_all(b"data").unwrap();
    f.write_all(&size.to_le_bytes()).unwrap();
    let mut bytes = Vec::with_capacity(size as usize);
    for s in iq {
        bytes.extend(s.real.to_le_bytes());
        bytes.extend(s.imaginary.to_le_bytes());
    }
    f.write_all(&bytes).unwrap();
}

fn add(iq: &mut [Complex32], signal: &[Complex32], hz: f64, gain: f64) {
    for (i, (s, x)) in iq.iter_mut().zip(signal).enumerate() {
        let (sin, cos) = (2.0 * PI * hz * i as f64 / RATE).sin_cos();
        s.real += ((x.real as f64 * cos - x.imaginary as f64 * sin) * gain) as f32;
        s.imaginary += ((x.real as f64 * sin + x.imaginary as f64 * cos) * gain) as f32;
    }
}

fn received(path: &std::path::Path, channels: &[ChannelConfig]) -> Vec<u64> {
    let session = Session::start(Config {
        input: Input::Wave {
            path: path.to_string_lossy().into(),
            center_hz: CENTRE,
            fast: true,
        },
        channels: channels.to_vec(),
    })
    .unwrap();
    let start = Instant::now();
    loop {
        let (status, _) = session.snapshot(None);
        if !status.running {
            assert!(status.error.is_none(), "{:?}", status.error);
            return status
                .channels
                .iter()
                .map(|c| c.status.voice_frames)
                .collect();
        }
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_channelizer_receives_as_well_as_a_front_end_per_channel() {
    let sps = (RATE / 4800.0) as usize;
    let mut rng = Rng::new(3);
    let mut dibits = Vec::new();
    for i in 0..FRAMES {
        let mut payload = [0u8; 20];
        for b in &mut payload {
            *b = rng.next_u64() as u8;
        }
        payload[19] &= 0x3f;
        dibits.extend(envelope_dibits(&synthesis::voice(&payload, i % 2 == 1)));
    }
    dibits.extend(std::iter::repeat_n(0, 384));
    let condition = Condition {
        sample_rate: RATE,
        seed: 3,
        ..Condition::default()
    };
    let wanted = channel(&modulate(&dibits, sps), &condition, &mut rng);
    // The wanted channel, 28.125 kHz above the centre, between neighbours
    // 10 dB stronger; one more channel near the band edge.
    let target = 28_125.0;
    let mut iq = vec![Complex32::default(); wanted.len()];
    add(&mut iq, &wanted, target, 1.0);
    add(&mut iq, &wanted, -65_625.0, 1.0);
    for neighbour in [-6250.0, 6250.0] {
        let other: Vec<u8> = (0..dibits.len())
            .map(|_| (rng.next_u64() & 3) as u8)
            .collect();
        add(
            &mut iq,
            &modulate(&other, sps),
            target + neighbour,
            10f64.powf(0.5),
        );
    }
    let noise = (sps as f64 / 10f64.powf(1.2)).sqrt();
    for s in &mut iq {
        let (a, b) = rng.complex_gaussian();
        s.real = ((s.real as f64 + noise * a) * 0.05) as f32;
        s.imaginary = ((s.imaginary as f64 + noise * b) * 0.05) as f32;
    }
    let path = std::env::temp_dir().join(format!("jr-channelizer-{}.wav", std::process::id()));
    write_wave(&path, &iq);
    let channels: Vec<ChannelConfig> = [target, -65_625.0]
        .iter()
        .enumerate()
        .map(|(i, f)| ChannelConfig {
            id: format!("c{i}"),
            label: format!("c{i}"),
            frequency_hz: CENTRE + f,
            format: Format::Slotted,
        })
        .collect();
    tuning::set(tuning::ALL & !tuning::CHANNELIZER);
    let direct = received(&path, &channels);
    tuning::set(tuning::ALL);
    let shared = received(&path, &channels);
    fs::remove_file(&path).ok();
    eprintln!("voice frames: per channel {direct:?}, channelizer {shared:?}");
    for (d, s) in direct.iter().zip(&shared) {
        assert!(
            *s as usize >= FRAMES - 6,
            "channelizer {shared:?}, per channel {direct:?}"
        );
        assert!(
            s + 2 >= *d,
            "channelizer {shared:?}, per channel {direct:?}"
        );
    }
}

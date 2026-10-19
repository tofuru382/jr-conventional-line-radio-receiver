use receiver_core::{
    data::{Decoder, Format},
    frame::Frame,
    pipeline::{self, Audio, ChannelConfig, Pipeline},
    session::{Config, Input, Session},
    source,
    wave::{IqWave, PcmWave},
    Complex32,
};
use std::{
    error::Error,
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const HELP: &str = concat!(
    "JR Cconventional-line Radio Receiver (JR 在来線デジタル列車無線受信アプリ) v",
    env!("CARGO_PKG_VERSION"),
    "\n\n  jr-receiver devices\n  jr-receiver presets [PRESET|--list]\n  jr-receiver monitor-tcp IP:PORT SECONDS [RATE] [CENTER_HZ] [PRESET] [GAIN_DB]\n  jr-receiver inspect-wave INPUT.wav\n  jr-receiver decode-frames STREAM_DIR NEW_OUTPUT_DIR [common|slotted]\n  jr-receiver decode-wave INPUT.wav CENTER_HZ NEW_OUTPUT_DIR [PRESET] [SECONDS]\n\nSTREAM_DIR contains frames.bin (48 bytes/frame) and indices.txt (uint32 slot indices).\nWAV input: stereo float32 or PCM16, I left / Q right. All channels in the selected preset decode together. Default preset: east (22 channels).\nOutput directories must not exist. INCOMPLETE remains after failure.\n"
);
fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        None | Some("--help" | "-h") => print!("{HELP}"),
        Some("devices") if a.len() == 1 => {
            println!("{}", serde_json::to_string_pretty(&source::enumerate()?)?)
        }
        Some("presets") if a.get(1).is_some_and(|s| s == "--list") && a.len() == 2 => println!(
            "{}",
            serde_json::to_string_pretty(&receiver_core::presets::catalog()?)?
        ),
        Some("presets") if a.len() <= 2 => println!(
            "{}",
            serde_json::to_string_pretty(
                &receiver_core::presets::resolve(region(a.get(1))?)?.channels
            )?
        ),
        Some("inspect-wave") if a.len() == 2 => {
            let w = IqWave::open(&a[1])?;
            println!(
                "{}",
                serde_json::json!({"sample_rate":w.sample_rate,"samples":w.samples,"seconds":w.samples as f64/w.sample_rate})
            );
        }
        Some("monitor-tcp") if (3..=7).contains(&a.len()) => {
            let address: std::net::SocketAddr = a[1].parse()?;
            let seconds: u64 = a[2].parse()?;
            if !(1..=3600).contains(&seconds) {
                return Err("SECONDS must be 1–3600".into());
            }
            let preset = receiver_core::presets::resolve(region(a.get(5))?)?;
            let radio = receiver_core::rtl_tcp::Config {
                host: address.ip().to_string(),
                port: address.port(),
                sample_rate: a
                    .get(3)
                    .map(|s| s.parse())
                    .transpose()?
                    .unwrap_or(preset.sample_rate as u32),
                center_hz: a
                    .get(4)
                    .map(|s| s.parse())
                    .transpose()?
                    .unwrap_or(preset.center_hz as u32),
                gain_db: a.get(6).map(|s| s.parse()).transpose()?,
                ppm: 0,
                reconnect_seconds: 60,
            };
            let mut session = Session::start(Config {
                input: Input::RtlTcp { radio },
                channels: preset.channels,
            })?;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(seconds) {
                if !session.snapshot(None).0.running {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            session.stop();
            let (status, _) = session.snapshot(None);
            println!("{}", serde_json::to_string_pretty(&status)?);
            if let Some(error) = status.error {
                return Err(error.into());
            }
            if status
                .channels
                .iter()
                .any(|c| c.status.iq_samples == 0 || c.error.is_some())
            {
                return Err(
                    "Some channels received no IQ or failed; inspect the JSON report".into(),
                );
            }
        }
        Some("decode-frames") if (3..=4).contains(&a.len()) => decode_frames(
            &a[1],
            &a[2],
            match a.get(3).map(String::as_str).unwrap_or("slotted") {
                "common" => Format::Common,
                "slotted" => Format::Slotted,
                _ => return Err("Expected common or slotted".into()),
            },
        )?,
        Some("decode-wave") if (4..=6).contains(&a.len()) => {
            let seconds = a.get(5).map(|s| s.parse::<f64>()).transpose()?;
            if seconds.is_some_and(|s| !s.is_finite() || s <= 0.0) {
                return Err("SECONDS must be positive".into());
            }
            decode_wave(&a[1], a[2].parse()?, &a[3], region(a.get(4))?, seconds)?;
        }
        _ => return Err(HELP.into()),
    }
    Ok(())
}
fn region(value: Option<&String>) -> Result<&str> {
    let id = value.map(String::as_str).unwrap_or("east");
    receiver_core::presets::resolve(id)?;
    Ok(id)
}
struct Output {
    wave: PcmWave,
    data: File,
    decoder: Decoder,
    last: Option<(u64, u32)>,
}
impl Output {
    fn new(path: &Path, format: Format) -> Result<Self> {
        fs::create_dir(path)?;
        Ok(Self {
            wave: PcmWave::create(path.join("audio.wav"))?,
            data: File::create(path.join("data.jsonl"))?,
            decoder: Decoder::new(format),
            last: None,
        })
    }
    fn audio(&mut self, a: Audio) -> Result<()> {
        if let Some((epoch, last)) = self.last {
            if a.epoch == epoch {
                let gap = a
                    .index
                    .checked_sub(last)
                    .and_then(|n| n.checked_sub(1))
                    .ok_or("Non-monotonic PCM index")?;
                self.wave.silence(gap as u64 * 320)?;
            }
        }
        self.wave.write(&a.pcm)?;
        self.last = Some((a.epoch, a.index));
        Ok(())
    }
    fn frame(&mut self, f: &Frame) -> Result<()> {
        for m in self.decoder.decode(f) {
            serde_json::to_writer(&mut self.data, &m)?;
            self.data.write_all(b"\n")?;
        }
        Ok(())
    }
    fn finish(self) -> Result<()> {
        self.wave.finish()?;
        self.data.sync_all()?;
        Ok(())
    }
}
fn begin(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    fs::write(
        path.join("INCOMPLETE"),
        "Decoding in progress; outputs may be incomplete.\n",
    )?;
    Ok(())
}
fn decode_frames(input: &str, out: &str, format: Format) -> Result<()> {
    let path = Path::new(input);
    let frames = fs::read(path.join("frames.bin"))?;
    let indices: Vec<u32> = BufReader::new(File::open(path.join("indices.txt"))?)
        .lines()
        .map(|s| s?.parse().map_err(std::io::Error::other))
        .collect::<std::io::Result<_>>()?;
    if frames.len() % 48 != 0
        || frames.len() / 48 != indices.len()
        || indices.windows(2).any(|p| p[1] <= p[0])
    {
        return Err("Frame/slot count mismatch or unordered indices".into());
    }
    let config = ChannelConfig {
        id: "stream".into(),
        label: "Indexed stream".into(),
        frequency_hz: 352625000.0,
        format,
    };
    let mut p = Pipeline::new(config, 19200.0, 352625000.0)?;
    let root = Path::new(out);
    begin(root)?;
    let mut output = Output::new(&root.join("stream"), format)?;
    for (data, index) in frames.as_chunks::<48>().0.iter().zip(indices) {
        let f = Frame {
            received: *data,
            bit_offset: index as u64 * 384,
            sync_matched: false,
            marker_errors: 0,
        };
        output.frame(&f)?;
        for a in p.frame(&f)? {
            output.audio(a)?;
        }
    }
    for a in p.flush()? {
        output.audio(a)?;
    }
    output.finish()?;
    let status = p.snapshot();
    fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&status)?,
    )?;
    fs::remove_file(root.join("INCOMPLETE"))?;
    println!(
        "{} frames, {} accepted voice, {} audio frames",
        status.frames, status.voice_frames, status.audio_frames
    );
    Ok(())
}
fn decode_wave(
    input: &str,
    center: f64,
    out: &str,
    region: &str,
    seconds: Option<f64>,
) -> Result<()> {
    let mut wave = IqWave::open(input)?;
    let configs = receiver_core::presets::resolve(region)?.channels;
    pipeline::validate_channels(&configs, wave.sample_rate, center)?;
    let limit = seconds
        .map(|s| (s * wave.sample_rate) as u64)
        .unwrap_or(wave.samples)
        .min(wave.samples);
    let root = PathBuf::from(out);
    begin(&root)?;
    let mut pipelines = configs
        .iter()
        .map(|c| Pipeline::new(c.clone(), wave.sample_rate, center))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut outputs = configs
        .iter()
        .map(|c| Output::new(&root.join(&c.id), c.format))
        .collect::<Result<Vec<_>>>()?;
    // The CLI is an offline, lossless path; it intentionally applies backpressure.
    let mut iq = vec![Complex32::default(); 16384];
    let mut read = 0;
    let start = Instant::now();
    while read < limit {
        let n = wave.read(&mut iq[..16384.min((limit - read) as usize)])?;
        if n == 0 {
            break;
        }
        read += n as u64;
        for (p, o) in pipelines.iter_mut().zip(outputs.iter_mut()) {
            for a in p.process(&iq[..n])? {
                o.audio(a)?;
            }
            for m in p.take_messages() {
                serde_json::to_writer(&mut o.data, &m)?;
                o.data.write_all(b"\n")?;
            }
        }
    }
    let mut reports = Vec::new();
    for ((mut p, mut o), config) in pipelines.into_iter().zip(outputs).zip(configs) {
        for a in p.flush()? {
            o.audio(a)?;
        }
        let status = p.snapshot();
        // History is explicitly a bounded summary; CLI writes this separately, not as a complete event log.
        fs::write(
            root.join(&config.id).join("messages.json"),
            serde_json::to_vec_pretty(&status.messages)?,
        )?;
        o.finish()?;
        reports.push(serde_json::json!({"channel":config,"status":status}));
    }
    let report = serde_json::json!({"sample_rate":wave.sample_rate,"center_hz":center,"samples":read,"input_seconds":read as f64/wave.sample_rate,"processing_seconds":start.elapsed().as_secs_f64(),"channels":reports});
    fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::remove_file(root.join("INCOMPLETE"))?;
    println!(
        "Decoded {} channels in {:.2} s; {}",
        reports.len(),
        start.elapsed().as_secs_f64(),
        root.display()
    );
    Ok(())
}

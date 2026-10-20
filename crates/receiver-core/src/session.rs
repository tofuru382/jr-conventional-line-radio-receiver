//! One acquisition stream, shared immutable IQ blocks, and independent channel workers.
//! Bounded queues cannot silently concatenate radio data across lost samples.
use crate::{
    monitor::{Activity, Monitor},
    pipeline::{validate_channels, Audio, ChannelConfig, Pipeline, Status},
    recording::{OutputOptions, Recorder, RecordingStatus},
    source::{RadioConfig, Read},
    wave::IqWave,
    Complex32,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Sdr { radio: RadioConfig },
    RtlTcp { radio: crate::rtl_tcp::Config },
    Wave { path: String, center_hz: f64 },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub input: Input,
    pub channels: Vec<ChannelConfig>,
    /// Voice decoder for every channel of this session.
    #[serde(default)]
    pub voice_decoder: crate::pipeline::VoiceDecoder,
}
#[derive(Clone, Serialize)]
pub struct ChannelStatus {
    pub config: ChannelConfig,
    pub status: Status,
    pub dropped_blocks: u64,
    pub audio_drops: u64,
    pub error: Option<String>,
}
struct Channel {
    status: ChannelStatus,
    audio: VecDeque<(u64, Audio)>,
    progress_us: u64,
    monitored: bool,
}
impl Channel {
    /// Hands the monitored channel's PCM to the single audio consumer and
    /// keeps the other queues fresh.
    fn route_audio(&mut self, monitored: bool, audio: &mut Vec<Audio>) {
        // Freshness applies when entering a channel, not to an
        // ongoing stream. TCP can deliver over half a second at once;
        // synthesis lookahead makes the first PCM older than 600 ms.
        // Pruning that PCM on every poll punched holes in live audio
        // while the recording (written before this queue) stayed intact.
        if !monitored || !self.monitored {
            while self
                .audio
                .front()
                .is_some_and(|(time, _)| time.saturating_add(600_000) < self.progress_us)
            {
                self.audio.pop_front();
            }
        }
        self.monitored = monitored;
        if monitored {
            audio.extend(self.audio.drain(..).map(|(_, a)| a));
        }
    }
}
#[derive(Clone, Serialize)]
pub struct Overview {
    pub running: bool,
    pub state: String,
    pub error: Option<String>,
    pub sample_rate: f64,
    pub center_hz: f64,
    pub source_overflows: u64,
    pub progress: f64,
    pub channels: Vec<ChannelStatus>,
    pub recording: RecordingStatus,
    pub selected_channel: Option<String>,
    pub active_calls: Vec<String>,
    /// RTL-TCP lost its server and is reconnecting automatically.
    pub reconnecting: bool,
    /// Failed attempts in the current outage (the server is not answering).
    pub reconnect_failures: u32,
    /// Successful automatic reconnections in this session.
    pub reconnects: u64,
}
struct Shared {
    state: String,
    error: Option<String>,
    rate: f64,
    center: f64,
    overflows: u64,
    progress: f64,
    reconnection: (bool, u32, u64),
}
pub struct Session {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    channels: Vec<Arc<Mutex<Channel>>>,
    thread: Option<JoinHandle<()>>,
    recorder: Arc<Recorder>,
    monitor: Arc<Mutex<Monitor>>,
}
struct Block {
    sequence: u64,
    start_sample: u64,
    iq: Arc<[Complex32]>,
}
enum Source {
    Wave(IqWave),
    RtlTcp(crate::rtl_tcp::Reconnecting),
    #[cfg(feature = "soapy")]
    Sdr(crate::source::Sdr),
    #[cfg(test)]
    Gated {
        source: Box<Source>,
        permits: mpsc::Receiver<()>,
    },
}
impl Source {
    fn samples(&self) -> Option<u64> {
        match self {
            Self::Wave(w) => Some(w.samples),
            #[cfg(test)]
            Self::Gated { source, .. } => source.samples(),
            _ => None,
        }
    }
    fn open(input: &Input, stop: &AtomicBool) -> Result<(Self, f64, f64), String> {
        match input {
            Input::RtlTcp { radio } => {
                let s = crate::rtl_tcp::Reconnecting::open(radio, stop)?;
                Ok((
                    Self::RtlTcp(s),
                    radio.sample_rate as f64,
                    radio.center_hz as f64,
                ))
            }
            Input::Wave { path, center_hz } => {
                let w = IqWave::open(path).map_err(|e| e.to_string())?;
                let rate = w.sample_rate;
                Ok((Self::Wave(w), rate, *center_hz))
            }
            Input::Sdr { radio } => {
                #[cfg(feature = "soapy")]
                {
                    let s = crate::source::Sdr::open(radio)?;
                    let (rate, center) = (s.sample_rate, s.center_hz);
                    Ok((Self::Sdr(s), rate, center))
                }
                #[cfg(not(feature = "soapy"))]
                {
                    let _ = radio;
                    Err("SoapySDR was disabled at build time".into())
                }
            }
        }
    }
    /// RTL-TCP reconnection state: (reconnecting, failed attempts, reconnects).
    fn reconnection(&self) -> (bool, u32, u64) {
        match self {
            Self::RtlTcp(s) => (s.reconnecting(), s.failed_attempts, s.reconnects),
            #[cfg(test)]
            Self::Gated { source, .. } => source.reconnection(),
            _ => (false, 0, 0),
        }
    }
    fn read(&mut self, buf: &mut [Complex32], stop: &AtomicBool) -> Result<Option<Read>, String> {
        match self {
            Self::RtlTcp(s) => s.read(buf, stop).map(Some),
            Self::Wave(w) => {
                let n = w.read(buf).map_err(|e| e.to_string())?;
                Ok(if n == 0 { None } else { Some(Read::Samples(n)) })
            }
            #[cfg(feature = "soapy")]
            Self::Sdr(s) => s.read(buf).map(Some),
            #[cfg(test)]
            Self::Gated { source, permits } => {
                match permits.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) => source.read(buf, stop),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(Some(Read::Timeout)),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Err("Test source gate disconnected".into())
                    }
                }
            }
        }
    }
}
impl Session {
    pub fn start(config: Config) -> Result<Self, String> {
        Self::start_with_source(config, Source::open)
    }
    fn start_with_source(
        config: Config,
        open_source: impl FnOnce(&Input, &AtomicBool) -> Result<(Source, f64, f64), String>
            + Send
            + 'static,
    ) -> Result<Self, String> {
        if config.channels.is_empty() || config.channels.len() > 64 {
            return Err("Select 1–64 channels".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared {
            state: "starting".into(),
            error: None,
            rate: 0.0,
            center: 0.0,
            overflows: 0,
            progress: 0.0,
            reconnection: (false, 0, 0),
        }));
        let channels: Vec<_> = config
            .channels
            .iter()
            .map(|c| {
                Arc::new(Mutex::new(Channel {
                    status: ChannelStatus {
                        config: c.clone(),
                        status: Status {
                            input_dbfs: -120.0,
                            ..Status::default()
                        },
                        dropped_blocks: 0,
                        audio_drops: 0,
                        error: None,
                    },
                    audio: VecDeque::new(),
                    progress_us: 0,
                    monitored: false,
                }))
            })
            .collect();
        let monitor = Arc::new(Mutex::new(Monitor::new(
            config.channels.iter().map(|c| c.id.clone()),
        )));
        let call_monitor = monitor.clone();
        let recorder = Arc::new(Recorder::default());
        let recording = recorder.clone();
        let (cancel, state, workers) = (stop.clone(), shared.clone(), channels.clone());
        let handle = thread::Builder::new()
            .name("iq-acquisition".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(
                        config,
                        &cancel,
                        &state,
                        &workers,
                        &recording,
                        &call_monitor,
                        open_source,
                    )
                }));
                let mut s = state.lock().unwrap();
                // Serialize automatic finalization with starting a recording.
                // Workers have already flushed their last synthesized audio.
                call_monitor.lock().unwrap().finish();
                recording.stop();
                match result {
                    Ok(Ok(())) => {
                        s.state = if cancel.load(Ordering::Relaxed) {
                            "stopped"
                        } else {
                            "completed"
                        }
                        .into()
                    }
                    Ok(Err(_))
                        if cancel.load(Ordering::Relaxed)
                            && workers
                                .iter()
                                .all(|c| c.lock().unwrap().status.error.is_none()) =>
                    {
                        s.state = "stopped".into();
                    }
                    Ok(Err(e)) => {
                        s.state = "failed".into();
                        s.error = Some(e);
                    }
                    Err(_) => {
                        s.state = "failed".into();
                        s.error = Some("Receiver worker panicked; restart reception".into());
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            shared,
            channels,
            thread: Some(handle),
            recorder,
            monitor,
        })
    }
    pub fn snapshot(&self, selected: Option<&str>) -> (Overview, Vec<Audio>) {
        self.snapshot_with_monitor(selected, false)
    }
    pub fn snapshot_with_monitor(
        &self,
        selected: Option<&str>,
        automatic: bool,
    ) -> (Overview, Vec<Audio>) {
        self.snapshot_with_options(selected, automatic, true)
    }
    /// With `take_audio` false the monitor PCM stays queued for another
    /// consumer (the Android native player); only status is returned.
    pub fn snapshot_with_options(
        &self,
        selected: Option<&str>,
        automatic: bool,
        take_audio: bool,
    ) -> (Overview, Vec<Audio>) {
        let s = self.shared.lock().unwrap();
        let (preferred, active_calls) = self.monitor.lock().unwrap().snapshot();
        let selected = if automatic {
            preferred.as_deref().or(selected)
        } else {
            selected
        };
        let mut audio = Vec::new();
        let channels = self
            .channels
            .iter()
            .map(|c| {
                let mut c = c.lock().unwrap();
                let mut status = c.status.clone();
                let monitored = selected == Some(status.config.id.as_str());
                if take_audio {
                    c.route_audio(monitored, &mut audio);
                }
                if !monitored {
                    status.status.messages.clear();
                    status.status.constellation.clear();
                }
                status
            })
            .collect();
        (
            Overview {
                running: matches!(s.state.as_str(), "starting" | "receiving"),
                state: s.state.clone(),
                error: s.error.clone(),
                sample_rate: s.rate,
                center_hz: s.center,
                source_overflows: s.overflows,
                progress: s.progress,
                channels,
                recording: self.recorder.snapshot(),
                selected_channel: selected.map(String::from),
                active_calls,
                reconnecting: s.reconnection.0,
                reconnect_failures: s.reconnection.1,
                reconnects: s.reconnection.2,
            },
            audio,
        )
    }
    /// Audio-only counterpart of [`Self::snapshot_with_monitor`] for a native
    /// player: the same channel choice, without cloning any channel status.
    pub fn take_monitor_audio(&self, selected: Option<&str>, automatic: bool) -> Vec<Audio> {
        let (preferred, _) = self.monitor.lock().unwrap().snapshot();
        let selected = if automatic {
            preferred.as_deref().or(selected)
        } else {
            selected
        };
        let mut audio = Vec::new();
        for c in &self.channels {
            let mut c = c.lock().unwrap();
            let monitored = selected == Some(c.status.config.id.as_str());
            c.route_audio(monitored, &mut audio);
        }
        audio
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
    pub fn start_recording(&self, directory: &std::path::Path) -> Result<RecordingStatus, String> {
        self.start_recording_with_options(directory, OutputOptions::default())
    }
    pub fn start_recording_with_options(
        &self,
        directory: &std::path::Path,
        options: OutputOptions,
    ) -> Result<RecordingStatus, String> {
        let s = self.shared.lock().unwrap();
        if s.state != "receiving" || self.stop.load(Ordering::Relaxed) {
            return Err("受信を開始し、接続が完了してから録音してください。".into());
        }
        let configs: Vec<_> = self
            .channels
            .iter()
            .map(|c| c.lock().unwrap().status.config.clone())
            .collect();
        self.recorder
            .start_with_options(directory, &configs, options)
    }
    pub fn stop_recording(&self) -> RecordingStatus {
        self.recorder.stop()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
fn run(
    config: Config,
    stop: &Arc<AtomicBool>,
    shared: &Arc<Mutex<Shared>>,
    channels: &[Arc<Mutex<Channel>>],
    recorder: &Arc<Recorder>,
    monitor: &Arc<Mutex<Monitor>>,
    open_source: impl FnOnce(&Input, &AtomicBool) -> Result<(Source, f64, f64), String>,
) -> Result<(), String> {
    if let Input::RtlTcp { radio } = &config.input {
        radio.validate()?;
        validate_channels(
            &config.channels,
            radio.sample_rate as f64,
            radio.center_hz as f64,
        )?;
    }
    let (mut source, rate, center) = open_source(&config.input, stop)?;
    validate_channels(&config.channels, rate, center)?;
    let total = source.samples();
    // Construct every pipeline before starting threads: no partial receiver on invalid input.
    // TCP delivers bursts at network packet boundaries. At 240 kS/s this holds
    // at most 1.28 seconds, still bounded and with explicit loss/reset on overrun.
    let queue_capacity = if matches!(config.input, Input::RtlTcp { .. }) {
        32
    } else {
        8
    };
    let pipelines = config
        .channels
        .into_iter()
        .map(|c| Pipeline::with_decoder(c, rate, center, config.voice_decoder))
        .collect::<Result<Vec<_>, _>>()?;
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    for (channel_index, (mut pipe, ch)) in pipelines
        .into_iter()
        .zip(channels.iter().cloned())
        .enumerate()
    {
        let (tx, rx) = mpsc::sync_channel::<Block>(queue_capacity);
        let cancel = stop.clone();
        let recording = recorder.clone();
        let monitor = monitor.clone();
        let h = thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                    let mut expected = 0;
                    let mut segment_start = 0;
                    let mut audio_origin_us = 0;
                    let channel_id = pipe.config.id.clone();
                    let mut last = Instant::now();
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let block = match rx.recv_timeout(Duration::from_millis(100)) {
                            Ok(b) => b,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        };
                        let to_us = |samples: u64| (samples as f64 / rate * 1_000_000.0) as u64;
                        let mut activity = Vec::new();
                        if block.sequence != expected {
                            pipe.discontinuity()?;
                            segment_start = block.start_sample;
                            activity.push(Activity::Reset(to_us(segment_start)));
                            ch.lock().unwrap().audio.clear();
                        }
                        expected = block.sequence + 1;
                        let mut epoch = pipe.status.epoch;
                        let audio = pipe.process_observed(&block.iq, |event| {
                            let time =
                                to_us(segment_start) + event.frame.bit_offset * 1_000_000 / 9600;
                            audio_origin_us = time.saturating_sub(event.index as u64 * 40_000);
                            if event.epoch != epoch {
                                activity.push(Activity::Reset(time));
                                epoch = event.epoch;
                            }
                            if event.corrected_bits.is_some() {
                                activity.push(Activity::Voice(time));
                            }
                            recording.push_frame(&channel_id, event);
                        })?;
                        monitor.lock().unwrap().update(
                            channel_index,
                            to_us(block.start_sample + block.iq.len() as u64),
                            activity,
                        );
                        for a in &audio {
                            recording.push(a);
                        }
                        let mut c = ch.lock().unwrap();
                        c.progress_us = to_us(block.start_sample + block.iq.len() as u64);
                        if c.status.status.epoch != pipe.status.epoch {
                            c.audio.clear();
                            c.status.status.epoch = pipe.status.epoch;
                        }
                        for a in audio {
                            if c.audio.len() == 125 {
                                c.audio.pop_front();
                                c.status.audio_drops += 1;
                            }
                            c.audio
                                .push_back((audio_origin_us + a.index as u64 * 40_000, a));
                        }
                        if last.elapsed() >= Duration::from_millis(150) {
                            c.status.status = pipe.snapshot();
                            last = Instant::now();
                        }
                    }
                    let tail = pipe.flush()?;
                    for a in &tail {
                        recording.push(a);
                    }
                    let mut c = ch.lock().unwrap();
                    for a in tail {
                        if c.audio.len() == 125 {
                            c.audio.pop_front();
                            c.status.audio_drops += 1;
                        }
                        c.audio
                            .push_back((audio_origin_us + a.index as u64 * 40_000, a));
                    }
                    c.status.status = pipe.snapshot();
                    Ok(())
                }));
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some("Channel worker panicked; restart reception".into()),
            };
            if let Some(e) = error {
                ch.lock().unwrap().status.error = Some(e);
                cancel.store(true, Ordering::Relaxed);
            }
        });
        senders.push(tx);
        handles.push(h);
    }
    {
        let mut s = shared.lock().unwrap();
        s.state = "receiving".into();
        s.rate = rate;
        s.center = center;
    }
    let mut sequence = 0;
    let mut samples = 0u64;
    let start = Instant::now();
    let mut buf = vec![Complex32::default(); (rate * 0.04).round().clamp(384.0, 65536.0) as usize];
    let result = (|| -> Result<(), String> {
        while !stop.load(Ordering::Relaxed) {
            let read = source.read(&mut buf, stop)?;
            let reconnection = source.reconnection();
            if shared.lock().unwrap().reconnection != reconnection {
                shared.lock().unwrap().reconnection = reconnection;
            }
            match read {
                None => break,
                Some(Read::Timeout) => continue,
                Some(Read::Discontinuity) => {
                    sequence += 1;
                    shared.lock().unwrap().overflows += 1;
                    continue;
                }
                Some(Read::Samples(n)) => {
                    samples += n as u64;
                    let iq: Arc<[Complex32]> = Arc::from(&buf[..n]);
                    for (i, tx) in senders.iter().enumerate() {
                        match tx.try_send(Block {
                            sequence,
                            start_sample: samples - n as u64,
                            iq: iq.clone(),
                        }) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                channels[i].lock().unwrap().status.dropped_blocks += 1
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                return Err("A channel worker stopped; restart reception".into())
                            }
                        }
                    }
                    sequence += 1;
                    if let Some(total) = total {
                        shared.lock().unwrap().progress = samples as f64 / total.max(1) as f64;
                        let target = Duration::from_secs_f64(samples as f64 / rate);
                        while !stop.load(Ordering::Relaxed) && start.elapsed() < target {
                            thread::sleep(
                                (target - start.elapsed().min(target))
                                    .min(Duration::from_millis(20)),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    drop(senders);
    for h in handles {
        if h.join().is_err() {
            return Err("Channel thread could not finish".into());
        }
    }
    if let Some(e) = channels
        .iter()
        .find_map(|c| c.lock().unwrap().status.error.clone())
    {
        return Err(e);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    mod recording_lifecycle {
        use super::*;
        use std::{
            fs,
            io::Write,
            path::{Path, PathBuf},
            sync::atomic::AtomicU64,
        };

        const SAMPLES: u32 = 9600;

        struct Temp(PathBuf);
        impl Temp {
            fn new() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let path = std::env::temp_dir().join(format!(
                    "jr-session-recording-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&path).unwrap();
                Self(path)
            }
        }
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn wait_for_state(session: &Session, expected: &str) -> Overview {
            let start = Instant::now();
            loop {
                let status = session.snapshot(None).0;
                if status.state == expected {
                    return status;
                }
                assert!(status.running, "{}: {:?}", status.state, status.error);
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "waiting for {expected}, got {}",
                    status.state
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn gated_wave(directory: &Path) -> (Session, mpsc::Sender<()>) {
            let path = directory.join("silence.wav");
            let size = SAMPLES * 8;
            {
                let mut file = fs::File::create(&path).unwrap();
                file.write_all(b"RIFF").unwrap();
                file.write_all(&(36 + size).to_le_bytes()).unwrap();
                file.write_all(b"WAVEfmt ").unwrap();
                for bytes in [
                    16u32.to_le_bytes(),
                    [3, 0, 2, 0],
                    240000u32.to_le_bytes(),
                    1920000u32.to_le_bytes(),
                    [8, 0, 32, 0],
                ] {
                    file.write_all(&bytes).unwrap();
                }
                file.write_all(b"data").unwrap();
                file.write_all(&size.to_le_bytes()).unwrap();
                file.write_all(&vec![0; size as usize]).unwrap();
            }
            let (release, permits) = mpsc::channel();
            let session = Session::start_with_source(
                Config {
                    input: Input::Wave {
                        path: path.to_string_lossy().into(),
                        center_hz: 352603125.0,
                    },
                    channels: crate::pipeline::presets("east"),
                    voice_decoder: Default::default(),
                },
                move |input, stop| {
                    let (source, rate, center) = Source::open(input, stop)?;
                    Ok((
                        Source::Gated {
                            source: Box::new(source),
                            permits,
                        },
                        rate,
                        center,
                    ))
                },
            )
            .unwrap();
            // No read, including the EOF read, can occur until explicitly
            // released. Slow filesystem operations cannot finish this input.
            wait_for_state(&session, "receiving");
            (session, release)
        }

        /// The silent input yields no call: a cleanly finalized session
        /// leaves no empty folder behind.
        fn assert_finalized(directory: &str) {
            assert!(!Path::new(directory).exists(), "{directory} was kept empty");
        }

        #[test]
        fn recording_stop_preserves_reception_and_allows_restart() {
            let temp = Temp::new();
            let (mut session, _release) = gated_wave(&temp.0);
            assert!(session.start_recording(&temp.0.join("missing")).is_err());
            assert_eq!(session.snapshot(None).0.state, "receiving");

            let first = session.start_recording(&temp.0).unwrap();
            let stopped = session.stop_recording();
            assert_eq!(stopped.state, "completed");
            assert_eq!(stopped.directory, first.directory);
            assert_finalized(first.directory.as_deref().unwrap());
            let status = session.snapshot(None).0;
            assert!(status.running);
            assert_eq!(status.state, "receiving");
            assert!(status.error.is_none());

            let second = session.start_recording(&temp.0).unwrap();
            assert_eq!(second.state, "recording");
            session.stop();
            let status = session.snapshot(None).0;
            assert_eq!(status.state, "stopped");
            assert_eq!(status.recording.state, "completed");
            assert_finalized(second.directory.as_deref().unwrap());
        }

        #[test]
        fn wave_eof_finalizes_recording_after_all_workers_drain() {
            let temp = Temp::new();
            let (mut session, release) = gated_wave(&temp.0);
            let recording = session.start_recording(&temp.0).unwrap();
            assert_eq!(recording.state, "recording");
            // One 40 ms IQ block, then the real IqWave EOF. Neither depends
            // on how long recording setup took on this machine.
            release.send(()).unwrap();
            release.send(()).unwrap();
            let done = wait_for_state(&session, "completed");
            assert!(!done.running);
            assert!(done.error.is_none());
            assert_eq!(done.recording.state, "completed");
            assert_eq!(done.recording.directory, recording.directory);
            assert_eq!(done.channels.len(), 22);
            for channel in done.channels {
                assert_eq!(channel.status.iq_samples, SAMPLES as u64);
                assert_eq!(channel.dropped_blocks, 0);
                assert!(channel.error.is_none());
            }
            assert_finalized(recording.directory.as_deref().unwrap());
            assert!(session.start_recording(&temp.0).is_err());
            session.stop();
        }

        #[test]
        fn dropping_session_finalizes_recording_before_input_eof() {
            let temp = Temp::new();
            let (session, _release) = gated_wave(&temp.0);
            let recording = session.start_recording(&temp.0).unwrap();
            assert_eq!(recording.state, "recording");
            drop(session);
            assert_finalized(recording.directory.as_deref().unwrap());
        }
    }

    fn monitor_test_session() -> Session {
        let configs = crate::pipeline::presets("east")[..2].to_vec();
        let monitor = Arc::new(Mutex::new(Monitor::new(
            configs.iter().map(|c| c.id.clone()),
        )));
        monitor
            .lock()
            .unwrap()
            .update(1, 400_000, vec![Activity::Voice(100_000)]);
        monitor
            .lock()
            .unwrap()
            .update(0, 400_000, vec![Activity::Voice(200_000)]);
        Session {
            stop: Arc::new(AtomicBool::new(false)),
            shared: Arc::new(Mutex::new(Shared {
                state: "receiving".into(),
                error: None,
                rate: 240000.0,
                center: 352603125.0,
                overflows: 0,
                progress: 0.0,
                reconnection: (false, 0, 0),
            })),
            channels: configs
                .iter()
                .map(|c| {
                    Arc::new(Mutex::new(Channel {
                        status: ChannelStatus {
                            config: c.clone(),
                            status: Status::default(),
                            dropped_blocks: 0,
                            audio_drops: 0,
                            error: None,
                        },
                        progress_us: 400_000,
                        monitored: false,
                        audio: VecDeque::from([(
                            200_000,
                            Audio {
                                channel: c.id.clone(),
                                epoch: 0,
                                index: 0,
                                pcm: vec![42; 320],
                            },
                        )]),
                    }))
                })
                .collect(),
            thread: None,
            recorder: Arc::new(Recorder::default()),
            monitor,
        }
    }

    #[test]
    fn operation_monitor_survives_selection_filter_without_changing_audio_choice() {
        let session = monitor_test_session();
        let message = crate::data::synthetic_operation();
        {
            let mut channel = session.channels[0].lock().unwrap();
            crate::data::OperationReception::update(
                &mut channel.status.status.operation_monitor,
                &message,
                "2025-03-14T00:27:00Z".into(),
            );
            channel
                .status
                .status
                .messages
                .push(crate::pipeline::Received {
                    message,
                    receptions: 1,
                });
        }
        for automatic in [false, true] {
            let id = session.channels[1].lock().unwrap().status.config.id.clone();
            let (status, _) = session.snapshot_with_monitor(Some(&id), automatic);
            assert_eq!(status.selected_channel.as_deref(), Some(id.as_str()));
            assert!(status.channels[0].status.messages.is_empty());
            assert_eq!(
                status.channels[0]
                    .status
                    .operation_monitor
                    .as_ref()
                    .unwrap()
                    .summary
                    .rows
                    .len(),
                7
            );
            assert_eq!(status.channels[1].config.id, id);
        }
        // A new session cannot inherit the previous source's received table.
        assert!(monitor_test_session()
            .snapshot(None)
            .0
            .channels
            .iter()
            .all(|c| c.status.operation_monitor.is_none()));
    }

    #[test]
    fn selected_audio_survives_half_second_delivery_bursts() {
        for automatic in [false, true] {
            let session = monitor_test_session();
            let configs = crate::pipeline::presets("east");
            // Both selection modes keep listening to the first active call.
            let id = &configs[1].id;
            assert_eq!(
                session.snapshot_with_monitor(Some(id), automatic).1.len(),
                1
            );
            for batch in 0..100 {
                let first = 1 + batch * 14;
                let mut c = session.channels[1].lock().unwrap();
                for index in first..first + 14 {
                    c.audio.push_back((
                        200_000 + index as u64 * 40_000,
                        Audio {
                            channel: id.clone(),
                            epoch: 0,
                            index,
                            pcm: vec![42; 320],
                        },
                    ));
                }
                // 560 ms of PCM arrives together, with 160 ms synthesis
                // lookahead plus the final 40 ms radio frame already read.
                c.progress_us = 200_000 + (first + 13) as u64 * 40_000 + 200_000;
                drop(c);
                let (status, audio) = session.snapshot_with_monitor(Some(id), automatic);
                assert_eq!(
                    audio.iter().map(|a| a.index).collect::<Vec<_>>(),
                    (first..first + 14).collect::<Vec<_>>(),
                    "selected audio lost in batch {batch}, automatic={automatic}"
                );
                assert_eq!(status.channels[1].audio_drops, 0);
                // Empty UI polls between network bursts must preserve the
                // selection and cannot make the next burst stale again.
                assert!(session
                    .snapshot_with_monitor(Some(id), automatic)
                    .1
                    .is_empty());
            }
        }
    }

    #[test]
    fn native_player_takes_audio_while_status_polls_leave_it_queued() {
        let session = monitor_test_session();
        let configs = crate::pipeline::presets("east");
        // A status-only poll reports the automatic choice but keeps its PCM.
        let (status, audio) = session.snapshot_with_options(Some(&configs[0].id), true, false);
        assert_eq!(
            status.selected_channel.as_deref(),
            Some(configs[1].id.as_str())
        );
        assert!(audio.is_empty());
        assert!(status.channels[0].status.messages.is_empty());
        let audio = session.take_monitor_audio(Some(&configs[0].id), true);
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].channel, configs[1].id);
        assert!(session
            .take_monitor_audio(Some(&configs[0].id), true)
            .is_empty());
        // Manual selection is authoritative for the native player too.
        let audio = session.take_monitor_audio(Some(&configs[0].id), false);
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].channel, configs[0].id);
    }

    #[test]
    fn automatic_snapshot_delivers_chosen_audio_and_manual_selection_remains_authoritative() {
        let session = monitor_test_session();
        let configs = crate::pipeline::presets("east");
        let (status, audio) = session.snapshot_with_monitor(Some(&configs[0].id), true);
        assert_eq!(
            status.selected_channel.as_deref(),
            Some(configs[1].id.as_str())
        );
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].channel, configs[1].id);
        // Manual selection uses the exact requested channel even during calls.
        let (status, audio) = session.snapshot_with_monitor(Some(&configs[0].id), false);
        assert_eq!(
            status.selected_channel.as_deref(),
            Some(configs[0].id.as_str())
        );
        assert_eq!(audio[0].channel, configs[0].id);
        assert_eq!(
            session
                .snapshot_with_monitor(Some(&configs[0].id), true)
                .0
                .selected_channel
                .as_deref(),
            Some(configs[1].id.as_str())
        );
        // Switching to a silent channel cannot replay a long-completed call.
        let mut c = session.channels[0].lock().unwrap();
        c.audio.push_back((
            200_000,
            Audio {
                channel: configs[0].id.clone(),
                epoch: 0,
                index: 1,
                pcm: vec![42; 320],
            },
        ));
        c.progress_us = 2_000_000;
        drop(c);
        assert!(session
            .snapshot_with_monitor(Some(&configs[0].id), false)
            .1
            .is_empty());
    }
}

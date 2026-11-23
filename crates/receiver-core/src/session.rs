//! One acquisition stream, shared immutable IQ blocks, and independent channel workers.
//! Bounded queues cannot silently concatenate radio data across lost samples.
use crate::{
    monitor::{Activity, Monitor},
    pipeline::{validate_channels, Audio, ChannelConfig, Pipeline, Status},
    recording::{OutputOptions, Recorder, RecordingStatus},
    source::{Control, RadioConfig, Read},
    wave::IqWave,
    Complex32,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Sdr {
        radio: RadioConfig,
    },
    RtlTcp {
        radio: crate::rtl_tcp::Config,
    },
    Wave {
        path: String,
        center_hz: f64,
        /// Decode as fast as the CPU allows instead of in real time (for
        /// analysing recordings; there is no live audio then).
        #[serde(default)]
        fast: bool,
    },
    /// Built-in USB driver (Android). Present in every build so settings
    /// round-trip; opening it needs the `usb` feature.
    Usb {
        radio: UsbRadio,
    },
}
#[cfg(feature = "usb")]
pub use crate::usb::Config as UsbRadio;
/// Placeholder when the built-in USB driver is not compiled in.
#[cfg(not(feature = "usb"))]
#[derive(Clone, Serialize, Deserialize)]
pub struct UsbRadio {
    pub sample_rate: u32,
    pub center_hz: u32,
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub ppm: i32,
    #[serde(default)]
    pub bias_tee: bool,
    #[serde(default)]
    pub gain_optimize: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub input: Input,
    pub channels: Vec<ChannelConfig>,
}
#[derive(Clone, Serialize)]
pub struct ChannelStatus {
    pub config: ChannelConfig,
    pub status: Status,
    pub dropped_blocks: u64,
    pub audio_drops: u64,
    /// Times this channel's processing failed and was restarted on its own;
    /// the other channels kept receiving.
    #[serde(default)]
    pub restarts: u64,
    pub error: Option<String>,
}
struct Channel {
    status: ChannelStatus,
    audio: VecDeque<(u64, Audio)>,
    progress_us: u64,
    monitored: bool,
}
impl Channel {
    /// A copy of this channel's status; the message history and constellation
    /// only for the monitored channel (the others' would be discarded).
    fn status_copy(&mut self, monitored: bool) -> ChannelStatus {
        if monitored {
            return self.status.clone();
        }
        let messages = std::mem::take(&mut self.status.status.messages);
        let constellation = std::mem::take(&mut self.status.status.constellation);
        let copy = self.status.clone();
        self.status.status.messages = messages;
        self.status.status.constellation = constellation;
        copy
    }
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
/// File name for a diagnostic IQ capture started now: sortable UTC time,
/// then the centre frequency and sample rate the IQ WAV input needs.
pub fn iq_capture_file_name(center_hz: f64, sample_rate: f64) -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let instant = chrono::DateTime::from_timestamp(t.as_secs() as i64, 0).unwrap_or_default();
    format!(
        "iq-{}-{}Hz-{}.wav",
        instant.format("%Y%m%dT%H%M%SZ"),
        center_hz.round() as u64,
        sample_rate.round() as u64
    )
}
/// A diagnostic raw IQ capture ([`Session::capture_iq`]).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct IqCapture {
    /// "saving", "saved" or "failed".
    pub state: &'static str,
    pub path: String,
    /// Why it failed or ended early.
    pub message: Option<String>,
    /// Requested and written length.
    pub seconds: f64,
    pub written_seconds: f64,
    pub bytes: u64,
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
    /// IQ samples per second actually arriving (over the last few seconds;
    /// 0 until measured). Should match `sample_rate` for live inputs.
    pub input_rate: f64,
    /// Diagnostic raw IQ capture (the latest one in this session).
    pub iq_capture: Option<IqCapture>,
    /// Automatic gain by the noise floor, when asked for.
    #[serde(default)]
    pub gain: Option<crate::gain::GainStatus>,
    /// What the input reports about the receiver (built-in driver: tuner and
    /// RTL-SDR Blog V4 handling).
    pub source_info: Option<String>,
    /// Automatic frequency correction applied to every channel (Hz; 0 until
    /// the synchronised channels show a common carrier error).
    #[serde(default)]
    pub frequency_correction_hz: f64,
    /// The same correction as a tuner frequency correction (ppm) to add to
    /// the configured one, so the next session starts on frequency.
    #[serde(default)]
    pub frequency_correction_ppm: f64,
    /// Blocks with non-finite samples, dropped as discontinuities.
    #[serde(default)]
    pub invalid_blocks: u64,
    /// Why the last tuning change while receiving could not be applied.
    #[serde(default)]
    pub control_error: Option<String>,
}
struct Shared {
    state: String,
    error: Option<String>,
    rate: f64,
    center: f64,
    overflows: u64,
    progress: f64,
    reconnection: (bool, u32, u64),
    input_rate: f64,
    /// Diagnostic raw IQ capture: requested file and sample count, and the
    /// latest capture's state.
    iq_request: Option<(std::path::PathBuf, u64)>,
    iq: Option<IqCapture>,
    gain: Option<crate::gain::GainStatus>,
    source_info: Option<String>,
    frequency_correction: f64,
    invalid_blocks: u64,
    /// Tuning change waiting for the acquisition thread, and the outcome of
    /// the last one.
    control: Option<Control>,
    control_error: Option<String>,
}
pub struct Session {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    channels: Vec<Arc<Mutex<Channel>>>,
    // Behind a mutex so that `stop` takes `&self`: callers can stop a shared
    // session without holding the lock that publishes it.
    thread: Mutex<Option<JoinHandle<()>>>,
    recorder: Arc<Recorder>,
    monitor: Arc<Mutex<Monitor>>,
}
struct Block {
    sequence: u64,
    start_sample: u64,
    iq: Arc<[Complex32]>,
    info: crate::pipeline::BlockInfo,
}
enum Source {
    Wave(IqWave),
    RtlTcp(crate::rtl_tcp::Reconnecting),
    #[cfg(feature = "usb")]
    Usb(Box<crate::usb::Usb>),
    #[cfg(feature = "soapy")]
    Sdr(crate::source::Reconnecting),
    #[cfg(test)]
    Gated {
        source: Box<Source>,
        permits: mpsc::Receiver<()>,
    },
    /// Silence forever, paced at about real time (block count so far).
    #[cfg(test)]
    Endless(u64),
    /// An R820T-like receiver for the gain optimizer: white noise from the
    /// ADC (-60 dB) plus the tuner's (-90 dB + gain), as fast as it can;
    /// records every gain set (None: back to automatic).
    #[cfg(test)]
    GainModel {
        gain: f64,
        seed: u64,
        applied: Arc<Mutex<Vec<Option<f64>>>>,
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
            Input::Usb { radio } => {
                #[cfg(feature = "usb")]
                {
                    let s = crate::usb::Usb::open(radio, stop)?;
                    Ok((
                        Self::Usb(Box::new(s)),
                        radio.sample_rate as f64,
                        radio.center_hz as f64,
                    ))
                }
                #[cfg(not(feature = "usb"))]
                {
                    let _ = (radio, stop);
                    Err("USB 直結はこのビルドでは使えません".into())
                }
            }
            Input::Wave {
                path, center_hz, ..
            } => {
                let w = IqWave::open(path).map_err(|e| e.to_string())?;
                let rate = w.sample_rate;
                Ok((Self::Wave(w), rate, *center_hz))
            }
            Input::Sdr { radio } => {
                #[cfg(feature = "soapy")]
                {
                    let s = crate::source::Reconnecting::open(radio)?;
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
    fn info(&self) -> Option<String> {
        match self {
            #[cfg(feature = "usb")]
            Self::Usb(s) => Some(s.info()),
            Self::RtlTcp(s) => s.info(),
            #[cfg(test)]
            Self::Gated { source, .. } => source.info(),
            _ => None,
        }
    }
    /// Applies a tuning change while receiving.
    fn apply(&mut self, control: &Control) -> Result<(), String> {
        match self {
            Self::RtlTcp(s) => s.apply(control),
            #[cfg(feature = "usb")]
            Self::Usb(s) => s.apply(control),
            #[cfg(feature = "soapy")]
            Self::Sdr(s) => s.apply(control),
            #[cfg(test)]
            Self::Gated { source, .. } => source.apply(control),
            #[cfg(test)]
            Self::GainModel { gain, applied, .. } => {
                if let Some(g) = control.gain_db {
                    *gain = g.unwrap_or(49.6);
                    crate::lock(applied).push(g);
                }
                Ok(())
            }
            _ => Err("IQ WAV の再生中はゲインと周波数補正を変更できません".into()),
        }
    }
    /// The tuner's gain table (0.1 dB steps) for the gain optimizer, when
    /// the input uses librtlsdr's R820T/R828D one.
    fn gain_table(&self) -> Option<&'static [u16]> {
        match self {
            Self::RtlTcp(s) => s.gain_table(),
            #[cfg(feature = "usb")]
            Self::Usb(_) => Some(&crate::rtl_tcp::GAINS_TENTH_DB[..]),
            #[cfg(test)]
            Self::Gated { source, .. } => source.gain_table(),
            #[cfg(test)]
            Self::GainModel { .. } => Some(&crate::rtl_tcp::GAINS_TENTH_DB[..]),
            _ => None,
        }
    }
    /// RTL-TCP reconnection state: (reconnecting, failed attempts, reconnects).
    fn reconnection(&self) -> (bool, u32, u64) {
        match self {
            Self::RtlTcp(s) => (s.reconnecting(), s.failed_attempts, s.reconnects),
            #[cfg(feature = "usb")]
            Self::Usb(s) => (s.reconnecting(), s.failed_attempts, s.reconnects),
            #[cfg(feature = "soapy")]
            Self::Sdr(s) => (s.reconnecting(), s.failed_attempts, s.reconnects),
            #[cfg(test)]
            Self::Gated { source, .. } => source.reconnection(),
            _ => (false, 0, 0),
        }
    }
    fn read(&mut self, buf: &mut [Complex32], stop: &AtomicBool) -> Result<Option<Read>, String> {
        match self {
            Self::RtlTcp(s) => s.read(buf, stop).map(Some),
            #[cfg(feature = "usb")]
            Self::Usb(s) => s.read(buf, stop).map(Some),
            Self::Wave(w) => {
                let n = w.read(buf).map_err(|e| e.to_string())?;
                Ok(if n == 0 { None } else { Some(Read::Samples(n)) })
            }
            #[cfg(feature = "soapy")]
            Self::Sdr(s) => s.read(buf, stop).map(Some),
            #[cfg(test)]
            Self::Endless(blocks) => {
                *blocks += 1;
                thread::sleep(Duration::from_millis(5));
                buf.fill(Complex32::default());
                Ok(Some(Read::Samples(buf.len())))
            }
            #[cfg(test)]
            Self::GainModel { gain, seed, .. } => {
                let sigma = ((1e-6 + 10f64.powf((-90.0 + *gain) / 10.0)) / 2.0).sqrt();
                let mut uniform = || {
                    *seed ^= *seed << 13;
                    *seed ^= *seed >> 7;
                    *seed ^= *seed << 17;
                    (*seed >> 11) as f64 / (1u64 << 53) as f64
                };
                for v in buf.iter_mut() {
                    let r = (-2.0 * uniform().max(1e-300).ln()).sqrt() * sigma;
                    let a = 2.0 * std::f64::consts::PI * uniform();
                    *v = Complex32 {
                        real: (r * a.cos()) as f32,
                        imaginary: (r * a.sin()) as f32,
                    };
                }
                Ok(Some(Read::Samples(buf.len())))
            }
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
            input_rate: 0.0,
            iq_request: None,
            iq: None,
            gain: None,
            source_info: None,
            frequency_correction: 0.0,
            invalid_blocks: 0,
            control: None,
            control_error: None,
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
                        restarts: 0,
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
                let mut s = crate::lock(&state);
                // Serialize automatic finalization with starting a recording.
                // Workers have already flushed their last synthesized audio.
                crate::lock(&call_monitor).finish();
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
                                .all(|c| crate::lock(c).status.error.is_none()) =>
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
                crate::diag::note(format!(
                    "受信を終了しました（{}{}）",
                    s.state,
                    s.error
                        .as_deref()
                        .map_or(String::new(), |e| format!("：{e}"))
                ));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            shared,
            channels,
            thread: Mutex::new(Some(handle)),
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
        let s = crate::lock(&self.shared);
        let (preferred, active_calls) = crate::lock(&self.monitor).snapshot();
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
                let mut c = crate::lock(c);
                let monitored = selected == Some(c.status.config.id.as_str());
                if take_audio {
                    c.route_audio(monitored, &mut audio);
                }
                c.status_copy(monitored)
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
                input_rate: s.input_rate,
                iq_capture: s.iq.clone(),
                gain: s.gain.clone(),
                source_info: s.source_info.clone(),
                frequency_correction_hz: s.frequency_correction,
                frequency_correction_ppm: if s.center > 0.0 {
                    -s.frequency_correction / s.center * 1e6
                } else {
                    0.0
                },
                invalid_blocks: s.invalid_blocks,
                control_error: s.control_error.clone(),
            },
            audio,
        )
    }
    /// Changes gain, frequency correction or bias tee while receiving. The
    /// acquisition thread applies it between two reads (within 100 ms) and
    /// keeps it for reconnections; a failure is reported in the status.
    pub fn set_control(&self, control: Control) -> Result<(), String> {
        control.validate()?;
        let mut s = crate::lock(&self.shared);
        if !matches!(s.state.as_str(), "starting" | "receiving") {
            return Err("受信中に変更してください。".into());
        }
        match s.control.as_mut() {
            Some(pending) => pending.merge(control),
            None => s.control = Some(control),
        }
        s.control_error = None;
        Ok(())
    }
    /// Status of every channel without touching the monitor audio: safe to
    /// call from any number of observers (UI, notification, tools).
    pub fn status(&self, selected: Option<&str>, automatic: bool) -> Overview {
        self.snapshot_with_options(selected, automatic, false).0
    }
    /// Whether the session is connecting or receiving (cheap; no copies).
    pub fn is_running(&self) -> bool {
        matches!(
            crate::lock(&self.shared).state.as_str(),
            "starting" | "receiving"
        )
    }
    /// Audio-only counterpart of [`Self::snapshot_with_monitor`] for a native
    /// player: the same channel choice, without cloning any channel status.
    pub fn take_monitor_audio(&self, selected: Option<&str>, automatic: bool) -> Vec<Audio> {
        self.take_monitor_audio_chosen(selected, automatic).1
    }
    /// [`Self::take_monitor_audio`] with the channel it was taken for (the
    /// automatic choice when `automatic`), so a pushed stream can tell the
    /// page which channel it carries.
    pub fn take_monitor_audio_chosen(
        &self,
        selected: Option<&str>,
        automatic: bool,
    ) -> (Option<String>, Vec<Audio>) {
        let (preferred, _) = crate::lock(&self.monitor).snapshot();
        let selected = if automatic {
            preferred.as_deref().or(selected)
        } else {
            selected
        };
        let chosen = selected.map(String::from);
        let mut audio = Vec::new();
        for c in &self.channels {
            let mut c = crate::lock(c);
            let monitored = selected == Some(c.status.config.id.as_str());
            c.route_audio(monitored, &mut audio);
        }
        (chosen, audio)
    }
    /// Ask the session to stop without waiting (see [`Self::stop`]).
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn stop(&self) {
        self.request_stop();
        let handle = crate::lock(&self.thread).take();
        if let Some(h) = handle {
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
        let s = crate::lock(&self.shared);
        if s.state != "receiving" || self.stop.load(Ordering::Relaxed) {
            return Err("受信を開始し、接続が完了してから録音してください。".into());
        }
        let configs: Vec<_> = self
            .channels
            .iter()
            .map(|c| crate::lock(c).status.config.clone())
            .collect();
        self.recorder
            .start_with_options(directory, &configs, options)
    }
    pub fn stop_recording(&self) -> RecordingStatus {
        self.recorder.stop()
    }
    /// Saves the next `seconds` (1-60) of raw IQ (16-bit stereo WAV, I left
    /// / Q right) for diagnosis; the IQ WAV input can replay it. Progress
    /// and the outcome are in [`Overview::iq_capture`].
    pub fn capture_iq(&self, path: std::path::PathBuf, seconds: f64) -> Result<(), String> {
        let mut s = crate::lock(&self.shared);
        if s.state != "receiving" {
            return Err("受信中に保存してください。".into());
        }
        if s.iq_request.is_some() || s.iq.as_ref().is_some_and(|c| c.state == "saving") {
            return Err("IQ を保存中です。".into());
        }
        let seconds = if seconds.is_finite() {
            seconds.clamp(1.0, 60.0)
        } else {
            10.0
        };
        let samples = (s.rate * seconds).round() as u64;
        s.iq = Some(IqCapture {
            state: "saving",
            path: path.to_string_lossy().into_owned(),
            message: None,
            seconds,
            written_seconds: 0.0,
            bytes: 0,
        });
        s.iq_request = Some((path, samples));
        Ok(())
    }
}

/// Raw IQ to a 16-bit stereo WAV (I left, Q right). A thread of its own
/// writes the file, so slow storage cannot hold up reception: if it falls
/// more than [`IQ_QUEUE`] blocks behind, the capture stops there. The thread
/// sizes the header and reports the outcome in the session's status.
struct IqWriter {
    tx: Option<mpsc::SyncSender<Vec<u8>>>,
    remaining: u64,
    written: u64,
    rate: f64,
}
/// Blocks (40 ms each) the file writer may fall behind.
const IQ_QUEUE: usize = 64;
impl IqWriter {
    fn create(
        path: std::path::PathBuf,
        samples: u64,
        rate: f64,
        shared: Arc<Mutex<Shared>>,
    ) -> std::io::Result<Self> {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?,
        );
        let sample_rate = rate.round() as u32;
        file.write_all(b"RIFF\0\0\0\0WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?; // PCM
        file.write_all(&2u16.to_le_bytes())?; // I, Q
        file.write_all(&sample_rate.to_le_bytes())?;
        file.write_all(&(sample_rate * 4).to_le_bytes())?;
        file.write_all(&4u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data\0\0\0\0")?;
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(IQ_QUEUE);
        std::thread::Builder::new()
            .name("iq-capture".into())
            .spawn(move || {
                let mut data = 0u64;
                let mut result = (|| -> std::io::Result<()> {
                    for bytes in rx {
                        file.write_all(&bytes)?;
                        data += bytes.len() as u64;
                    }
                    Ok(())
                })();
                // The header always matches what was written, so even a
                // capture cut short replays.
                result = result.and_then(|_| {
                    use std::io::{Seek, SeekFrom};
                    let size = u32::try_from(data).unwrap_or(u32::MAX);
                    file.seek(SeekFrom::Start(4))?;
                    file.write_all(&size.saturating_add(36).to_le_bytes())?;
                    file.seek(SeekFrom::Start(40))?;
                    file.write_all(&size.to_le_bytes())?;
                    file.flush()?;
                    file.get_ref().sync_all()
                });
                let mut s = crate::lock(&shared);
                if let Some(c) = s.iq.as_mut().filter(|c| c.state == "saving") {
                    c.bytes = data + 44;
                    c.written_seconds = data as f64 / 4.0 / rate;
                    match result {
                        Ok(()) => c.state = "saved",
                        Err(e) => {
                            c.state = "failed";
                            c.message = Some(format!("IQ を保存できません：{e}"));
                        }
                    }
                    if c.state == "saved" && c.written_seconds + 0.05 < c.seconds {
                        c.message
                            .get_or_insert_with(|| "受信が止まったため途中までになりました".into());
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            remaining: samples,
            written: 0,
            rate,
        })
    }
    /// Queues `iq`; returns true when the requested length has been queued,
    /// or the reason the capture stopped.
    fn write(&mut self, iq: &[Complex32]) -> Result<bool, String> {
        let n = (iq.len() as u64).min(self.remaining) as usize;
        let mut bytes = Vec::with_capacity(n * 4);
        for s in &iq[..n] {
            for v in [s.real, s.imaginary] {
                let x = (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                bytes.extend_from_slice(&x.to_le_bytes());
            }
        }
        let tx = self.tx.as_ref().ok_or("IQ の保存は終わっています")?;
        match tx.try_send(bytes) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.tx = None;
                return Err("保存先への書き込みが追いつかないため途中で止めました".into());
            }
            // The writer stopped on an error it reports itself.
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.tx = None;
                return Err(String::new());
            }
        }
        self.remaining -= n as u64;
        self.written += n as u64;
        Ok(self.remaining == 0)
    }
    fn written_seconds(&self) -> f64 {
        self.written as f64 / self.rate
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
/// Clipped I/Q components (at full scale: the dongles' 8-bit codes 0 and 255
/// convert to exactly ∓1) and the largest component, for the gain optimizer.
fn clipping(samples: &[Complex32]) -> (u64, f32) {
    let (mut clipped, mut peak) = (0u64, 0f32);
    for s in samples {
        let (re, im) = (s.real.abs(), s.imaginary.abs());
        clipped += u64::from(re >= 0.999) + u64::from(im >= 0.999);
        peak = peak.max(re).max(im);
    }
    (clipped, peak.min(1.0))
}
/// IQ samples per block handed to the channel workers (40 ms).
fn block_samples(rate: f64) -> usize {
    (rate * 0.04).round().clamp(384.0, 65536.0) as usize
}
/// Blocks each channel worker may have queued. TCP delivers bursts at network
/// packet boundaries, and USB a whole bulk transfer at once; at 240 kS/s 32
/// blocks hold 1.28 seconds, still bounded and with explicit loss/reset on
/// overrun. An overrun resets the channel's synchronisation, so a queue
/// shorter than one burst would never synchronise.
fn queue_capacity(input: &Input) -> usize {
    if matches!(input, Input::RtlTcp { .. } | Input::Usb { .. }) {
        32
    } else {
        8
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
    if let Input::Usb { radio } = &config.input {
        #[cfg(feature = "usb")]
        radio.validate()?;
        validate_channels(
            &config.channels,
            radio.sample_rate as f64,
            radio.center_hz as f64,
        )?;
    }
    crate::diag::note(format!(
        "受信を開始します（{}・{} 波）",
        match &config.input {
            Input::Sdr { .. } => "SoapySDR",
            Input::RtlTcp { .. } => "RTL-TCP",
            Input::Wave { .. } => "IQ WAV",
            Input::Usb { .. } => "USB 直結",
        },
        config.channels.len()
    ));
    let (mut source, rate, center) =
        open_source(&config.input, stop).inspect_err(|e| crate::diag::note(e.clone()))?;
    crate::lock(shared).source_info = source.info();
    validate_channels(&config.channels, rate, center)?;
    let total = source.samples();
    // Construct every pipeline before starting threads: no partial receiver on invalid input.
    let queue_capacity = queue_capacity(&config.input);
    // One polyphase filter bank splits the input for every channel, which
    // then works at about 24 kS/s; without it (or below 96 kS/s) each
    // channel mixes and filters the whole input itself.
    let mut channelizer = crate::tuning::on(crate::tuning::CHANNELIZER)
        .then(|| jr_conventional_dsp::channelizer::Channelizer::new(rate))
        .flatten();
    // Each pipeline's input rate and centre, and its channelizer bin.
    let mut placements = Vec::new();
    let pipelines = config
        .channels
        .into_iter()
        .map(|c| {
            let placement = match channelizer.as_mut() {
                Some(bank) => {
                    let p = bank.place(c.frequency_hz - center);
                    (p.rate, c.frequency_hz - p.residual_hz, Some(p.bin))
                }
                None => (rate, center, None),
            };
            placements.push(placement);
            Pipeline::new(c, placement.0, placement.1)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut bands: Vec<(usize, Vec<Complex32>)> = Vec::new();
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    // Automatic frequency correction (f64 bits, Hz), set by this thread from
    // the synchronised channels and applied by every worker.
    let tuning = Arc::new(AtomicU64::new(0f64.to_bits()));
    for (channel_index, (pipe, ch)) in pipelines
        .into_iter()
        .zip(channels.iter().cloned())
        .enumerate()
    {
        let (tx, rx) = mpsc::sync_channel::<Block>(queue_capacity);
        let worker = Worker {
            index: channel_index,
            rate: placements[channel_index].0,
            center: placements[channel_index].1,
            cancel: stop.clone(),
            recording: recorder.clone(),
            monitor: monitor.clone(),
            tuning: tuning.clone(),
            channel: ch,
        };
        let name = format!("channel-{}", pipe.config.id);
        let h = thread::Builder::new()
            .name(name)
            .spawn(move || worker.run(pipe, rx))
            .map_err(|e| format!("チャンネルの処理を開始できません：{e}"))?;
        senders.push(Some(tx));
        handles.push(h);
    }
    {
        let mut s = crate::lock(shared);
        s.state = "receiving".into();
        s.rate = rate;
        s.center = center;
    }
    // A recording is decoded faster than real time only on request; live
    // inputs never wait for a slow channel (it drops blocks instead).
    let (paced, blocking) = match &config.input {
        Input::Wave { fast, .. } => (!*fast, true),
        _ => (false, false),
    };
    let mut sequence = 0;
    let mut samples = 0u64;
    let start = Instant::now();
    let mut window = (Instant::now(), 0u64);
    let mut iq_capture: Option<IqWriter> = None;
    let mut afc = crate::afc::Afc::default();
    let mut afc_due = rate as u64;
    let mut buf = vec![Complex32::default(); block_samples(rate)];
    // Automatic gain by the noise floor (crate::gain): the user's gain
    // (None: automatic) and choice, and the optimizer once it could start.
    let (mut user_gain, mut optimize) = match &config.input {
        Input::RtlTcp { radio } => (radio.gain_db, radio.gain_optimize),
        Input::Usb { radio } => (radio.gain_db, radio.gain_optimize),
        Input::Sdr { radio } => (radio.gain_db, false),
        Input::Wave { .. } => (None, false),
    };
    let channel_bins: Vec<usize> = placements.iter().filter_map(|p| p.2).collect();
    let mut optimizer: Option<crate::gain::GainOptimizer> = None;
    let mut optimizer_wanted = optimize && user_gain.is_none();
    let mut idle = (Instant::now(), true);
    let result = (|| -> Result<(), String> {
        while !stop.load(Ordering::Relaxed) {
            let control = crate::lock(shared).control.take();
            if let Some(control) = control {
                // Only what changed reaches the source: re-sending the same
                // automatic gain (with a new ppm) would restart its search.
                let gain_changed = control.gain_db.is_some_and(|g| g != user_gain);
                let optimize_changed = control.gain_optimize.is_some_and(|o| o != optimize);
                user_gain = control.gain_db.unwrap_or(user_gain);
                optimize = control.gain_optimize.unwrap_or(optimize);
                let mut change = control.clone();
                change.gain_optimize = None;
                if !gain_changed {
                    change.gain_db = None;
                }
                if (gain_changed || optimize_changed) && optimizer.is_some() && user_gain.is_none()
                {
                    // From the optimizer's gains back to the source's own.
                    change.gain_db = Some(None);
                }
                let outcome = if change == Control::default() {
                    Ok(())
                } else {
                    source.apply(&change)
                };
                if let Err(e) = &outcome {
                    crate::diag::note(format!("受信設定を変更できません：{e}"));
                }
                crate::lock(shared).control_error = outcome.err();
                if gain_changed || optimize_changed {
                    optimizer = None;
                    if let Some(bank) = channelizer.as_mut() {
                        bank.set_measuring(false);
                    }
                    crate::lock(shared).gain = None;
                    optimizer_wanted = optimize && user_gain.is_none();
                }
            }
            if optimizer_wanted {
                optimizer_wanted = false;
                let fft =
                    jr_conventional_dsp::tuning::bins() == jr_conventional_dsp::tuning::Bins::Fft;
                let status = match (source.gain_table(), channelizer.as_mut()) {
                    (_, None) => Err("このサンプルレートでは使えません（従来の自動で受信します）"),
                    (_, Some(_)) if !fft => Err("この設定では使えません（従来の自動で受信します）"),
                    (None, _) => {
                        Err("この入力・チューナーでは使えません（従来の自動で受信します）")
                    }
                    (Some(table), Some(bank)) => {
                        match crate::gain::GainOptimizer::new(table, rate) {
                            Some((opt, first)) => {
                                bank.set_measuring(true);
                                let outcome = source.apply(&Control {
                                    gain_db: Some(Some(first)),
                                    ..Control::default()
                                });
                                if let Err(e) = &outcome {
                                    crate::diag::note(format!("ゲインを変更できません：{e}"));
                                }
                                let status = opt.status();
                                optimizer = Some(opt);
                                Ok(status)
                            }
                            None => {
                                Err("この入力・チューナーでは使えません（従来の自動で受信します）")
                            }
                        }
                    }
                };
                crate::lock(shared).gain =
                    Some(status.unwrap_or_else(crate::gain::GainStatus::unavailable));
            }
            let read = source.read(&mut buf, stop)?;
            let received = std::time::SystemTime::now();
            // Samples after lost ones: mark the gap, then take them.
            let read = match read {
                Some(Read::SamplesAfterGap(n)) => {
                    window = (Instant::now(), 0);
                    sequence += 1;
                    if let Some(bank) = channelizer.as_mut() {
                        bank.reset();
                    }
                    crate::lock(shared).overflows += 1;
                    Some(Read::Samples(n))
                }
                other => other,
            };
            let reconnection = source.reconnection();
            if crate::lock(shared).reconnection != reconnection {
                let mut s = crate::lock(shared);
                s.reconnection = reconnection;
                s.source_info = source.info();
            }
            match read {
                None => break,
                Some(Read::Timeout) => continue,
                Some(Read::SamplesAfterGap(_)) => unreachable!("handled above"),
                Some(Read::Discontinuity) => {
                    window = (Instant::now(), 0);
                    sequence += 1;
                    if let Some(bank) = channelizer.as_mut() {
                        bank.reset();
                    }
                    crate::lock(shared).overflows += 1;
                    continue;
                }
                Some(Read::Samples(n)) => {
                    // One check for every channel: a block with NaN or
                    // infinity (a broken source) is dropped as a gap rather
                    // than ending the session.
                    if buf[..n]
                        .iter()
                        .any(|s| !s.real.is_finite() || !s.imaginary.is_finite())
                    {
                        samples += n as u64;
                        sequence += 1;
                        if let Some(bank) = channelizer.as_mut() {
                            bank.reset();
                        }
                        crate::lock(shared).invalid_blocks += 1;
                        continue;
                    }
                    if iq_capture.is_none() {
                        let request = crate::lock(shared).iq_request.take();
                        if let Some((path, count)) = request {
                            match IqWriter::create(path, count, rate, shared.clone()) {
                                Ok(w) => iq_capture = Some(w),
                                Err(e) => {
                                    if let Some(c) = crate::lock(shared).iq.as_mut() {
                                        c.state = "failed";
                                        c.message = Some(format!("IQ を保存できません：{e}"));
                                    }
                                }
                            }
                        }
                    }
                    if let Some(w) = iq_capture.as_mut() {
                        let outcome = w.write(&buf[..n]);
                        let seconds = w.written_seconds();
                        {
                            let mut s = crate::lock(shared);
                            if let Some(c) = s.iq.as_mut().filter(|c| c.state == "saving") {
                                c.written_seconds = seconds;
                                if let Err(e) = &outcome {
                                    if !e.is_empty() {
                                        c.message = Some(e.clone());
                                    }
                                }
                            }
                        }
                        // Dropping the writer closes its queue: the file
                        // thread finishes the file and reports.
                        if !matches!(outcome, Ok(false)) {
                            iq_capture = None;
                        }
                    }
                    samples += n as u64;
                    window.1 += n as u64;
                    if samples >= afc_due {
                        // Once a second of IQ.
                        afc_due = samples + rate as u64;
                        // Only measurements taken with the correction now in
                        // effect, after the carrier loops settled: a queued
                        // status from before the last step would count it twice.
                        let current = afc.correction();
                        let mut errors: Vec<f64> = channels
                            .iter()
                            .filter_map(|c| {
                                let c = crate::lock(c);
                                let s = &c.status.status;
                                (s.frame_locked
                                    && s.carrier_locked
                                    && s.correction_settled
                                    && s.applied_correction_hz == current)
                                    .then_some(s.frequency_error_hz)
                            })
                            .collect();
                        if let Some(correction) = afc.update(&mut errors) {
                            tuning.store(correction.to_bits(), Ordering::Relaxed);
                            crate::lock(shared).frequency_correction = correction;
                        }
                    }
                    let elapsed = window.0.elapsed();
                    if elapsed >= Duration::from_secs(3) {
                        let mut s = crate::lock(shared);
                        s.input_rate = window.1 as f64 / elapsed.as_secs_f64();
                        s.source_info = source.info();
                        drop(s);
                        window = (Instant::now(), 0);
                    }
                    let iq: Arc<[Complex32]> = Arc::from(&buf[..n]);
                    let mut info = crate::pipeline::BlockInfo {
                        dbfs: Some(crate::pipeline::block_dbfs(&iq)),
                        captured: received.checked_sub(Duration::from_secs_f64(n as f64 / rate)),
                        input_samples: None,
                    };
                    // Each bin's samples, shared by the channels in it, and
                    // where they start at the bins' rate.
                    let mut narrow: Vec<(usize, Arc<[Complex32]>)> = Vec::new();
                    let mut narrow_start = 0;
                    if let Some(bank) = channelizer.as_mut() {
                        for (_, out) in &mut bands {
                            out.clear();
                        }
                        bank.process(&buf[..n], &mut bands);
                        narrow = bands.iter().map(|(k, v)| (*k, Arc::from(&v[..]))).collect();
                        narrow_start = (samples - n as u64) / bank.decimation() as u64;
                        info.input_samples = Some(n as u64);
                    }
                    if let Some(opt) = optimizer.as_mut() {
                        let floor = channelizer
                            .as_mut()
                            .and_then(|bank| bank.take_bin_power())
                            .and_then(|power| crate::gain::noise_floor_db(&power, &channel_bins));
                        let (clipped, peak) = clipping(&buf[..n]);
                        if idle.0.elapsed() >= Duration::from_secs(1) {
                            idle = (Instant::now(), crate::lock(monitor).snapshot().1.is_empty());
                        }
                        let window = crate::gain::Window {
                            floor_db: floor,
                            clipped,
                            samples: n as u64,
                            peak,
                        };
                        if let Some(db) = opt.observe(&window, idle.1) {
                            let outcome = source.apply(&Control {
                                gain_db: Some(Some(db)),
                                ..Control::default()
                            });
                            if let Err(e) = outcome {
                                crate::diag::note(format!("ゲインを変更できません：{e}"));
                            }
                        }
                        let status = opt.status();
                        let mut s = crate::lock(shared);
                        if s.gain.as_ref() != Some(&status) {
                            s.gain = Some(status);
                        }
                    }
                    for (i, slot) in senders.iter_mut().enumerate() {
                        let Some(tx) = slot else { continue };
                        let (start_sample, iq) = match placements[i].2 {
                            Some(bin) => (
                                narrow_start,
                                narrow.iter().find(|(k, _)| *k == bin).unwrap().1.clone(),
                            ),
                            None => (samples - n as u64, iq.clone()),
                        };
                        let block = Block {
                            sequence,
                            start_sample,
                            iq,
                            info,
                        };
                        let gone = if blocking {
                            tx.send(block).is_err()
                        } else {
                            match tx.try_send(block) {
                                Ok(()) => false,
                                Err(TrySendError::Full(_)) => {
                                    crate::lock(&channels[i]).status.dropped_blocks += 1;
                                    false
                                }
                                Err(TrySendError::Disconnected(_)) => true,
                            }
                        };
                        if gone {
                            // That channel gave up after repeated failures
                            // (its status says why); the others go on.
                            *slot = None;
                        }
                    }
                    if senders.iter().all(Option::is_none) {
                        return Err(
                            "すべてのチャンネルの処理が停止しました。受信を再開してください。"
                                .into(),
                        );
                    }
                    sequence += 1;
                    if let Some(total) = total {
                        crate::lock(shared).progress = samples as f64 / total.max(1) as f64;
                        let target = Duration::from_secs_f64(samples as f64 / rate);
                        while paced && !stop.load(Ordering::Relaxed) && start.elapsed() < target {
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
    // One failed channel is reported on that channel; the session fails
    // only when none of them could keep receiving.
    let failed: Vec<String> = channels
        .iter()
        .filter_map(|c| crate::lock(c).status.error.clone())
        .collect();
    if !failed.is_empty() && failed.len() == channels.len() {
        return Err(failed[0].clone());
    }
    result
}

/// One channel's processing thread. A failure (an error or a panic) restarts
/// this channel's pipeline instead of stopping the session; a channel that
/// keeps failing gives up alone.
struct Worker {
    index: usize,
    rate: f64,
    center: f64,
    cancel: Arc<AtomicBool>,
    recording: Arc<Recorder>,
    monitor: Arc<Mutex<Monitor>>,
    tuning: Arc<AtomicU64>,
    channel: Arc<Mutex<Channel>>,
}
/// Failures within [`RESTART_WINDOW`] after which a channel stops.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
#[derive(Default)]
struct WorkerState {
    expected: Option<u64>,
    segment_start: u64,
    audio_origin_us: u64,
    tuned: f64,
    retune_failed: bool,
}
impl Worker {
    fn run(self, mut pipe: Pipeline, rx: mpsc::Receiver<Block>) {
        pipe.trust_input();
        let config = pipe.config.clone();
        let mut failures: VecDeque<Instant> = VecDeque::new();
        let mut state = WorkerState::default();
        loop {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.receive(&mut pipe, &rx, &mut state)
            }));
            let error = match result {
                Ok(Ok(())) => return,
                Ok(Err(e)) => e,
                Err(_) => "Channel worker panicked".into(),
            };
            let now = Instant::now();
            failures.push_back(now);
            while failures
                .front()
                .is_some_and(|t| now.duration_since(*t) > RESTART_WINDOW)
            {
                failures.pop_front();
            }
            let rebuilt = Pipeline::new(config.clone(), self.rate, self.center);
            match rebuilt {
                Ok(fresh)
                    if failures.len() <= MAX_RESTARTS && !self.cancel.load(Ordering::Relaxed) =>
                {
                    crate::diag::note(format!("{} の処理をやり直します：{error}", config.label));
                    pipe = fresh;
                    pipe.trust_input();
                    // The next block starts a new segment (a discontinuity).
                    state = WorkerState::default();
                    let mut c = crate::lock(&self.channel);
                    c.status.restarts += 1;
                    c.audio.clear();
                }
                _ => {
                    crate::diag::note(format!(
                        "{} の処理を停止しました（ほかの波は受信を続けます）：{error}",
                        config.label
                    ));
                    crate::lock(&self.channel).status.error = Some(error);
                    return;
                }
            }
        }
    }
    fn receive(
        &self,
        pipe: &mut Pipeline,
        rx: &mpsc::Receiver<Block>,
        state: &mut WorkerState,
    ) -> Result<(), String> {
        let rate = self.rate;
        let channel_id = pipe.config.id.clone();
        let mut last = Instant::now();
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
            let block = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(b) => b,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            };
            #[cfg(test)]
            tests::injected_failure(&pipe.config.id)?;
            let to_us = |samples: u64| (samples as f64 / rate * 1_000_000.0) as u64;
            let mut activity = Vec::new();
            if state.expected != Some(block.sequence) {
                // Not at the very first block: after lost blocks, or after
                // this channel restarted with a fresh pipeline.
                if state.expected.is_some() {
                    pipe.discontinuity()?;
                }
                if state.expected.is_some() || block.start_sample != 0 {
                    state.segment_start = block.start_sample;
                    activity.push(Activity::Reset(to_us(state.segment_start)));
                    crate::lock(&self.channel).audio.clear();
                }
            }
            state.expected = Some(block.sequence + 1);
            let correction = f64::from_bits(self.tuning.load(Ordering::Relaxed));
            if correction != state.tuned {
                // A channel at the very edge of the IQ bandwidth stays where
                // it is; say so once.
                if let Err(e) = pipe.retune(correction) {
                    if !state.retune_failed {
                        state.retune_failed = true;
                        crate::diag::note(format!(
                            "{} は周波数補正を適用できません：{e}",
                            pipe.config.label
                        ));
                    }
                }
                state.tuned = correction;
            }
            let mut epoch = pipe.status.epoch;
            let segment_start = state.segment_start;
            let audio_origin = &mut state.audio_origin_us;
            let audio = pipe.process_block(&block.iq, block.info, |event| {
                let time = to_us(segment_start) + event.frame.bit_offset * 1_000_000 / 9600;
                *audio_origin = time.saturating_sub(event.index as u64 * 40_000);
                if event.epoch != epoch {
                    activity.push(Activity::Reset(time));
                    epoch = event.epoch;
                }
                if event.corrected_bits.is_some() {
                    activity.push(Activity::Voice(time));
                }
                self.recording.push_frame(&channel_id, event);
            })?;
            crate::lock(&self.monitor).update(
                self.index,
                to_us(block.start_sample + block.iq.len() as u64),
                activity,
            );
            for a in &audio {
                self.recording.push(a);
            }
            let mut c = crate::lock(&self.channel);
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
                    .push_back((state.audio_origin_us + a.index as u64 * 40_000, a));
            }
            if last.elapsed() >= Duration::from_millis(150) {
                pipe.snapshot_into(&mut c.status.status);
                last = Instant::now();
            }
        }
        let tail = pipe.flush()?;
        for a in &tail {
            self.recording.push(a);
        }
        let mut c = crate::lock(&self.channel);
        for a in tail {
            if c.audio.len() == 125 {
                c.audio.pop_front();
                c.status.audio_drops += 1;
            }
            c.audio
                .push_back((state.audio_origin_us + a.index as u64 * 40_000, a));
        }
        pipe.snapshot_into(&mut c.status.status);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Channel id → failures still to inject (errors, then a panic for the
    /// last one when the flag is set).
    static FAILURES: Mutex<Vec<(String, u32, bool)>> = Mutex::new(Vec::new());
    pub(super) fn injected_failure(channel: &str) -> Result<(), String> {
        let mut failures = crate::lock(&FAILURES);
        if let Some(entry) = failures
            .iter_mut()
            .find(|(id, n, _)| id == channel && *n > 0)
        {
            entry.1 -= 1;
            if entry.2 && entry.1 == 0 {
                drop(failures);
                panic!("injected panic");
            }
            return Err("injected failure".into());
        }
        Ok(())
    }

    #[test]
    fn a_failing_channel_restarts_or_stops_alone() {
        // Own channel ids: other tests run sessions concurrently.
        let configs: Vec<_> = crate::pipeline::presets("west")
            .into_iter()
            .map(|mut c| {
                c.id = format!("isolation-{}", c.id);
                c
            })
            .collect();
        let (flaky, broken) = (configs[0].id.clone(), configs[1].id.clone());
        // One error then a panic: restarted twice. Ten errors: gives up.
        crate::lock(&FAILURES).extend([(flaky.clone(), 2, true), (broken.clone(), 10, false)]);
        let session = Session::start_with_source(
            Config {
                input: Input::Wave {
                    path: String::new(),
                    center_hz: 352603125.0,
                    fast: false,
                },
                channels: configs.clone(),
            },
            |_, _| Ok((Source::Endless(0), 240000.0, 352603125.0)),
        )
        .unwrap();
        let start = Instant::now();
        loop {
            let status = session.status(None, false);
            let find = |id: &str| status.channels.iter().find(|c| c.config.id == id).unwrap();
            let (f, b) = (find(&flaky), find(&broken));
            if f.restarts == 2 && b.error.is_some() {
                assert!(status.running, "{:?}", status.error);
                assert!(f.error.is_none());
                assert_eq!(b.restarts, MAX_RESTARTS as u64);
                // Every other channel kept receiving without a restart.
                assert!(status
                    .channels
                    .iter()
                    .filter(|c| c.config.id != flaky && c.config.id != broken)
                    .all(|c| c.restarts == 0 && c.error.is_none()));
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "restarts {} / {:?}, session {:?}",
                f.restarts,
                b.error,
                status.error
            );
            thread::sleep(Duration::from_millis(10));
        }
        session.stop();
        assert_eq!(session.status(None, false).state, "stopped");
    }

    #[test]
    fn the_gain_optimizer_sweeps_settles_at_the_knee_and_hands_back_on_request() {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let model = applied.clone();
        let channels: Vec<_> = crate::pipeline::presets("east")
            .into_iter()
            .take(2)
            .map(|mut c| {
                c.id = format!("gain-{}", c.id);
                c
            })
            .collect();
        let session = Session::start_with_source(
            Config {
                input: Input::RtlTcp {
                    radio: crate::rtl_tcp::Config {
                        host: "127.0.0.1".into(),
                        port: 1234,
                        sample_rate: 240_000,
                        center_hz: 352_603_125,
                        gain_db: None,
                        ppm: 0,
                        reconnect_seconds: 0,
                        bias_tee: false,
                        gain_optimize: true,
                    },
                },
                channels,
            },
            move |_, _| {
                Ok((
                    Source::GainModel {
                        gain: 49.6,
                        seed: 0x9e37_79b9_7f4a_7c15,
                        applied: model,
                    },
                    240000.0,
                    352603125.0,
                ))
            },
        )
        .unwrap();
        let start = Instant::now();
        let status = loop {
            let status = session.status(None, false);
            if let Some(g) = status.gain.as_ref().filter(|g| g.phase == "tracking") {
                break g.clone();
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "{:?} {:?}",
                status.gain,
                status.error
            );
            thread::sleep(Duration::from_millis(20));
        };
        let set = crate::lock(&applied).clone();
        // Eleven sweep gains from low to high, then the choice: the floor
        // stands 8 dB above the ADC's from about 37 dB.
        assert_eq!(set.len(), 12, "{set:?}");
        assert!(set[..11].windows(2).all(|w| w[0] < w[1]), "{set:?}");
        let chosen = set[11].unwrap();
        assert!((36.0..=41.0).contains(&chosen), "{set:?}");
        assert_eq!(status.gain_db, Some(chosen));
        assert!(status.note.unwrap().contains("雑音の量から"));
        // Turning it off hands the gain back to the source's own automatic
        // gain; a ppm change alone leaves the gain alone.
        session
            .set_control(Control {
                gain_db: Some(None),
                ppm: Some(3),
                bias_tee: Some(false),
                gain_optimize: Some(false),
            })
            .unwrap();
        while session.status(None, false).gain.is_some() {
            assert!(start.elapsed() < Duration::from_secs(60));
            thread::sleep(Duration::from_millis(10));
        }
        session
            .set_control(Control {
                gain_db: Some(None),
                ppm: Some(4),
                bias_tee: Some(false),
                gain_optimize: Some(false),
            })
            .unwrap();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(crate::lock(&applied)[12..], [None]);
        session.stop();
    }

    /// Every USB transfer arrives at once; a channel queue that cannot hold
    /// it drops blocks on each transfer and never synchronises.
    #[cfg(feature = "usb")]
    #[test]
    fn usb_queue_holds_two_transfers() {
        let input = Input::Usb {
            radio: UsbRadio {
                sample_rate: 240_000,
                center_hz: 352_603_125,
                gain_db: None,
                ppm: 0,
                reconnect_seconds: 60,
                bias_tee: false,
                gain_optimize: false,
            },
        };
        let held = queue_capacity(&input) * block_samples(240_000.0);
        assert!(held >= 2 * rtlsdr_nusb::MAX_F32_IQ_SAMPLES_PER_TRANSFER);
    }

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
                        fast: false,
                    },
                    channels: crate::pipeline::presets("east"),
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
            let (session, _release) = gated_wave(&temp.0);
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
            let (session, release) = gated_wave(&temp.0);
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
                input_rate: 0.0,
                iq_request: None,
                iq: None,
                gain: None,
                source_info: None,
                frequency_correction: 0.0,
                invalid_blocks: 0,
                control: None,
                control_error: None,
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
                            restarts: 0,
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
            thread: Mutex::new(None),
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

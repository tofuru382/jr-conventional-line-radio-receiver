//! Decoded PCM and lossless frame/telegram exports, independent of UI polling.
//! The bounded writer queue never blocks DSP on disk I/O. Overrun ends the
//! output explicitly, preserving accepted records and leaving INCOMPLETE.
use crate::{
    pipeline::{Audio, ChannelConfig, FrameEvent},
    wave::PcmWave,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

const QUEUE_FRAMES: usize = 2048;
const SEGMENT_SAMPLES: u64 = 8000 * 60 * 30;
const GAP_SAMPLES: u64 = 8000 * 5;

fn now() -> DateTime<Utc> {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    DateTime::from_timestamp(t.as_secs() as i64, t.subsec_nanos()).unwrap()
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct OutputOptions {
    pub audio: bool,
    pub raw_frames: bool,
    pub data_messages: bool,
    /// Delete WAV files shorter than this (isolated noise bursts). 0 keeps all.
    #[serde(default)]
    pub min_clip_ms: u32,
    /// Keep writing the same WAV across a resynchronization when the previous
    /// audio on that channel arrived within this many milliseconds. 0 splits.
    #[serde(default)]
    pub join_resync_ms: u32,
    /// Put the channel label in WAV names: `352537500Hz-D5_音声第1波-000001.wav`.
    #[serde(default)]
    pub channel_names: bool,
}
impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            audio: true,
            raw_frames: false,
            data_messages: false,
            min_clip_ms: 0,
            join_resync_ms: 0,
            channel_names: false,
        }
    }
}
enum Event {
    Audio(Audio),
    Frame {
        channel: String,
        received_at: String,
        event: FrameEvent,
    },
}
struct Sender {
    options: OutputOptions,
    tx: SyncSender<Event>,
}
#[derive(Clone, Debug, Serialize)]
pub struct RecordingStatus {
    pub state: String,
    pub directory: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub files: u64,
    pub pcm_samples: u64,
    pub raw_frames: u64,
    pub data_messages: u64,
    pub options: OutputOptions,
    pub error: Option<String>,
    /// Nothing was saved (no call, no selected JSONL rows): the session
    /// folder was removed instead of being kept empty.
    pub discarded: bool,
}
impl Default for RecordingStatus {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            directory: None,
            started_at: None,
            finished_at: None,
            files: 0,
            pcm_samples: 0,
            raw_frames: 0,
            data_messages: 0,
            options: OutputOptions::default(),
            error: None,
            discarded: false,
        }
    }
}

#[derive(Default)]
pub struct Recorder {
    // Start/stop serialize here; producers only touch sender, never this lock.
    operation: Mutex<Option<JoinHandle<()>>>,
    sender: Mutex<Option<Sender>>,
    status: Arc<Mutex<RecordingStatus>>,
}
impl Recorder {
    pub fn snapshot(&self) -> RecordingStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn start(
        &self,
        parent: &Path,
        channels: &[ChannelConfig],
    ) -> Result<RecordingStatus, String> {
        self.start_with_options(parent, channels, OutputOptions::default())
    }
    pub fn start_with_options(
        &self,
        parent: &Path,
        channels: &[ChannelConfig],
        options: OutputOptions,
    ) -> Result<RecordingStatus, String> {
        if !options.audio && !options.raw_frames && !options.data_messages {
            return Err("保存する内容を 1 つ以上選択してください。".into());
        }
        let mut operation = self.operation.lock().unwrap();
        if matches!(self.snapshot().state.as_str(), "recording" | "stopping") {
            return Err("すでに保存中です。保存を停止してから開始してください。".into());
        }
        self.sender.lock().unwrap().take();
        if let Some(h) = operation.take() {
            let _ = h.join();
        }
        let mut writer = Writer::create(parent, channels, options)?;
        let initial = RecordingStatus {
            state: "recording".into(),
            options,
            directory: Some(writer.directory.to_string_lossy().into()),
            started_at: Some(writer.started_at.clone()),
            ..RecordingStatus::default()
        };
        *self.status.lock().unwrap() = initial;
        let state = self.status.clone();
        let (tx, rx) = mpsc::sync_channel::<Event>(QUEUE_FRAMES);
        let handle = thread::Builder::new().name("reception-output".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut error = None;
                loop {
                    let event = match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Err(e) = writer.flush_jsonl() { error = Some(e); break; }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let written = writer.event(event);
                    let mut s = state.lock().unwrap();
                    s.files = writer.files;
                    s.pcm_samples = writer.pcm_samples;
                    s.raw_frames = writer.raw_frames;
                    s.data_messages = writer.data_messages;
                    if let Err(e) = written { error = Some(e); break; }
                }
                let previous = state.lock().unwrap().error.clone();
                writer.finish(error.or(previous))
            }));
            let mut s = state.lock().unwrap();
            match result {
                Ok((files, samples, error, discarded)) => {
                    s.files = files;
                    s.pcm_samples = samples;
                    s.error = error.or(s.error.take());
                    s.discarded = discarded && s.error.is_none();
                }
                Err(_) => s.error = Some("保存処理が異常終了しました。保存先の INCOMPLETE と出力ファイル を確認してください。".into()),
            }
            s.state = if s.error.is_some() { "failed" } else { "completed" }.into();
            s.finished_at = Some(now().to_rfc3339());
        }).map_err(|e| {
            let message = format!("保存処理を開始できません：{e}");
            let mut s = self.status.lock().unwrap();
            s.state = "failed".into(); s.error = Some(message.clone());
            message
        })?;
        *operation = Some(handle);
        *self.sender.lock().unwrap() = Some(Sender { options, tx });
        Ok(self.snapshot())
    }

    pub fn push(&self, audio: &Audio) {
        self.send(|o| o.audio.then(|| Event::Audio(audio.clone())));
    }
    pub fn push_frame(&self, channel: &str, event: FrameEvent) {
        self.send(|o| {
            (o.raw_frames || o.data_messages && !event.messages.is_empty()).then(|| Event::Frame {
                channel: channel.into(),
                received_at: now().to_rfc3339(),
                event,
            })
        });
    }
    fn send(&self, make: impl FnOnce(OutputOptions) -> Option<Event>) {
        let mut sender = self.sender.lock().unwrap();
        let Some(sending) = sender.as_ref() else {
            return;
        };
        let Some(event) = make(sending.options) else {
            return;
        };
        if let Err(e) = sending.tx.try_send(event) {
            let message = match e {
                TrySendError::Full(_) => "保存処理が追いつかず保存を終了しました。保存先の速度と空き容量を確認してください。",
                TrySendError::Disconnected(_) => "保存処理が停止しました。保存先とエラー内容を確認してください。",
            };
            let mut s = self.status.lock().unwrap();
            s.error.get_or_insert_with(|| message.into());
            if s.state == "recording" {
                s.state = "stopping".into();
            }
            sender.take();
        }
    }

    pub fn stop(&self) -> RecordingStatus {
        let mut operation = self.operation.lock().unwrap();
        {
            let mut sender = self.sender.lock().unwrap();
            let mut s = self.status.lock().unwrap();
            if s.state == "recording" {
                s.state = "stopping".into();
            }
            sender.take();
        }
        if let Some(h) = operation.take() {
            let _ = h.join();
        }
        self.snapshot()
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Clip {
    wave: PcmWave,
    name: String,
    epoch: u64,
    first_index: u32,
    last_index: u32,
    samples: u64,
}
struct Track {
    config: ChannelConfig,
    number: u64,
    clip: Option<Clip>,
    last: Option<(u64, u32)>,
    /// Wall-clock time of the last audio written on this channel.
    last_at: Option<std::time::Instant>,
}
struct Writer {
    options: OutputOptions,
    directory: PathBuf,
    started_at: String,
    tracks: HashMap<String, Track>,
    index: File,
    files: u64,
    pcm_samples: u64,
    segment_samples: u64,
    raw: Option<BufWriter<File>>,
    messages: Option<BufWriter<File>>,
    raw_frames: u64,
    data_messages: u64,
    last_flush: std::time::Instant,
}
fn create_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
/// A channel label usable in a file name on every OS: letters and digits
/// (any script) are kept, everything else becomes `_`, at most 24 characters.
fn file_label(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        let keep = c.is_alphanumeric();
        if keep {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').chars().take(24).collect()
}
/// Files a session folder holds before anything is recorded.
const SESSION_FILES: [&str; 6] = [
    "INCOMPLETE",
    "session.json",
    "result.json",
    "files.jsonl",
    "raw-frames.jsonl",
    "data-messages.jsonl",
];
/// Removes a `recording-*` folder that recorded nothing: only the session's
/// own bookkeeping files, with empty indexes. Anything else (a WAV, a JSONL
/// row, a file someone put there) keeps the folder.
fn remove_empty_session(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(&known) = SESSION_FILES.iter().find(|n| **n == name) else {
            return false;
        };
        let Ok(meta) = entry.metadata() else {
            return false;
        };
        if !meta.is_file() || known.ends_with(".jsonl") && meta.len() != 0 {
            return false;
        }
        names.push(known);
    }
    names
        .iter()
        .all(|name| fs::remove_file(directory.join(name)).is_ok())
        && fs::remove_dir(directory).is_ok()
}
/// Empty sessions left by an app that was killed before it could stop.
fn remove_empty_sessions(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("recording-")
            && path.is_dir()
        {
            remove_empty_session(&path);
        }
    }
}
impl Writer {
    fn create(
        parent: &Path,
        channels: &[ChannelConfig],
        options: OutputOptions,
    ) -> Result<Self, String> {
        if !parent.is_absolute() || !parent.is_dir() {
            return Err("保存先には存在するフォルダーの絶対パスを指定してください。".into());
        }
        let mut tracks = HashMap::new();
        let mut frequencies = std::collections::HashSet::new();
        if channels.is_empty() || channels.len() > 64 {
            return Err("保存対象は 1–64 波にしてください。".into());
        }
        for c in channels {
            if c.id.is_empty()
                || !c.frequency_hz.is_finite()
                || c.frequency_hz <= 0.0
                || c.frequency_hz > 100_000_000_000.0
                || c.frequency_hz.fract() != 0.0
                || !frequencies.insert(c.frequency_hz as u64)
                || tracks.contains_key(&c.id)
            {
                return Err("保存対象の ID・周波数が不正または重複しています。".into());
            }
            tracks.insert(
                c.id.clone(),
                Track {
                    config: c.clone(),
                    number: 0,
                    clip: None,
                    last: None,
                    last_at: None,
                },
            );
        }
        remove_empty_sessions(parent);
        let instant = now();
        let stem = format!("recording-{}", instant.format("%Y%m%dT%H%M%S%.3fZ"));
        let mut directory = None;
        for n in 0..1000 {
            let candidate = parent.join(format!("{stem}-{n:03}"));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    directory = Some(candidate);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("保存フォルダーを作成できません：{e}")),
            }
        }
        let directory =
            directory.ok_or("保存フォルダー名が重複しました。別の保存先を指定してください。")?;
        // An unfinished session remains identifiable even after a process crash.
        create_json(
            &directory.join("INCOMPLETE"),
            &serde_json::json!({"message":"Recording is in progress or incomplete. WAV headers are finalized on stop."}),
        )?;
        let started_at = instant.to_rfc3339();
        create_json(
            &directory.join("session.json"),
            &serde_json::json!({
                "version":2,"outputs":options,"started_at":started_at,"sample_rate":8000,"bits_per_sample":16,"audio_channels":1,
                "raw_frame_encoding":"Unchanged 48-byte radio envelope, hexadecimal in received byte order (MSB-first packed bits).",
                "received_at":"Host UTC time at decoding; not the transmitter timestamp. JSONL is ordered within each channel, not globally by RF time.",
                "channels":channels,"segment_max_seconds":1800,"gap_max_seconds":5,
                "timeline":"Each file starts at its first decoded PCM frame; long gaps and resynchronization create a new file. Frame indices are local to each channel epoch."
            }),
        )?;
        let index = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("files.jsonl"))
            .map_err(|e| format!("録音索引を作成できません：{e}"))?;
        let jsonl = |name: &str, enabled: bool| -> Result<Option<BufWriter<File>>, String> {
            if !enabled {
                return Ok(None);
            }
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(name))
                .map(|f| Some(BufWriter::new(f)))
                .map_err(|e| format!("{name} を作成できません：{e}"))
        };
        let raw = jsonl("raw-frames.jsonl", options.raw_frames)?;
        let messages = jsonl("data-messages.jsonl", options.data_messages)?;
        Ok(Self {
            options,
            raw,
            messages,
            raw_frames: 0,
            data_messages: 0,
            last_flush: std::time::Instant::now(),
            directory,
            started_at,
            tracks,
            index,
            files: 0,
            pcm_samples: 0,
            segment_samples: SEGMENT_SAMPLES,
        })
    }
    fn event(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::Audio(audio) => self.audio(&audio)?,
            Event::Frame {
                channel,
                received_at,
                event,
            } => {
                let track = self
                    .tracks
                    .get(&channel)
                    .ok_or("保存対象外のフレームを受信しました。")?;
                let base = serde_json::json!({
                    "schema_version":1,"received_at":received_at,"channel_id":channel,
                    "frequency_hz":track.config.frequency_hz,"label":track.config.label,"format":track.config.format,
                    "epoch":event.epoch,"index":event.index,"bit_offset":event.frame.bit_offset
                });
                if let Some(file) = &mut self.raw {
                    let mut row = base.clone();
                    row["raw_hex"] = event
                        .frame
                        .received
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<String>()
                        .into();
                    row["sync_matched"] = event.frame.sync_matched.into();
                    row["marker_errors"] = event.frame.marker_errors.into();
                    row["voice_accepted"] = event.corrected_bits.is_some().into();
                    row["corrected_bits"] = event.corrected_bits.into();
                    Self::line(file, &row)?;
                    self.raw_frames += 1;
                }
                if let Some(file) = &mut self.messages {
                    for message in event.messages {
                        let mut row = base.clone();
                        row["message"] =
                            serde_json::to_value(message).map_err(|e| e.to_string())?;
                        Self::line(file, &row)?;
                        self.data_messages += 1;
                    }
                }
            }
        }
        if self.last_flush.elapsed() >= std::time::Duration::from_millis(500) {
            self.flush_jsonl()?;
        }
        Ok(())
    }
    fn flush_jsonl(&mut self) -> Result<(), String> {
        for file in [&mut self.raw, &mut self.messages].into_iter().flatten() {
            file.flush()
                .map_err(|e| format!("JSONL を保存できません。空き容量を確認してください：{e}"))?;
        }
        self.last_flush = std::time::Instant::now();
        Ok(())
    }
    fn line(file: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
        serde_json::to_writer(&mut *file, value).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())
    }
    /// Finalizes the track's WAV. Returns the samples of a clip deleted for
    /// being shorter than `min_samples` (0 when kept).
    fn close(
        track: &mut Track,
        index: &mut File,
        directory: &Path,
        min_samples: u64,
    ) -> Result<u64, String> {
        if let Some(clip) = track.clip.take() {
            let samples = clip.wave.samples();
            clip.wave
                .finish()
                .map_err(|e| format!("{} の WAV を確定できません：{e}", clip.name))?;
            if samples < min_samples {
                fs::remove_file(directory.join(&clip.name))
                    .map_err(|e| format!("{} を削除できません：{e}", clip.name))?;
                // Reuse the number so files stay consecutive.
                track.number -= 1;
                return Ok(samples);
            }
            serde_json::to_writer(&mut *index,&serde_json::json!({
                "file":clip.name,"channel_id":track.config.id,"frequency_hz":track.config.frequency_hz,
                "label":track.config.label,"epoch":clip.epoch,"first_index":clip.first_index,"last_index":clip.last_index,"samples":samples
            })).map_err(|e|e.to_string())?;
            index.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        Ok(0)
    }
    fn close_track(&mut self, id: &str) -> Result<(), String> {
        let min = u64::from(self.options.min_clip_ms) * 8;
        let track = self.tracks.get_mut(id).unwrap();
        let dropped = Self::close(track, &mut self.index, &self.directory, min)?;
        if dropped > 0 {
            self.files -= 1;
            self.pcm_samples -= dropped;
        }
        Ok(())
    }
    fn audio(&mut self, audio: &Audio) -> Result<(), String> {
        if audio.pcm.len() != 320 {
            return Err("録音用 PCM フレームの長さが不正です。".into());
        }
        let track = self
            .tracks
            .get_mut(&audio.channel)
            .ok_or("保存対象外の音声を受信しました。")?;
        if track.last.is_some_and(|(epoch, index)| {
            audio.epoch < epoch || audio.epoch == epoch && audio.index <= index
        }) {
            return Err("音声フレームの順序が不正なため録音を終了しました。".into());
        }
        let gap = track
            .last
            .filter(|(epoch, _)| *epoch == audio.epoch)
            .map_or(0, |(_, index)| (audio.index - index - 1) as u64 * 320);
        // A resynchronization in the middle of a call keeps the same file
        // when the channel was heard moments ago (weak signals resync often).
        let join = track.last_at.is_some_and(|t| {
            t.elapsed() <= std::time::Duration::from_millis(self.options.join_resync_ms.into())
        });
        // Compare with the last frame written, not the clip's first epoch: a
        // joined clip continues in the new epoch.
        let resync = track.last.is_some_and(|(epoch, _)| epoch != audio.epoch);
        let split = track.clip.as_ref().is_some_and(|c| {
            resync && !join || gap > GAP_SAMPLES || c.samples + gap + 320 > self.segment_samples
        });
        if split {
            let id = audio.channel.clone();
            self.close_track(&id)?;
        }
        let track = self.tracks.get_mut(&audio.channel).unwrap();
        if track.clip.is_none() {
            track.number += 1;
            // Only validated integer frequency, a sanitized label and our own
            // counter enter paths.
            let label = if self.options.channel_names {
                file_label(&track.config.label)
            } else {
                String::new()
            };
            let name = if label.is_empty() {
                format!(
                    "{}Hz-{:06}.wav",
                    track.config.frequency_hz as u64, track.number
                )
            } else {
                format!(
                    "{}Hz-{}-{:06}.wav",
                    track.config.frequency_hz as u64, label, track.number
                )
            };
            let wave = PcmWave::create(self.directory.join(&name))
                .map_err(|e| format!("{name} を作成できません：{e}"))?;
            track.clip = Some(Clip {
                wave,
                name,
                epoch: audio.epoch,
                first_index: audio.index,
                last_index: audio.index,
                samples: 0,
            });
            self.files += 1;
        }
        let clip = track.clip.as_mut().unwrap();
        if clip.samples > 0 && gap > 0 {
            clip.wave
                .silence(gap)
                .map_err(|e| format!("録音の無音区間を書き込めません：{e}"))?;
            clip.samples += gap;
        }
        clip.wave.write(&audio.pcm).map_err(|e| {
            format!("音声を書き込めません。保存先の空き容量を確認してください：{e}")
        })?;
        clip.samples += 320;
        clip.last_index = audio.index;
        track.last = Some((audio.epoch, audio.index));
        track.last_at = Some(std::time::Instant::now());
        self.pcm_samples += 320;
        Ok(())
    }
    fn finish(mut self, mut error: Option<String>) -> (u64, u64, Option<String>, bool) {
        let ids: Vec<String> = self.tracks.keys().cloned().collect();
        for id in ids {
            if let Err(e) = self.close_track(&id) {
                error.get_or_insert(e);
            }
        }
        if let Err(e) = self.index.sync_all() {
            error.get_or_insert(e.to_string());
        }
        for file in [&mut self.raw, &mut self.messages].into_iter().flatten() {
            if let Err(e) = file.flush().and_then(|_| file.get_ref().sync_all()) {
                error.get_or_insert(e.to_string());
            }
        }
        let result = serde_json::json!({"raw_frames":self.raw_frames,"data_messages":self.data_messages,"finished_at":now().to_rfc3339(),"files":self.files,"pcm_samples":self.pcm_samples,"error":error});
        if let Err(e) = create_json(&self.directory.join("result.json"), &result) {
            error.get_or_insert(e);
        }
        if error.is_none() {
            if let Err(e) = fs::remove_file(self.directory.join("INCOMPLETE")) {
                error = Some(e.to_string());
            }
        }
        let (files, samples) = (self.files, self.pcm_samples);
        let empty =
            error.is_none() && files == 0 && self.raw_frames == 0 && self.data_messages == 0;
        let directory = self.directory.clone();
        // Close every handle first (Windows cannot remove open files).
        drop(self);
        let discarded = empty && remove_empty_session(&directory);
        (files, samples, error, discarded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{frame::Frame, pipeline::presets};
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "jr-recording-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn pcm(c: &ChannelConfig, epoch: u64, index: u32, value: i16) -> Audio {
        Audio {
            channel: c.id.clone(),
            epoch,
            index,
            pcm: vec![value; 320],
        }
    }
    fn read_pcm(path: &Path) -> Vec<i16> {
        let b = fs::read(path).unwrap();
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize,
            b.len() - 8
        );
        assert_eq!(u16::from_le_bytes(b[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 8000);
        assert_eq!(u16::from_le_bytes(b[34..36].try_into().unwrap()), 16);
        assert_eq!(
            u32::from_le_bytes(b[40..44].try_into().unwrap()) as usize,
            b.len() - 44
        );
        b[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| i16::from_le_bytes(*p))
            .collect()
    }
    #[test]
    fn multichannel_pcm_is_exact_with_gaps_and_resynchronization_in_separate_files() {
        let temp = Temp::new();
        let channels = presets("east")[..2].to_vec();
        let r = Recorder::default();
        let status = r.start(&temp.0, &channels).unwrap();
        let dir = PathBuf::from(status.directory.unwrap());
        assert!(r.start(&temp.0, &channels).is_err());
        r.push(&pcm(&channels[0], 0, 10, 120));
        r.push(&pcm(&channels[1], 0, 20, -240));
        r.push(&pcm(&channels[0], 0, 12, 360)); // one missing frame is silence
        r.push(&pcm(&channels[0], 1, 0, 480)); // epoch reset starts another WAV
        let s = r.stop();
        assert_eq!(s.state, "completed");
        assert_eq!(s.files, 3);
        assert_eq!(s.pcm_samples, 1280);
        assert_eq!(
            read_pcm(&dir.join("352537500Hz-000001.wav")),
            [vec![120; 320], vec![0; 320], vec![360; 320]].concat()
        );
        assert_eq!(
            read_pcm(&dir.join("352543750Hz-000001.wav")),
            vec![-240; 320]
        );
        assert_eq!(
            read_pcm(&dir.join("352537500Hz-000002.wav")),
            vec![480; 320]
        );
        assert!(!dir.join("INCOMPLETE").exists());
        assert_eq!(
            fs::read_to_string(dir.join("files.jsonl"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        let next = r.start(&temp.0, &channels).unwrap();
        assert_ne!(next.directory.as_deref(), s.directory.as_deref());
        let no_audio = r.stop();
        assert_eq!(no_audio.files, 0);
        assert_eq!(no_audio.state, "completed");
        assert_eq!(
            read_pcm(&dir.join("352543750Hz-000001.wav")),
            vec![-240; 320]
        );
    }
    #[test]
    fn channel_labels_go_into_wav_names_safely() {
        assert_eq!(file_label("D5 音声第1波"), "D5_音声第1波");
        assert_eq!(file_label("../a/b\\c:*?"), "a_b_c");
        assert_eq!(file_label("  "), "");
        assert_eq!(file_label(&"長".repeat(40)).chars().count(), 24);
        let temp = Temp::new();
        let channels = presets("east")[..1].to_vec();
        let r = Recorder::default();
        let options = OutputOptions {
            channel_names: true,
            ..Default::default()
        };
        let dir = PathBuf::from(
            r.start_with_options(&temp.0, &channels, options)
                .unwrap()
                .directory
                .unwrap(),
        );
        r.push(&pcm(&channels[0], 0, 0, 5));
        assert_eq!(r.stop().files, 1);
        let name = format!(
            "{}Hz-{}-000001.wav",
            channels[0].frequency_hz as u64,
            file_label(&channels[0].label)
        );
        assert_eq!(read_pcm(&dir.join(&name)), vec![5; 320]);
    }
    #[test]
    fn resyncs_in_a_call_join_one_file_and_short_noise_bursts_are_dropped() {
        let temp = Temp::new();
        let channels = presets("east")[..2].to_vec();
        let r = Recorder::default();
        let options = OutputOptions {
            min_clip_ms: 1000,
            join_resync_ms: 2000,
            ..Default::default()
        };
        let dir = PathBuf::from(
            r.start_with_options(&temp.0, &channels, options)
                .unwrap()
                .directory
                .unwrap(),
        );
        // Channel 0: a 1.2 s call broken by two resynchronizations.
        for (epoch, value) in [(0u64, 1i16), (1, 2), (2, 3)] {
            for index in 0..10 {
                r.push(&pcm(&channels[0], epoch, index, value));
            }
        }
        // Channel 1: a lone 80 ms burst (noise), then a 1.2 s call after a
        // long silence that must start its own numbering at 1.
        r.push(&pcm(&channels[1], 0, 0, 9));
        r.push(&pcm(&channels[1], 0, 1, 9));
        for index in 0..30 {
            r.push(&pcm(&channels[1], 0, 500 + index, 5));
        }
        let s = r.stop();
        assert_eq!(s.state, "completed", "{:?}", s.error);
        assert_eq!(s.files, 2);
        assert_eq!(s.pcm_samples, 2 * 30 * 320);
        assert_eq!(
            read_pcm(&dir.join("352537500Hz-000001.wav")),
            [vec![1; 3200], vec![2; 3200], vec![3; 3200]].concat()
        );
        assert_eq!(read_pcm(&dir.join("352543750Hz-000001.wav")), vec![5; 9600]);
        assert!(!dir.join("352543750Hz-000002.wav").exists());
        let index = fs::read_to_string(dir.join("files.jsonl")).unwrap();
        assert_eq!(index.lines().count(), 2);
    }
    #[test]
    fn a_session_of_only_noise_bursts_saves_nothing() {
        let temp = Temp::new();
        let channels = presets("east")[..1].to_vec();
        let r = Recorder::default();
        let options = OutputOptions {
            min_clip_ms: 1000,
            join_resync_ms: 2000,
            ..Default::default()
        };
        let dir = PathBuf::from(
            r.start_with_options(&temp.0, &channels, options)
                .unwrap()
                .directory
                .unwrap(),
        );
        r.push(&pcm(&channels[0], 0, 0, 9));
        let s = r.stop();
        assert_eq!((s.files, s.pcm_samples), (0, 0));
        assert!(s.discarded);
        assert!(!dir.exists());
    }
    #[test]
    fn long_gaps_and_size_limit_rotate_before_wav_overflow_and_ids_cannot_escape_directory() {
        let temp = Temp::new();
        let mut channels = presets("east")[..1].to_vec();
        channels[0].id = "../../escaped".into();
        let mut w = Writer::create(&temp.0, &channels, OutputOptions::default()).unwrap();
        let dir = w.directory.clone();
        w.segment_samples = 640;
        for (index, value) in [(0, 1), (1, 2), (2, 3), (10000, 4)] {
            w.audio(&pcm(&channels[0], 0, index, value)).unwrap();
        }
        let (files, samples, error, _) = w.finish(None);
        assert!(error.is_none());
        assert_eq!((files, samples), (3, 1280));
        assert_eq!(
            read_pcm(&dir.join("352537500Hz-000001.wav")),
            [vec![1; 320], vec![2; 320]].concat()
        );
        assert_eq!(read_pcm(&dir.join("352537500Hz-000002.wav")), vec![3; 320]);
        assert_eq!(read_pcm(&dir.join("352537500Hz-000003.wav")), vec![4; 320]);
    }
    #[test]
    fn failed_file_creation_keeps_existing_data_and_marks_recording_incomplete() {
        let temp = Temp::new();
        let channels = presets("east")[..1].to_vec();
        let r = Recorder::default();
        assert!(r.start(&temp.0.join("missing"), &channels).is_err());
        assert_eq!(r.snapshot().state, "idle");
        let dir = PathBuf::from(r.start(&temp.0, &channels).unwrap().directory.unwrap());
        // Simulates a destination that cannot be created, without relying on
        // platform permissions or actually filling the user's disk.
        let file = dir.join("352537500Hz-000001.wav");
        fs::write(&file, b"keep existing file").unwrap();
        r.push(&pcm(&channels[0], 0, 0, 1));
        let s = r.stop();
        assert_eq!(s.state, "failed");
        assert!(s.error.is_some());
        assert_eq!(s.files, 0);
        assert_eq!(fs::read(file).unwrap(), b"keep existing file");
        assert!(dir.join("INCOMPLETE").exists());
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("result.json")).unwrap()).unwrap();
        assert!(result["error"].is_string());
    }
    #[test]
    fn writer_overrun_disconnects_without_blocking_or_silently_claiming_success() {
        let r = Recorder::default();
        let (tx, rx) = mpsc::sync_channel(1);
        *r.sender.lock().unwrap() = Some(Sender {
            options: OutputOptions::default(),
            tx,
        });
        r.status.lock().unwrap().state = "recording".into();
        let c = presets("east")[0].clone();
        r.push(&pcm(&c, 0, 0, 1));
        r.push(&pcm(&c, 0, 1, 2));
        let s = r.snapshot();
        assert_eq!(s.state, "stopping");
        assert!(s.error.unwrap().contains("追いつかず"));
        assert!(r.sender.lock().unwrap().is_none());
        assert!(matches!(
            rx.recv().unwrap(),
            Event::Audio(Audio { index: 0, .. })
        ));
        assert!(rx.recv().is_err());
    }
    #[test]
    fn drop_finalizes_audio_and_invalid_order_preserves_the_accepted_pcm() {
        let temp = Temp::new();
        let channels = presets("east")[..1].to_vec();
        let dir = {
            let r = Recorder::default();
            let s = r.start(&temp.0, &channels).unwrap();
            r.push(&pcm(&channels[0], 0, 0, 5));
            PathBuf::from(s.directory.unwrap())
        };
        assert_eq!(read_pcm(&dir.join("352537500Hz-000001.wav")), vec![5; 320]);
        assert!(!dir.join("INCOMPLETE").exists());
        let r = Recorder::default();
        let dir = PathBuf::from(r.start(&temp.0, &channels).unwrap().directory.unwrap());
        r.push(&pcm(&channels[0], 0, 1, 7));
        r.push(&pcm(&channels[0], 0, 1, 8));
        assert_eq!(r.stop().state, "failed");
        assert_eq!(read_pcm(&dir.join("352537500Hz-000001.wav")), vec![7; 320]);
        assert!(dir.join("INCOMPLETE").exists());
    }
    #[test]
    fn output_choices_are_independent_and_empty_selection_does_not_create_a_session() {
        let temp = Temp::new();
        let configs = presets("east")[..1].to_vec();
        let r = Recorder::default();
        assert!(r
            .start_with_options(
                &temp.0,
                &configs,
                OutputOptions {
                    audio: false,
                    raw_frames: false,
                    data_messages: false,
                    ..Default::default()
                }
            )
            .is_err());
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
        for options in [
            OutputOptions::default(),
            OutputOptions {
                audio: false,
                raw_frames: true,
                data_messages: false,
                ..Default::default()
            },
            OutputOptions {
                audio: false,
                raw_frames: false,
                data_messages: true,
                ..Default::default()
            },
        ] {
            let dir = PathBuf::from(
                r.start_with_options(&temp.0, &configs, options)
                    .unwrap()
                    .directory
                    .unwrap(),
            );
            assert_eq!(dir.join("raw-frames.jsonl").exists(), options.raw_frames);
            assert_eq!(
                dir.join("data-messages.jsonl").exists(),
                options.data_messages
            );
            // Nothing arrived: the session leaves no empty folder behind.
            let status = r.stop();
            assert!(status.discarded);
            assert!(status.error.is_none());
            assert!(!dir.exists());
        }
    }
    #[test]
    fn sessions_with_a_call_are_kept_and_leftover_empty_ones_are_removed() {
        let temp = Temp::new();
        let configs = presets("east")[..1].to_vec();
        let r = Recorder::default();
        let leftover = PathBuf::from(r.start(&temp.0, &configs).unwrap().directory.unwrap());
        // A killed app never runs stop(): simulate the folder it leaves.
        fs::write(leftover.join("result.json"), "{}").unwrap();
        let foreign = temp.0.join("recording-foreign");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("note.txt"), "keep").unwrap();
        r.stop();
        fs::create_dir(&leftover).unwrap_or(());
        for name in ["INCOMPLETE", "session.json", "files.jsonl"] {
            fs::write(leftover.join(name), "").unwrap();
        }
        let kept = PathBuf::from(r.start(&temp.0, &configs).unwrap().directory.unwrap());
        assert!(!leftover.exists(), "empty leftover session was not removed");
        assert!(foreign.join("note.txt").exists());
        r.push(&pcm(&configs[0], 0, 0, 7));
        let status = r.stop();
        assert!(!status.discarded);
        assert_eq!(status.files, 1);
        assert!(kept.join("result.json").exists());
    }
    #[test]
    fn operation_summary_is_exported_with_nulls_and_original_bytes() {
        let temp = Temp::new();
        let configs = presets("east");
        let common = configs
            .iter()
            .find(|c| c.format == crate::data::Format::Common)
            .unwrap();
        let mut writer = Writer::create(
            &temp.0,
            &configs,
            OutputOptions {
                audio: false,
                raw_frames: false,
                data_messages: true,
                ..Default::default()
            },
        )
        .unwrap();
        let dir = writer.directory.clone();
        let mut message = crate::data::synthetic_operation();
        message.operation_summary.as_mut().unwrap().rows[0].on_line = None;
        let expected = serde_json::to_value(&message).unwrap();
        writer
            .event(Event::Frame {
                channel: common.id.clone(),
                received_at: "2025-03-14T00:27:00Z".into(),
                event: FrameEvent {
                    epoch: 0,
                    index: 0,
                    frame: Frame {
                        bit_offset: 0,
                        sync_matched: true,
                        marker_errors: 0,
                        received: [0; 48],
                    },
                    corrected_bits: None,
                    messages: vec![message],
                },
            })
            .unwrap();
        assert!(writer.finish(None).2.is_none());
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("data-messages.jsonl")).unwrap())
                .unwrap();
        assert_eq!(json["message"], expected);
        assert!(json["message"]["operation_summary"]["rows"][0]["on_line"].is_null());
        assert_eq!(json["received_at"], "2025-03-14T00:27:00Z");
        assert!(json["message"]["timestamp"].is_null());
    }

    #[test]
    fn raw_rejected_envelopes_are_preserved_and_flush_errors_leave_incomplete() {
        let temp = Temp::new();
        let configs = presets("east")[..1].to_vec();
        let mut writer = Writer::create(
            &temp.0,
            &configs,
            OutputOptions {
                audio: false,
                raw_frames: true,
                data_messages: true,
                ..Default::default()
            },
        )
        .unwrap();
        let dir = writer.directory.clone();
        writer
            .event(Event::Frame {
                channel: configs[0].id.clone(),
                received_at: now().to_rfc3339(),
                event: FrameEvent {
                    epoch: 2,
                    index: 5,
                    frame: Frame {
                        bit_offset: 384 * 5,
                        sync_matched: false,
                        marker_errors: 48,
                        received: [0; 48],
                    },
                    corrected_bits: None,
                    messages: vec![],
                },
            })
            .unwrap();
        writer.flush_jsonl().unwrap();
        let row: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("raw-frames.jsonl")).unwrap())
                .unwrap();
        assert_eq!(row["raw_hex"], "00".repeat(48));
        assert_eq!(row["sync_matched"], false);
        assert_eq!(row["voice_accepted"], false);
        assert_eq!(row["epoch"], 2);
        assert_eq!(fs::read(dir.join("data-messages.jsonl")).unwrap().len(), 0);
        // An unwritable handle deterministically exercises a buffered write
        // failure on every platform, without filling the disk.
        let mut unwritable = BufWriter::new(File::open(dir.join("session.json")).unwrap());
        unwritable.write_all(b"pending row\n").unwrap();
        writer.raw = Some(unwritable);
        let (_, _, error, _) = writer.finish(None);
        assert!(error.is_some());
        assert!(dir.join("INCOMPLETE").exists());
    }
}

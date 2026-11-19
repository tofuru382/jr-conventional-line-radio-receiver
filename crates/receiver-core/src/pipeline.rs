pub use crate::frame::Detector;
use crate::{
    data::{Decoder, Format, Message, OperationReception, Stats},
    frame::{Decodes, Frame, Merger, Synchronizer},
    voice_frame, Complex32,
};
use jr_conventional_dsp::receiver::Dsp;
use jr_conventional_voice::receiver::Voice;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Seconds the alternate detector keeps running after the last frame lock:
/// a call's pauses and fades are shorter, idle channels pay for the primary
/// detector alone.
const ALTERNATE_HOLD_SECONDS: f64 = 3.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String,
    pub label: String,
    pub frequency_hz: f64,
    pub format: Format,
}
#[derive(Clone, Debug, Serialize)]
pub struct Audio {
    pub channel: String,
    pub epoch: u64,
    pub index: u32,
    pub pcm: Vec<i16>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Received {
    pub message: Message,
    pub receptions: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub iq_samples: u64,
    pub frames: u64,
    pub synchronized_frames: u64,
    pub voice_frames: u64,
    pub rejected_voice: u64,
    pub corrected_bits: u64,
    pub audio_frames: u64,
    pub discontinuities: u64,
    pub frame_locked: bool,
    /// Frames taken from the alternate (fast-tracking) detector because only
    /// it decoded at that position.
    pub alternate_frames: u64,
    /// Frames that decoded only after combining both detectors bit by bit.
    pub combined_frames: u64,
    /// Whether the alternate detector runs (it pauses on idle channels).
    pub alternate_running: bool,
    /// Power after the channel filter (dB re full-scale IQ): this channel's
    /// own signal level, unlike `input_dbfs`, which covers the whole band.
    pub channel_dbfs: f64,
    /// Automatic frequency correction in effect for `frequency_error_hz`,
    /// and whether the carrier loop has settled since it last changed.
    pub applied_correction_hz: f64,
    pub correction_settled: bool,
    pub carrier_locked: bool,
    pub frequency_error_hz: f64,
    pub evm_percent: f64,
    pub input_dbfs: f64,
    pub data: Stats,
    pub constellation: Vec<[f32; 2]>,
    pub messages: Vec<Received>,
    pub operation_monitor: Option<OperationReception>,
    pub epoch: u64,
}
pub struct FrameEvent {
    pub epoch: u64,
    pub index: u32,
    pub frame: Frame,
    pub corrected_bits: Option<u16>,
    pub messages: Vec<Message>,
    /// The accepted CELP payload, when the frame decoded as voice.
    pub speech: Option<[u8; 20]>,
    /// The 28 information bytes, when the frame decoded as data.
    pub data: Option<[u8; 28]>,
    /// Which detector's decisions the frame was taken from.
    pub detector: Detector,
    /// When the frame's last sample was received, from the capture time of
    /// its IQ block (see [`BlockInfo`]); `None` without a capture clock.
    pub received_at: Option<std::time::SystemTime>,
}
/// What the receiver session knows about an IQ block before the channels
/// process it, computed once for all of them.
#[derive(Clone, Copy, Default)]
pub struct BlockInfo {
    /// Mean power of the whole (wideband) block in dBFS.
    pub dbfs: Option<f64>,
    /// When the block's first sample was received.
    pub captured: Option<std::time::SystemTime>,
}
/// Frame decoded once and reused by the merger, the voice decoder, the data
/// decoder and the recording.
#[derive(Default)]
pub struct Decoded {
    pub speech: Option<voice_frame::Speech>,
    pub data: Option<([u8; 28], usize)>,
}
impl Decodes for Decoded {
    fn ok(&self) -> bool {
        self.speech.is_some() || self.data.is_some()
    }
}
impl Decoded {
    /// Cheap hard-decision decoders first, so a clean frame of either kind
    /// never pays for the other kind's soft-decision retries.
    pub fn of(frame: &Frame) -> Self {
        if let Some(speech) = voice_frame::decode(&frame.received) {
            return Self {
                speech: Some(speech),
                data: None,
            };
        }
        if let Some(data) = crate::data::decode_frame(&frame.received) {
            return Self {
                speech: None,
                data: Some(data),
            };
        }
        if let Some(speech) = voice_frame::decode_soft_only(&frame.received, &frame.confidence) {
            return Self {
                speech: Some(speech),
                data: None,
            };
        }
        Self {
            speech: None,
            data: crate::data::decode_frame_soft_only(&frame.received, &frame.confidence),
        }
    }
}
pub struct Pipeline {
    pub config: ChannelConfig,
    rate: f64,
    center: f64,
    // JR-CLRR: automatic frequency correction (Hz) added to the mixer.
    tuning: f64,
    dsp: Dsp,
    sync: Synchronizer,
    alternate_sync: Synchronizer,
    merger: Merger,
    voice: Voice,
    data: Decoder,
    dibits: [Vec<u8>; 2],
    soft: [Vec<[u8; 2]>; 2],
    origin: Option<u64>,
    last_offset: Option<u64>,
    last_speech: Option<u64>,
    segment_samples: u64,
    // Segment sample count until which the alternate detector keeps running
    // after the last frame lock.
    alternate_until: u64,
    // The session checked the IQ for non-finite samples (see trust_input).
    input_checked: bool,
    // iq_samples when the correction last changed.
    retuned_at: u64,
    // Capture time of the current segment's sample 0, when known.
    segment_time: Option<std::time::SystemTime>,
    // Changes of `history` since the last snapshot.
    history_revision: u64,
    snapshot_revision: u64,
    history: VecDeque<Received>,
    events: VecDeque<Message>,
    pub status: Status,
}
impl Pipeline {
    pub fn new(config: ChannelConfig, rate: f64, center: f64) -> Result<Self, String> {
        if !center.is_finite() || !config.frequency_hz.is_finite() || config.frequency_hz <= 0.0 {
            return Err("Invalid channel frequency".into());
        }
        Ok(Self {
            dsp: Self::demodulator(rate, config.frequency_hz - center)?,
            sync: Self::synchronizer(),
            alternate_sync: Self::synchronizer(),
            merger: Merger::default(),
            voice: Voice::default(),
            data: Decoder::new(config.format),
            config,
            rate,
            center,
            tuning: 0.0,
            dibits: [Vec::new(), Vec::new()],
            soft: [Vec::new(), Vec::new()],
            origin: None,
            last_offset: None,
            last_speech: None,
            segment_samples: 0,
            alternate_until: 0,
            input_checked: false,
            retuned_at: 0,
            segment_time: None,
            history_revision: 0,
            snapshot_revision: u64::MAX,
            history: VecDeque::new(),
            events: VecDeque::new(),
            status: Status {
                input_dbfs: -120.0,
                channel_dbfs: -120.0,
                correction_settled: true,
                ..Status::default()
            },
        })
    }
    /// The caller checks every IQ block for non-finite samples before it
    /// reaches this pipeline (once for all channels), so the DSP skips it.
    pub fn trust_input(&mut self) {
        self.input_checked = true;
    }
    fn synchronizer() -> Synchronizer {
        Synchronizer::with_validator(|frame| Decoded::of(frame).ok())
    }
    // Two detectors: LMS tracking, and drift RLS tracking for simulcast paths
    // fading at train speed. Their frames merge by position (see Merger).
    fn demodulator(rate: f64, offset: f64) -> Result<Dsp, String> {
        let mut dsp = Dsp::new(rate, offset)?;
        dsp.enable_alternate();
        Ok(dsp)
    }
    /// JR-CLRR: shifts the channel by `tuning` Hz (the session's automatic
    /// frequency correction) while it keeps receiving.
    pub fn retune(&mut self, tuning: f64) -> Result<(), String> {
        self.dsp
            .retune(self.rate, self.config.frequency_hz - self.center + tuning)?;
        self.tuning = tuning;
        self.retuned_at = self.status.iq_samples;
        self.status.applied_correction_hz = tuning;
        self.status.correction_settled = false;
        Ok(())
    }
    pub fn discontinuity(&mut self) -> Result<(), String> {
        self.dsp = Self::demodulator(
            self.rate,
            self.config.frequency_hz - self.center + self.tuning,
        )?;
        self.sync = Self::synchronizer();
        self.alternate_sync = Self::synchronizer();
        self.merger.clear();
        self.voice = Voice::default();
        self.data.reset_assembly();
        self.origin = None;
        self.last_offset = None;
        self.last_speech = None;
        self.segment_samples = 0;
        self.alternate_until = 0;
        self.segment_time = None;
        self.history.clear();
        self.history_revision += 1;
        self.events.clear();
        self.status.epoch += 1;
        self.status.discontinuities += 1;
        self.status.frame_locked = false;
        self.status.carrier_locked = false;
        self.status.constellation.clear();
        self.status.messages.clear();
        Ok(())
    }
    pub fn process(&mut self, iq: &[Complex32]) -> Result<Vec<Audio>, String> {
        self.process_observed(iq, |_| {})
    }
    pub fn process_observed(
        &mut self,
        iq: &[Complex32],
        observe: impl FnMut(FrameEvent),
    ) -> Result<Vec<Audio>, String> {
        self.process_block(iq, BlockInfo::default(), observe)
    }
    /// [`Self::process_observed`] with what the session computed once for
    /// all channels: the block's wideband power and its capture time.
    pub fn process_block(
        &mut self,
        iq: &[Complex32],
        info: BlockInfo,
        mut observe: impl FnMut(FrameEvent),
    ) -> Result<Vec<Audio>, String> {
        if let Some(captured) = info.captured {
            let before =
                std::time::Duration::from_secs_f64(self.segment_samples as f64 / self.rate);
            self.segment_time = captured.checked_sub(before);
        }
        self.status.iq_samples += iq.len() as u64;
        // The alternate detector costs twice the primary one; run it only
        // around signals (see `alternate_until`).
        self.dsp
            .set_alternate_active(self.segment_samples < self.alternate_until);
        self.segment_samples += iq.len() as u64;
        let snap = if self.input_checked {
            self.dsp
                .process_dual_finite(iq, &mut self.dibits, &mut self.soft)
        } else {
            self.dsp
                .process_dual(iq, &mut self.dibits, &mut self.soft)?
        };
        if let Some(level) = self.dsp.take_channel_power_db() {
            self.status.channel_dbfs = level;
        }
        // The carrier loop follows a correction step within a few hundred
        // symbols; half a second leaves a wide margin.
        if !self.status.correction_settled
            && self.status.iq_samples.saturating_sub(self.retuned_at) as f64 >= 0.5 * self.rate
        {
            self.status.correction_settled = true;
        }
        for (stream, sync) in [&mut self.sync, &mut self.alternate_sync]
            .into_iter()
            .enumerate()
        {
            let mut frames = Vec::new();
            for (&dibit, &soft) in self.dibits[stream].iter().zip(&self.soft[stream]) {
                sync.push_soft(dibit, soft, &mut frames);
            }
            for frame in frames {
                self.merger.push(stream, frame);
            }
        }
        let settled = [self.sync.settled(), self.alternate_sync.settled()];
        let frames = self.merger.release(settled, false, Decoded::of);
        // Frame lock, not carrier lock: the sequence detector's carrier loop
        // reports lock on plain noise as well.
        if self.sync.locked || self.alternate_sync.locked {
            self.alternate_until =
                self.segment_samples + (ALTERNATE_HOLD_SECONDS * self.rate) as u64;
        }
        self.status.alternate_running = self.dsp.alternate_running();
        self.status.carrier_locked = snap.carrier_locked != 0;
        self.status.frequency_error_hz = snap.frequency_offset_hz;
        self.status.frame_locked = self.sync.locked || self.alternate_sync.locked;
        if let Some(dbfs) = info.dbfs {
            self.status.input_dbfs = dbfs;
        } else if !iq.is_empty() {
            self.status.input_dbfs = block_dbfs(iq);
        }
        let mut audio = Vec::new();
        for (frame, detector, decoded) in frames {
            audio.extend(self.frame_decoded(frame, detector, decoded, &mut observe)?);
        }
        // Release synthesis lookahead even when the RF carrier disappears.
        audio.extend(self.flush_idle((self.segment_samples as f64 / self.rate * 9600.0) as u64)?);
        Ok(audio)
    }
    pub fn frame(&mut self, frame: &Frame) -> Result<Vec<Audio>, String> {
        self.frame_observed(frame, &mut |_| {})
    }
    pub fn frame_observed(
        &mut self,
        frame: &Frame,
        observe: &mut impl FnMut(FrameEvent),
    ) -> Result<Vec<Audio>, String> {
        let decoded = Decoded::of(frame);
        self.frame_decoded(frame.clone(), Detector::Primary, decoded, observe)
    }
    fn frame_decoded(
        &mut self,
        frame: Frame,
        detector: Detector,
        decoded: Decoded,
        observe: &mut impl FnMut(FrameEvent),
    ) -> Result<Vec<Audio>, String> {
        match detector {
            Detector::Primary => {}
            Detector::Alternate => self.status.alternate_frames += 1,
            Detector::Combined => self.status.combined_frames += 1,
        }
        let new_stream = self.origin.is_some_and(|o| {
            frame.bit_offset < o
                || !(frame.bit_offset - o).is_multiple_of(384)
                || (frame.bit_offset - o) / 384 > u32::MAX as u64
        }) || self
            .last_offset
            .is_some_and(|last| frame.bit_offset <= last);
        if new_stream {
            self.voice = Voice::default();
            self.data.reset_assembly();
            self.origin = None;
            self.last_speech = None;
            self.status.epoch += 1;
        }
        let origin = *self.origin.get_or_insert(frame.bit_offset);
        let index = ((frame.bit_offset - origin) / 384) as u32;
        self.last_offset = Some(frame.bit_offset);
        self.status.frames += 1;
        self.status.synchronized_frames += u64::from(frame.sync_matched);
        let received_at = self.segment_time.map(|t| {
            t + std::time::Duration::from_secs_f64((frame.bit_offset + 384) as f64 / 9600.0)
        });
        let Decoded { speech, data } = decoded;
        let corrected_bits = speech.as_ref().map(|s| s.corrected_bits);
        let payload = speech.as_ref().map(|s| s.payload);
        if let Some(ref s) = speech {
            self.last_speech = Some(frame.bit_offset);
            self.status.voice_frames += 1;
            self.status.corrected_bits += s.corrected_bits as u64;
        } else {
            self.status.rejected_voice += 1;
        }
        self.voice.push(index, payload)?;
        let info = data.map(|(info, _)| info);
        let messages = self.data.decode_with(&frame, data);
        for message in messages.iter().cloned() {
            if message.operation_summary.is_some() {
                OperationReception::update(
                    &mut self.status.operation_monitor,
                    &message,
                    chrono::DateTime::<chrono::Utc>::from(
                        received_at.unwrap_or_else(std::time::SystemTime::now),
                    )
                    .to_rfc3339(),
                );
            }
            if self.events.len() == 128 {
                self.events.pop_front();
            }
            self.events.push_back(message.clone());
            let existing = self.history.iter_mut().find(|m| {
                m.message.kind == message.kind
                    && m.message.identity == message.identity
                    && (message.kind == "ChannelActivity"
                        || m.message.raw_hex == message.raw_hex
                            && m.message.timestamp == message.timestamp)
            });
            self.history_revision += 1;
            if let Some(m) = existing {
                m.message = message;
                m.receptions += 1;
            } else {
                if self.history.len() == 128 {
                    self.history.pop_front();
                }
                self.history.push_back(Received {
                    message,
                    receptions: 1,
                });
            }
        }
        let bit_offset = frame.bit_offset;
        observe(FrameEvent {
            epoch: self.status.epoch,
            index,
            frame,
            corrected_bits,
            messages,
            speech: payload,
            data: info,
            detector,
            received_at,
        });
        let mut audio = self.drain();
        audio.extend(self.flush_idle(bit_offset)?);
        Ok(audio)
    }
    // Drain the 160 ms synthesis lookahead after 100 ms without voice. This
    // does not end the inferred call: the monitor allows a full second of loss.
    fn flush_idle(&mut self, bit_offset: u64) -> Result<Vec<Audio>, String> {
        // Concealed slots (up to plc::frames()) must not count as silence.
        let idle = 960.max((jr_conventional_voice::plc::frames() as u64 + 1) * 384 + 192);
        if self
            .last_speech
            .is_some_and(|last| bit_offset.saturating_sub(last) >= idle)
        {
            self.voice.flush()?;
            let audio = self.drain();
            self.voice = Voice::default();
            self.last_speech = None;
            Ok(audio)
        } else {
            Ok(Vec::new())
        }
    }
    fn drain(&mut self) -> Vec<Audio> {
        let mut audio = Vec::new();
        while let Some(out) = self.voice.read() {
            self.status.audio_frames += 1;
            audio.push(Audio {
                channel: self.config.id.clone(),
                epoch: self.status.epoch,
                index: out.index,
                pcm: out.pcm.to_vec(),
            });
        }
        audio
    }
    pub fn flush(&mut self) -> Result<Vec<Audio>, String> {
        self.flush_observed(|_| {})
    }
    /// [`Self::flush`], reporting the frames it releases like
    /// [`Self::process_observed`].
    pub fn flush_observed(
        &mut self,
        mut observe: impl FnMut(FrameEvent),
    ) -> Result<Vec<Audio>, String> {
        // Positions still waiting for the other detector at end of input.
        let mut audio = Vec::new();
        for (frame, detector, decoded) in self.merger.release([None, None], true, Decoded::of) {
            audio.extend(self.frame_decoded(frame, detector, decoded, &mut observe)?);
        }
        self.voice.flush()?;
        audio.extend(self.drain());
        Ok(audio)
    }
    pub fn take_messages(&mut self) -> Vec<Message> {
        self.events.drain(..).collect()
    }
    pub fn snapshot(&mut self) -> Status {
        self.refresh_status();
        self.status.clone()
    }
    /// Update `target` (a copy handed out earlier) to the current status,
    /// rebuilding the message list only when the history changed.
    pub fn snapshot_into(&mut self, target: &mut Status) {
        let rebuild = self.snapshot_revision != self.history_revision;
        self.refresh_status();
        // Clone everything but the message list, which is copied only when
        // the history changed since the previous snapshot.
        let messages = std::mem::take(&mut self.status.messages);
        let kept = if rebuild {
            messages.clone()
        } else {
            std::mem::take(&mut target.messages)
        };
        *target = self.status.clone();
        target.messages = kept;
        self.status.messages = messages;
    }
    fn refresh_status(&mut self) {
        let (points, evm) = self.dsp.signal();
        self.status.constellation = points;
        self.status.evm_percent = evm;
        self.status.data = self.data.stats.clone();
        if self.snapshot_revision != self.history_revision {
            self.status.messages = self.history.iter().rev().cloned().collect();
            self.snapshot_revision = self.history_revision;
        }
    }
}

/// Mean power of an IQ block in dBFS.
pub fn block_dbfs(iq: &[Complex32]) -> f64 {
    let power = iq
        .iter()
        .map(|s| s.real as f64 * s.real as f64 + s.imaginary as f64 * s.imaginary as f64)
        .sum::<f64>()
        / iq.len().max(1) as f64;
    10.0 * power.max(1e-12).log10()
}

/// Compatibility accessor for the built-in regional profiles.
/// Frequency definitions live only in ui/radio-presets.json.
pub fn presets(region: &str) -> Vec<ChannelConfig> {
    crate::presets::resolve(region)
        .expect("known built-in profile")
        .channels
}
pub fn validate_channels(channels: &[ChannelConfig], rate: f64, center: f64) -> Result<(), String> {
    if channels.is_empty() || channels.len() > 64 {
        return Err("Select 1–64 receive channels".into());
    }
    let mut ids = std::collections::HashSet::new();
    for c in channels {
        if c.id.is_empty() || !ids.insert(&c.id) {
            return Err("Channel IDs must be unique".into());
        }
        Dsp::new(rate, c.frequency_hz - center)
            .map_err(|e| format!("{} ({:.5} MHz): {e}", c.label, c.frequency_hz / 1e6))?;
        if !c.frequency_hz.is_finite() || c.frequency_hz <= 0.0 {
            return Err("Invalid frequency".into());
        }
    }
    Ok(())
}

use crate::{
    data::{Decoder, Format, Message, OperationReception, Stats},
    frame::{Frame, Synchronizer},
    voice_frame, Complex32,
};
use jr_conventional_dsp::receiver::Dsp;
use jr_conventional_voice::receiver::Voice;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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
}
pub struct Pipeline {
    pub config: ChannelConfig,
    rate: f64,
    center: f64,
    dsp: Dsp,
    sync: Synchronizer,
    voice: Voice,
    data: Decoder,
    dibits: Vec<u8>,
    origin: Option<u64>,
    last_offset: Option<u64>,
    last_speech: Option<u64>,
    segment_samples: u64,
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
            dsp: Dsp::new(rate, config.frequency_hz - center)?,
            sync: Synchronizer::default(),
            voice: Voice::default(),
            data: Decoder::new(config.format),
            config,
            rate,
            center,
            dibits: Vec::new(),
            origin: None,
            last_offset: None,
            last_speech: None,
            segment_samples: 0,
            history: VecDeque::new(),
            events: VecDeque::new(),
            status: Status {
                input_dbfs: -120.0,
                ..Status::default()
            },
        })
    }
    pub fn discontinuity(&mut self) -> Result<(), String> {
        self.dsp = Dsp::new(self.rate, self.config.frequency_hz - self.center)?;
        self.sync = Synchronizer::default();
        self.voice = Voice::default();
        self.data.reset_assembly();
        self.origin = None;
        self.last_offset = None;
        self.last_speech = None;
        self.segment_samples = 0;
        self.history.clear();
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
        mut observe: impl FnMut(FrameEvent),
    ) -> Result<Vec<Audio>, String> {
        self.status.iq_samples += iq.len() as u64;
        self.segment_samples += iq.len() as u64;
        let snap = self.dsp.process(iq, &mut self.dibits)?;
        let mut frames = Vec::new();
        for &dibit in &self.dibits {
            self.sync.push(dibit, &mut frames);
        }
        self.status.carrier_locked = snap.carrier_locked != 0;
        self.status.frequency_error_hz = snap.frequency_offset_hz;
        self.status.frame_locked = self.sync.locked;
        if !iq.is_empty() {
            let power = iq
                .iter()
                .map(|s| s.real as f64 * s.real as f64 + s.imaginary as f64 * s.imaginary as f64)
                .sum::<f64>()
                / iq.len() as f64;
            self.status.input_dbfs = 10.0 * power.max(1e-12).log10();
        }
        let mut audio = Vec::new();
        for frame in frames {
            audio.extend(self.frame_observed(&frame, &mut observe)?);
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
        let speech = voice_frame::decode(&frame.received);
        let corrected_bits = speech.as_ref().map(|s| s.corrected_bits);
        if let Some(ref s) = speech {
            self.last_speech = Some(frame.bit_offset);
            self.status.voice_frames += 1;
            self.status.corrected_bits += s.corrected_bits as u64;
        } else {
            self.status.rejected_voice += 1;
        }
        self.voice.push(index, speech.map(|s| s.payload))?;
        let messages = self.data.decode(frame);
        for message in messages.iter().cloned() {
            if message.operation_summary.is_some() {
                OperationReception::update(
                    &mut self.status.operation_monitor,
                    &message,
                    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
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
        observe(FrameEvent {
            epoch: self.status.epoch,
            index,
            frame: frame.clone(),
            corrected_bits,
            messages,
        });
        let mut audio = self.drain();
        audio.extend(self.flush_idle(frame.bit_offset)?);
        Ok(audio)
    }
    // Drain the 160 ms synthesis lookahead after 100 ms without voice. This
    // does not end the inferred call: the monitor allows a full second of loss.
    fn flush_idle(&mut self, bit_offset: u64) -> Result<Vec<Audio>, String> {
        if self
            .last_speech
            .is_some_and(|last| bit_offset.saturating_sub(last) >= 960)
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
        self.voice.flush()?;
        Ok(self.drain())
    }
    pub fn take_messages(&mut self) -> Vec<Message> {
        self.events.drain(..).collect()
    }
    pub fn snapshot(&mut self) -> Status {
        let (points, evm) = self.dsp.signal();
        self.status.constellation = points;
        self.status.evm_percent = evm;
        self.status.data = self.data.stats.clone();
        self.status.messages = self.history.iter().rev().cloned().collect();
        self.status.clone()
    }
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

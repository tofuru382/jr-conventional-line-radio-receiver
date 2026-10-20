use crate::{
    data::{Decoder, Format, Message, OperationReception, Stats},
    frame::{Frame, Synchronizer},
    voice_frame, Complex32,
};
use jr_conventional_dsp::receiver::Dsp;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Voice decoder generation. Each later one sounds more natural and costs
/// more CPU per decoded 40 ms frame (v1.3 ≈ 1x, v1.4 ≈ 7x, v1.5 ≈ 20x).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceDecoder {
    V13,
    V14,
    #[default]
    V15,
}
/// The selected decoder, boxed to keep synthesis state off worker stacks.
enum Voice {
    V13(Box<jr_conventional_voice_lite::receiver::Voice>),
    V14(Box<jr_conventional_voice_v14::receiver::Voice>),
    V15(jr_conventional_voice::receiver::Voice),
}
struct Decoded {
    index: u32,
    pcm: Vec<i16>,
}
impl Voice {
    fn new(kind: VoiceDecoder) -> Self {
        match kind {
            VoiceDecoder::V13 => Self::V13(Box::default()),
            VoiceDecoder::V14 => Self::V14(Box::default()),
            VoiceDecoder::V15 => Self::V15(Default::default()),
        }
    }
    fn push(&mut self, index: u32, payload: Option<[u8; 20]>) -> Result<(), String> {
        match self {
            Self::V13(v) => v.push(index, payload),
            Self::V14(v) => v.push(index, payload),
            Self::V15(v) => v.push(index, payload),
        }
    }
    fn read(&mut self) -> Option<Decoded> {
        match self {
            Self::V13(v) => v.read().map(|o| Decoded {
                index: o.index,
                pcm: o.pcm.to_vec(),
            }),
            Self::V14(v) => v.read().map(|o| Decoded {
                index: o.index,
                pcm: o.pcm.to_vec(),
            }),
            Self::V15(v) => v.read().map(|o| Decoded {
                index: o.index,
                pcm: o.pcm.to_vec(),
            }),
        }
    }
    fn flush(&mut self) -> Result<(), String> {
        match self {
            Self::V13(v) => v.flush(),
            Self::V14(v) => v.flush(),
            Self::V15(v) => v.flush(),
        }
    }
}

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
    decoder: VoiceDecoder,
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
        Self::with_decoder(config, rate, center, VoiceDecoder::default())
    }
    pub fn with_decoder(
        config: ChannelConfig,
        rate: f64,
        center: f64,
        decoder: VoiceDecoder,
    ) -> Result<Self, String> {
        if !center.is_finite() || !config.frequency_hz.is_finite() || config.frequency_hz <= 0.0 {
            return Err("Invalid channel frequency".into());
        }
        Ok(Self {
            dsp: Dsp::new(rate, config.frequency_hz - center)?,
            sync: Synchronizer::default(),
            decoder,
            voice: Voice::new(decoder),
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
        self.voice = Voice::new(self.decoder);
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
            self.voice = Voice::new(self.decoder);
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
            self.voice = Voice::new(self.decoder);
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
                pcm: out.pcm,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(index: u32) -> [u8; 20] {
        let mut state = index as u64 + 1;
        let mut result = std::array::from_fn(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 32) as u8
        });
        result[19] &= 63;
        result
    }

    #[test]
    fn every_decoder_turns_each_slot_into_one_frame_of_pcm() {
        for kind in [VoiceDecoder::V13, VoiceDecoder::V14, VoiceDecoder::V15] {
            let mut voice = Voice::new(kind);
            let mut out = Vec::new();
            for index in 0..12 {
                let bytes = (index != 5).then(|| payload(index));
                voice.push(index, bytes).unwrap();
                while let Some(frame) = voice.read() {
                    out.push(frame);
                }
            }
            voice.flush().unwrap();
            while let Some(frame) = voice.read() {
                out.push(frame);
            }
            // v1.3 emits nothing for a missing slot; later ones conceal it.
            let indices: Vec<u32> = out.iter().map(|f| f.index).filter(|&i| i != 5).collect();
            assert_eq!(
                indices,
                (0..12).filter(|&i| i != 5).collect::<Vec<_>>(),
                "{kind:?}"
            );
            assert!(out.iter().all(|f| f.pcm.len() == 320), "{kind:?}");
            assert!(
                out.iter().any(|f| f.pcm.iter().any(|&s| s != 0)),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn decoder_names_match_the_app_settings() {
        for (kind, name) in [
            (VoiceDecoder::V13, "\"v13\""),
            (VoiceDecoder::V14, "\"v14\""),
            (VoiceDecoder::V15, "\"v15\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), name);
        }
        let config: crate::session::Config = serde_json::from_value(serde_json::json!({
            "input": {"kind": "wave", "path": "/x.wav", "center_hz": 352603125.0},
            "channels": [],
        }))
        .unwrap();
        assert_eq!(config.voice_decoder, VoiceDecoder::V15);
    }
}

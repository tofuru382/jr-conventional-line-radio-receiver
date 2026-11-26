//! Indexed receiver with seven-frame total lookahead and bounded gap draining.
use crate::correction::Correction;
use crate::numeric::Numeric;
use crate::predictor::{Payload, Prediction, Predictor, LOOKAHEAD};
use crate::synthesis::{Input, Pipeline};
use crate::vocoder::Pcm;
use std::collections::VecDeque;

#[derive(Clone, Copy)]
struct QueuedFrame {
    payload: Payload,
    index: u32,
}
#[derive(Clone, Copy)]
pub struct Output {
    pub index: u32,
    pub pcm: Pcm,
}
pub struct VoiceStream {
    predictor: Predictor,
    correction: Correction,
    numeric: Numeric,
    pipeline: Pipeline,
    next_index: u64,
    fade_in: bool,
    known_missing: bool,
    queue: [QueuedFrame; LOOKAHEAD + 5],
    head: usize,
    count: usize,
    last_fed: Option<u32>,
    output: [Output; 16],
    output_head: usize,
    output_count: usize,
    pub synthesized: u32,
    pub emitted: u32,
    // The last received payload and how many slots in a row repeated it.
    last_payload: Option<Payload>,
    concealed: u32,
    loss_slots: VecDeque<(u32, u32)>,
    previous_pcm: Option<Output>,
    previous_loss: bool,
}
impl Default for VoiceStream {
    fn default() -> Self {
        Self {
            predictor: Predictor::default(),
            correction: Correction::default(),
            numeric: Numeric::default(),
            pipeline: Pipeline::default(),
            next_index: 0,
            fade_in: false,
            known_missing: false,
            queue: [QueuedFrame {
                payload: [0; 20],
                index: 0,
            }; LOOKAHEAD + 5],
            head: 0,
            count: 0,
            last_fed: None,
            output: [Output {
                index: 0,
                pcm: [0; 320],
            }; 16],
            output_head: 0,
            output_count: 0,
            synthesized: 0,
            emitted: 0,
            last_payload: None,
            concealed: 0,
            loss_slots: VecDeque::new(),
            previous_pcm: None,
            previous_loss: false,
        }
    }
}
impl VoiceStream {
    pub fn has_output(&self) -> bool {
        self.output_count != 0
    }
    pub fn read(&mut self) -> Option<Output> {
        if self.output_count == 0 {
            return None;
        }
        let result = self.output[self.output_head];
        self.output_head = (self.output_head + 1) % self.output.len();
        self.output_count -= 1;
        Some(result)
    }
    pub fn push(&mut self, index: u32, payload: Payload) -> Result<(), ()> {
        self.last_payload = Some(payload);
        self.concealed = 0;
        self.enqueue(index, payload)
    }
    /// A slot whose frame did not decode. Up to [`crate::plc::frames`] such
    /// slots in a row repeat the last received payload (packet loss
    /// concealment): the speech parameters carry on through a short fade
    /// instead of the synthesis draining, resetting and fading in again.
    /// Longer losses, or a slot that does not follow the last one, are
    /// handled as a discontinuity as before.
    pub fn conceal(&mut self, index: u32) -> Result<(), ()> {
        let last = if self.count != 0 {
            Some(self.queue[(self.head + self.count - 1) % self.queue.len()].index)
        } else {
            self.last_fed
        };
        match (self.last_payload, last) {
            (Some(payload), Some(last))
                if self.concealed < crate::plc::frames() && index as u64 == last as u64 + 1 =>
            {
                self.concealed += 1;
                // Mark by output index, not arrival order: synthesis looks ahead.
                self.loss_slots.push_back((index, self.concealed));
                self.enqueue(index, payload)
            }
            _ => self.missing_frame(),
        }
    }
    fn enqueue(&mut self, index: u32, payload: Payload) -> Result<(), ()> {
        if self.count == self.queue.len() {
            return Err(());
        }
        self.known_missing = false;
        self.queue[(self.head + self.count) % self.queue.len()] = QueuedFrame { index, payload };
        self.count += 1;
        self.advance()
    }
    pub fn missing_frame(&mut self) -> Result<(), ()> {
        self.known_missing = true;
        self.advance()
    }
    fn can_produce(&self, before: u32) -> bool {
        self.synthesized == before || !self.pipeline.has_pending()
    }
    pub fn advance(&mut self) -> Result<(), ()> {
        let before = self.synthesized;
        while self.count != 0 {
            let frame = self.queue[self.head];
            let gap = self
                .last_fed
                .is_some_and(|last| frame.index as u64 != last as u64 + 1);
            if gap {
                if self.predictor.pending() != 0 {
                    if !self.can_produce(before) {
                        return Ok(());
                    }
                    let p = self.predictor.emit()?;
                    self.render(p, true)?;
                    continue;
                }
                if self.pipeline.has_pending() {
                    if self.synthesized != before {
                        return Ok(());
                    }
                    if let Some(output) = self.pipeline.finish()? {
                        self.emit(Output {
                            index: u32::try_from(output.index).map_err(|_| ())?,
                            pcm: output.pcm,
                        })?;
                    }
                    if self.pipeline.has_pending() {
                        return Ok(());
                    }
                }
                self.predictor.reset();
                self.correction.reset();
                self.pipeline = Pipeline::default();
                self.fade_in = true;
                self.last_fed = None;
            }
            let will_produce = self.predictor.pending() == LOOKAHEAD;
            if will_produce && !self.can_produce(before) {
                return Ok(());
            }
            if self.last_fed.is_none() {
                self.next_index = frame.index as u64;
            }
            if let Some(p) = self.predictor.decode(&frame.payload)? {
                self.render(p, false)?;
            }
            self.last_fed = Some(frame.index);
            self.head = (self.head + 1) % self.queue.len();
            self.count -= 1;
        }
        // A missing slot proves a discontinuity, but retain the final control until
        // EOF or a restart determines whether its trailing fade is required.
        if self.known_missing && self.predictor.pending() > 1 && self.can_produce(before) {
            let p = self.predictor.emit()?;
            self.render(p, false)?;
        }
        if self.synthesized.wrapping_sub(before) > 1 {
            return Err(());
        }
        Ok(())
    }
    pub fn flush(&mut self) -> Result<(), ()> {
        while self.count != 0 {
            self.advance()?;
        }
        while self.predictor.pending() != 0 {
            let p = self.predictor.emit()?;
            self.render(p, false)?;
        }
        while let Some(output) = self.pipeline.finish()? {
            self.emit(Output {
                index: u32::try_from(output.index).map_err(|_| ())?,
                pcm: output.pcm,
            })?;
        }
        Ok(())
    }
    fn render(&mut self, p: Prediction, draining_gap: bool) -> Result<(), ()> {
        let corrected = self
            .correction
            .process(&p.bits, &p.controls, &mut self.numeric)?;
        let mut payload = [0; 20];
        for i in 0..158 {
            payload[i / 8] |= p.bits[i] << (i % 8);
        }
        let input = Input {
            key: self.next_index,
            payload,
            controls: corrected,
            fade_in: self.fade_in,
            fade_out: draining_gap && self.predictor.pending() == 0,
        };
        if let Some(output) = self.pipeline.push(input)? {
            self.emit(Output {
                index: u32::try_from(output.index).map_err(|_| ())?,
                pcm: output.pcm,
            })?;
        }
        self.fade_in = false;
        self.next_index += 1;
        Ok(())
    }
    fn emit(&mut self, mut frame: Output) -> Result<(), ()> {
        if self.output_count == self.output.len() {
            return Err(());
        }
        while self
            .loss_slots
            .front()
            .is_some_and(|&(i, _)| i < frame.index)
        {
            self.loss_slots.pop_front();
        }
        let loss = if self
            .loss_slots
            .front()
            .is_some_and(|&(i, _)| i == frame.index)
        {
            self.loss_slots.pop_front().map_or(0, |(_, n)| n)
        } else {
            0
        };
        let previous = self
            .previous_pcm
            .as_ref()
            .filter(|p| p.index.checked_add(1) == Some(frame.index));
        if loss != 0 || (self.previous_loss && previous.is_some()) {
            repair_boundary(&mut frame.pcm, previous.map(|p| &p.pcm), loss);
        }
        self.previous_loss = loss != 0;
        self.previous_pcm = Some(frame);
        self.output[(self.output_head + self.output_count) % self.output.len()] = frame;
        self.output_count += 1;
        self.synthesized = self.synthesized.wrapping_add(1);
        self.emitted = self.emitted.wrapping_add(1);
        Ok(())
    }
}

/// Estimate a period only from strongly periodic decoded speech. For unvoiced
/// speech, do not invent a pitch: use a short taper from the preceding sample.
fn pitch_period(pcm: &Pcm) -> Option<usize> {
    let mut best = 0.65_f64;
    let mut period = None;
    for lag in 20..=160 {
        let (mut dot, mut a, mut b) = (0.0, 0.0, 0.0);
        for i in 160..320 {
            let x = pcm[i] as f64;
            let y = pcm[i - lag] as f64;
            dot += x * y;
            a += x * x;
            b += y * y;
        }
        if a < 160.0 * 64.0 * 64.0 || b < 160.0 * 64.0 * 64.0 {
            continue;
        }
        // Prefer the fundamental over multiples of a similar correlation.
        let score = dot / (a * b).sqrt() - lag as f64 * 0.0004;
        if score > best {
            best = score;
            period = Some(lag);
        }
    }
    period
}

/// Repeated-payload PLC stays in the decoder's normal state machine. Apply
/// attenuation and a short pitch-aware overlap only to marked loss slots and
/// their immediate recovery; clean frames and discontinuous calls stay intact.
fn repair_boundary(pcm: &mut Pcm, previous: Option<&Pcm>, loss: u32) {
    let (start, end) = match loss {
        0 => (1.0, 1.0),
        1 => (1.0, 0.82),
        2 => (0.82, 0.55),
        3 => (0.55, 0.24),
        _ => (0.24, 0.0),
    };
    let period = previous.and_then(pitch_period);
    for (i, sample) in pcm.iter_mut().enumerate() {
        let t = i as f32 / 319.0;
        let mut y = *sample as f32 * (start + (end - start) * t);
        if let Some(p) = previous.filter(|_| i < 40) {
            let old = match period {
                Some(lag) => p[320 - lag + i % lag] as f32,
                None => p[319] as f32 * (1.0 - i as f32 / 40.0),
            };
            let blend = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / 39.0).cos();
            y = old + (y - old) * blend;
        }
        *sample = y.round().clamp(-32768.0, 32767.0) as i16;
    }
}

#[cfg(test)]
mod plc_tests {
    use super::*;

    #[test]
    fn concealment_decays_and_recovery_preserves_the_frame_tail() {
        let previous = std::array::from_fn(|i| {
            (10000.0 * (std::f32::consts::TAU * i as f32 / 40.0).sin()) as i16
        });
        assert_eq!(pitch_period(&previous), Some(40));
        let mut lost = [10000; 320];
        repair_boundary(&mut lost, Some(&previous), 3);
        assert_eq!(lost[319], 2400);
        let mut recovered = [5000; 320];
        repair_boundary(&mut recovered, Some(&lost), 0);
        assert_eq!(recovered[319], 5000);
    }

    #[test]
    fn silence_has_no_pitch() {
        assert_eq!(pitch_period(&[0; 320]), None);
    }
}

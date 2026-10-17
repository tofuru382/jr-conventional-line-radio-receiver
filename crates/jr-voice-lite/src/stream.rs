//! Indexed payload stream. Gaps drain lookahead, fade edges and reset synthesis.
use crate::chime::ChimeSynth;
use crate::correction::Correction;
use crate::numeric::{round_even, PI};
use crate::predictor::{Payload, Prediction, Predictor, LOOKAHEAD};
use crate::vocoder::{Pcm, Vocoder};

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
    vocoder: Vocoder,
    chime: ChimeSynth,
    held: Option<Output>,
    next_index: u64,
    fade_in: bool,
    queue: [QueuedFrame; LOOKAHEAD + 1],
    head: usize,
    count: usize,
    last_fed: Option<u32>,
    output: [Output; 16],
    output_head: usize,
    output_count: usize,
    pub synthesized: u32,
    pub emitted: u32,
}
impl Default for VoiceStream {
    fn default() -> Self {
        Self {
            predictor: Predictor::default(),
            correction: Correction::default(),
            vocoder: Vocoder::default(),
            chime: ChimeSynth::default(),
            held: None,
            next_index: 0,
            fade_in: false,
            queue: [QueuedFrame {
                payload: [0; 20],
                index: 0,
            }; LOOKAHEAD + 1],
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
        if self.count == self.queue.len() {
            return Err(());
        }
        self.queue[(self.head + self.count) % self.queue.len()] = QueuedFrame { index, payload };
        self.count += 1;
        self.advance()
    }
    pub fn advance(&mut self) -> Result<(), ()> {
        let mut budget = 1;
        while self.count != 0 {
            let frame = self.queue[self.head];
            let gap = self
                .last_fed
                .is_some_and(|last| frame.index as u64 != last as u64 + 1);
            if gap {
                if self.predictor.pending() != 0 {
                    if budget == 0 {
                        return Ok(());
                    }
                    let prediction = self.predictor.emit()?;
                    self.render(prediction)?;
                    budget -= 1;
                    continue;
                }
                self.predictor.reset();
                if let Some(held) = &mut self.held {
                    fade(&mut held.pcm[240..], false);
                }
                self.emit_frame()?;
                self.vocoder.reset();
                self.correction.reset();
                self.chime.reset();
                self.fade_in = true;
                self.last_fed = None;
            }
            let will_render = self.predictor.pending() == LOOKAHEAD;
            if will_render && budget == 0 {
                return Ok(());
            }
            if self.last_fed.is_none() {
                self.next_index = frame.index as u64;
            }
            if let Some(prediction) = self.predictor.decode(&frame.payload)? {
                self.render(prediction)?;
            }
            budget -= usize::from(will_render);
            self.last_fed = Some(frame.index);
            self.head = (self.head + 1) % self.queue.len();
            self.count -= 1;
        }
        Ok(())
    }
    pub fn flush(&mut self) -> Result<(), ()> {
        while self.count != 0 {
            self.advance()?;
        }
        while self.predictor.pending() != 0 {
            let prediction = self.predictor.emit()?;
            self.render(prediction)?;
        }
        self.emit_frame()
    }
    fn render(&mut self, p: Prediction) -> Result<(), ()> {
        self.emit_frame()?;
        let corrected = self
            .correction
            .process(&p.bits, &p.controls, &mut self.vocoder.numeric)?;
        self.chime.render(&p.controls, &p.periods, p.chime)?;
        let mut pcm = self.vocoder.process(&corrected, true, true)?;
        self.chime.blend(p.chime_end, &mut pcm);
        if self.fade_in {
            fade(&mut pcm[..80], true);
        }
        self.fade_in = false;
        self.held = Some(Output {
            index: u32::try_from(self.next_index).map_err(|_| ())?,
            pcm,
        });
        self.next_index += 1;
        self.synthesized = self.synthesized.wrapping_add(1);
        if self.predictor.pending() != 0 {
            self.emit_frame()?;
        }
        Ok(())
    }
    fn emit_frame(&mut self) -> Result<(), ()> {
        if let Some(frame) = self.held.take() {
            if self.output_count == self.output.len() {
                return Err(());
            }
            self.output[(self.output_head + self.output_count) % self.output.len()] = frame;
            self.output_count += 1;
            self.emitted = self.emitted.wrapping_add(1);
        }
        Ok(())
    }
}
fn fade(pcm: &mut [i16], fade_in: bool) {
    let denominator = pcm.len().saturating_sub(1).max(1) as f32;
    for (i, sample) in pcm.iter_mut().enumerate() {
        let gain = 0.5 - 0.5 * (PI * i as f32 / denominator).cos();
        *sample = round_even(*sample as f32 * if fade_in { gain } else { 1.0 - gain }) as i16;
    }
}

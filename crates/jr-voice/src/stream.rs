//! Indexed receiver with four-frame total lookahead and bounded gap draining.
use crate::chime::ChimeSynth;
use crate::correction::Correction;
use crate::numeric::Numeric;
use crate::planner::{Input, Planner};
use crate::predictor::{Payload, Prediction, Predictor, LOOKAHEAD};
use crate::vocoder::Pcm;

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
    planner: Planner,
    chime: ChimeSynth,
    next_index: u64,
    fade_in: bool,
    known_missing: bool,
    queue: [QueuedFrame; LOOKAHEAD + 2],
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
            numeric: Numeric::default(),
            planner: Planner::default(),
            chime: ChimeSynth::default(),
            next_index: 0,
            fade_in: false,
            known_missing: false,
            queue: [QueuedFrame {
                payload: [0; 20],
                index: 0,
            }; LOOKAHEAD + 2],
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
        self.known_missing = false;
        self.queue[(self.head + self.count) % self.queue.len()] = QueuedFrame { index, payload };
        self.count += 1;
        self.advance()
    }
    pub fn missing_frame(&mut self) -> Result<(), ()> {
        self.known_missing = true;
        self.advance()
    }
    fn can_produce(&self, budget: usize, before: u32) -> bool {
        budget != 0 && (self.synthesized == before || !self.planner.has_pending())
    }
    pub fn advance(&mut self) -> Result<(), ()> {
        let before = self.synthesized;
        let mut budget = 1;
        while self.count != 0 {
            let frame = self.queue[self.head];
            let gap = self
                .last_fed
                .is_some_and(|last| frame.index as u64 != last as u64 + 1);
            if gap {
                if self.predictor.pending() != 0 {
                    if !self.can_produce(budget, before) {
                        return Ok(());
                    }
                    let p = self.predictor.emit()?;
                    self.render(p, true)?;
                    budget -= 1;
                    continue;
                }
                if self.planner.has_pending() {
                    if self.synthesized != before {
                        return Ok(());
                    }
                    if let Some(output) = self.planner.finish()? {
                        self.emit(output)?;
                    }
                }
                self.predictor.reset();
                self.correction.reset();
                self.chime.reset();
                self.planner = Planner::default();
                self.fade_in = true;
                self.last_fed = None;
            }
            let will_produce = self.predictor.pending() == LOOKAHEAD;
            if will_produce && !self.can_produce(budget, before) {
                return Ok(());
            }
            if self.last_fed.is_none() {
                self.next_index = frame.index as u64;
            }
            if let Some(p) = self.predictor.decode(&frame.payload)? {
                self.render(p, false)?;
            }
            budget -= usize::from(will_produce);
            self.last_fed = Some(frame.index);
            self.head = (self.head + 1) % self.queue.len();
            self.count -= 1;
        }
        // A missing slot proves a discontinuity, but retain the final control until
        // EOF or a restart determines whether its trailing fade is required.
        if self.known_missing && self.predictor.pending() > 1 && self.can_produce(budget, before) {
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
        if let Some(output) = self.planner.finish()? {
            self.emit(output)?;
        }
        Ok(())
    }
    fn render(&mut self, p: Prediction, draining_gap: bool) -> Result<(), ()> {
        let corrected = self
            .correction
            .process(&p.bits, &p.controls, &mut self.numeric)?;
        self.chime.render(&p.controls, &p.periods, p.chime)?;
        let (weight, offset) = self.chime.mix_coefficients(p.chime_end);
        let mut payload = [0; 20];
        for i in 0..158 {
            payload[i / 8] |= p.bits[i] << (i % 8);
        }
        let input = Input {
            key: u32::try_from(self.next_index).map_err(|_| ())?,
            payload,
            controls: corrected,
            weight,
            offset,
            fade_in: self.fade_in,
            fade_out: draining_gap && self.predictor.pending() == 0,
        };
        if let Some(output) = self.planner.push(input)? {
            self.emit(output)?;
        }
        self.fade_in = false;
        self.next_index += 1;
        Ok(())
    }
    fn emit(&mut self, frame: Output) -> Result<(), ()> {
        if self.output_count == self.output.len() {
            return Err(());
        }
        self.output[(self.output_head + self.output_count) % self.output.len()] = frame;
        self.output_count += 1;
        self.synthesized = self.synthesized.wrapping_add(1);
        self.emitted = self.emitted.wrapping_add(1);
        Ok(())
    }
}

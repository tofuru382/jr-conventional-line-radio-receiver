//! Bounded common synthesis pipeline with four frames of parameter lookahead.
use crate::celp::Native;
use crate::excitation::{geometry, pcm_descriptor, periodicity, Sources};
use crate::numeric::{round_even, PI};
use crate::planner::{Input as PlannedInput, Output as PlannedOutput, Planner};
use crate::predictor::{Controls, Payload};
use crate::source_features::SourceFeatures;
use crate::synthesis_models::models;
use crate::synthesis_parameters as parameters;
use crate::vocoder::{Pcm, Vocoder};
use std::collections::VecDeque;

#[derive(Clone)]
pub struct Input {
    pub key: u64,
    pub payload: Payload,
    pub controls: Controls,
    pub fade_in: bool,
    pub fade_out: bool,
}
pub struct Output {
    pub index: u64,
    pub pcm: Pcm,
}
struct SourceFrame {
    input: Input,
    changed: Controls,
    source: [f32; 320],
    strength: f64,
    features: [[f32; 633]; 2],
    native_descriptor: [[f32; 22]; 2],
    gain_features: [[f32; 24]; 2],
    age: usize,
}
struct SelectedFrame {
    input: Input,
    features: [[f32; 633]; 2],
    native_descriptor: [[f32; 22]; 2],
    gain_features: [[f32; 24]; 2],
    age: usize,
}
#[derive(Clone)]
struct TemporalFrame {
    input: Input,
    source: [f32; 320],
    delta: [f32; 4],
    embedding: [[f32; 128]; 2],
}
struct GainFrame {
    input: Input,
    source: [f32; 320],
    gain: [f64; 4],
}

fn fade(pcm: &mut Pcm, input: &Input) {
    for (begin, enabled, inverse) in [(0, input.fade_in, false), (240, input.fade_out, true)] {
        if enabled {
            for n in 0..80 {
                let gain = 0.5 - 0.5 * (PI * n as f32 / 79.0).cos();
                pcm[begin + n] =
                    round_even(pcm[begin + n] as f32 * if inverse { 1.0 - gain } else { gain })
                        as i16;
            }
        }
    }
}
fn planned(input: &Input, source: [f32; 320]) -> PlannedInput {
    PlannedInput {
        key: input.key,
        payload: input.payload,
        controls: input.controls,
        source,
        weight: [1.0; 320],
        offset: [0.0; 320],
        fade_in: input.fade_in,
        fade_out: input.fade_out,
    }
}
fn synth(
    vocoder: &mut Vocoder,
    input: &Input,
    source: &[f32; 320],
    curve: Option<&[f32; 320]>,
    restore: bool,
) -> Result<Pcm, ()> {
    let mut pcm =
        vocoder.process_explicit(&input.controls, Some(4), Some(source), curve, restore)?;
    fade(&mut pcm, input);
    Ok(pcm)
}

#[derive(Default)]
pub struct Pipeline {
    native: Native,
    features: SourceFeatures,
    sources: Sources,
    base_synthesis: Vocoder,
    candidate_synthesis: Vocoder,
    original: Planner,
    selected: Planner,
    source_queue: VecDeque<SourceFrame>,
    selected_queue: VecDeque<SelectedFrame>,
    guard_trial: Vocoder,
    gain_baseline: Vocoder,
    final_synthesis: Vocoder,
    temporal: VecDeque<TemporalFrame>,
    temporal_begin: usize,
    temporal_count: usize,
    temporal_next: usize,
    pending_gain: Option<GainFrame>,
    previous_gain: Option<f64>,
    output: VecDeque<Output>,
    received: usize,
    returned: usize,
    last: Option<u64>,
    finished: bool,
}
impl Pipeline {
    pub fn has_pending(&self) -> bool {
        self.received != self.returned
    }
    fn read(&mut self) -> Option<Output> {
        let out = self.output.pop_front();
        self.returned += usize::from(out.is_some());
        out
    }
    pub fn push(&mut self, input: Input) -> Result<Option<Output>, ()> {
        if self.finished
            || !self.output.is_empty()
            || self
                .last
                .is_some_and(|last| last.checked_add(1) != Some(input.key))
        {
            return Err(());
        }
        let observed = self.native.decode(&input.payload)?;
        let base = self.features.process(&input.payload, &input.controls)?;
        let components = self
            .sources
            .components(&observed.excitation, &input.controls);
        let pcm = synth(
            &mut self.base_synthesis,
            &input,
            &components.native,
            None,
            true,
        )?;
        let x = parameters::source_features(
            &base,
            &input.controls,
            &pcm,
            &observed.excitation,
            &components.white64,
        );
        let extra = geometry(&components);
        let mut features = [[0.0; 633]; 2];
        for h in 0..2 {
            features[h][..621].copy_from_slice(&x[h]);
            features[h][621..].copy_from_slice(&extra[h]);
        }
        let raw: [[f32; 40]; 2] =
            std::array::from_fn(|h| models().source.forward(&features[h]).try_into().unwrap());
        let source = self.sources.mix(&input.controls, &raw, &components);
        let spectral = std::array::from_fn(|h| raw[h][..34].try_into().unwrap());
        let changed = parameters::correct(&input.controls, &spectral, 0.5);
        let candidate_input = Input {
            controls: changed,
            ..input.clone()
        };
        let candidate = synth(
            &mut self.candidate_synthesis,
            &candidate_input,
            &source,
            None,
            true,
        )?;
        self.source_queue.push_back(SourceFrame {
            input: input.clone(),
            changed,
            source,
            strength: periodicity(&candidate),
            features,
            native_descriptor: pcm_descriptor(&observed.pcm),
            gain_features: parameters::gain_features(&observed.gain),
            age: self.received,
        });
        self.received += 1;
        self.last = Some(input.key);
        if let Some(out) = self.original.push(planned(&input, [0.0; 320]))? {
            self.choose(out)?;
        }
        if self.source_queue.len() > 1
            || self.selected_queue.len() > 1
            || self.temporal.len() > 3
            || self.output.len() > 1
        {
            return Err(());
        }
        Ok(self.read())
    }
    fn choose(&mut self, original: PlannedOutput) -> Result<(), ()> {
        let frame = self.source_queue.pop_front().ok_or(())?;
        if original.index != frame.input.key {
            return Err(());
        }
        let strength = periodicity(&original.pcm);
        let fallback =
            frame.strength < 0.5 && strength >= 0.65 && strength - frame.strength >= 0.15;
        let mut input = frame.input;
        let source = if fallback {
            [0.0; 320]
        } else {
            input.controls = frame.changed;
            frame.source
        };
        self.selected_queue.push_back(SelectedFrame {
            input: input.clone(),
            features: frame.features,
            native_descriptor: frame.native_descriptor,
            gain_features: frame.gain_features,
            age: frame.age,
        });
        if let Some(out) = self.selected.push(planned(&input, source))? {
            self.resynthesize(out)?;
        }
        Ok(())
    }
    fn resynthesize(&mut self, selected: PlannedOutput) -> Result<(), ()> {
        let frame = self.selected_queue.pop_front().ok_or(())?;
        if frame.input.key != selected.index {
            return Err(());
        }
        let descriptor = pcm_descriptor(&selected.pcm);
        let mut x = [[0.0; 679]; 2];
        for h in 0..2 {
            x[h][..633].copy_from_slice(&frame.features[h]);
            x[h][633..655].copy_from_slice(&descriptor[h]);
            x[h][655..677].copy_from_slice(&frame.native_descriptor[h]);
            x[h][677] = (frame.age as f32).ln_1p();
            x[h][678] = if frame.age == 0 { 1.0 } else { 0.0 };
        }
        let raw: [[f32; 34]; 2] =
            std::array::from_fn(|h| models().spectrum.forward(&x[h]).try_into().unwrap());
        let mut controls = parameters::correct(&frame.input.controls, &raw, 1.0);
        controls[..4].copy_from_slice(&frame.input.controls[..4]);
        let trial_input = Input {
            controls,
            ..frame.input.clone()
        };
        let trial = synth(
            &mut self.guard_trial,
            &trial_input,
            &selected.source,
            None,
            true,
        )?;
        if periodicity(&trial) < 0.5 && periodicity(&selected.pcm) >= 0.5 {
            controls = frame.input.controls;
        }
        let input = Input {
            controls,
            ..frame.input
        };
        let baseline = synth(
            &mut self.gain_baseline,
            &input,
            &selected.source,
            None,
            false,
        )?;
        let mut features = [[0.0; 707]; 2];
        let mut delta = [0.0; 4];
        for h in 0..2 {
            features[h][..679].copy_from_slice(&x[h]);
            for b in 0..2 {
                let at = (h * 2 + b) * 80;
                let energy = baseline[at..at + 80]
                    .iter()
                    .map(|&v| (v as f64 / 32768.0).powi(2))
                    .sum::<f64>()
                    / 80.0;
                features[h][679 + b] =
                    (10.0 * energy.max(1e-12).log10() - controls[h * 2 + b] as f64) as f32;
                features[h][681 + b] = controls[h * 2 + b];
            }
            features[h][683..].copy_from_slice(&frame.gain_features[h]);
            let observed = features[h][570..].try_into().unwrap();
            let gain = models().gain.predict(observed);
            for b in 0..2 {
                delta[h * 2 + b] = gain[b] * 0.5;
            }
        }
        let embedding = std::array::from_fn(|h| models().temporal.temporal_embedding(&features[h]));
        self.temporal.push_back(TemporalFrame {
            input,
            source: selected.source,
            delta,
            embedding,
        });
        self.temporal_count += 1;
        self.advance_temporal(false)
    }
    fn advance_temporal(&mut self, end: bool) -> Result<(), ()> {
        while self.temporal_next < self.temporal_count
            && (end || self.temporal_next + 1 < self.temporal_count)
        {
            let at = self.temporal_next;
            let frame = self.temporal[at - self.temporal_begin].clone();
            let raw: [[f32; 34]; 2] = std::array::from_fn(|h| {
                let context = std::array::from_fn(|j| {
                    let half = (2 * at as i64 + h as i64 + j as i64 - 4)
                        .clamp(0, 2 * self.temporal_count as i64 - 1)
                        as usize;
                    self.temporal[half / 2 - self.temporal_begin].embedding[half % 2]
                });
                models().temporal.temporal_finish(&context)
            });
            let (controls, gain) = parameters::temporal(&frame.input.controls, &frame.delta, &raw);
            let current = GainFrame {
                input: Input {
                    controls,
                    ..frame.input
                },
                source: frame.source,
                gain,
            };
            if let Some(previous) = self.pending_gain.take() {
                self.emit(previous, Some(current.gain[0]))?;
            }
            self.pending_gain = Some(current);
            self.temporal_next += 1;
            while self.temporal_begin < self.temporal_next.saturating_sub(2) {
                self.temporal.pop_front();
                self.temporal_begin += 1;
            }
        }
        Ok(())
    }
    fn emit(&mut self, frame: GainFrame, following: Option<f64>) -> Result<(), ()> {
        let curve = parameters::gain_curve(&frame.gain, self.previous_gain, following);
        let pcm = synth(
            &mut self.final_synthesis,
            &frame.input,
            &frame.source,
            Some(&curve),
            false,
        )?;
        self.previous_gain = Some(frame.gain[3]);
        self.output.push_back(Output {
            index: frame.input.key,
            pcm,
        });
        Ok(())
    }
    pub fn finish(&mut self) -> Result<Option<Output>, ()> {
        if !self.finished {
            if let Some(frame) = self.original.finish()? {
                self.choose(frame)?;
            }
            if let Some(frame) = self.selected.finish()? {
                self.resynthesize(frame)?;
            }
            self.advance_temporal(true)?;
            if let Some(frame) = self.pending_gain.take() {
                self.emit(frame, None)?;
            }
            self.temporal.clear();
            self.finished = true;
            if !self.source_queue.is_empty()
                || !self.selected_queue.is_empty()
                || self.output.len() > 4
            {
                return Err(());
            }
        }
        Ok(self.read())
    }
}

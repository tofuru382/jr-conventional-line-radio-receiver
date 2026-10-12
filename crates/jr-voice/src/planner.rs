//! One additional frame of lookahead, with three current and nine future trials.
use crate::numeric::{round_even, PI};
use crate::plan_acoustics::frame_cost;
use crate::plan_features::Features;
use crate::plan_target::{ModelState, Target};
use crate::predictor::{Controls, Payload};
use crate::stream::Output;
use crate::vocoder::{Pcm, Vocoder};
#[derive(Clone)]
pub struct Input {
    pub key: u32,
    pub payload: Payload,
    pub controls: Controls,
    pub weight: [f32; 320],
    pub offset: [f32; 320],
    pub fade_in: bool,
    pub fade_out: bool,
}
struct Pending {
    input: Input,
    target: Target,
    reset: bool,
}
#[derive(Default)]
pub struct Planner {
    features: Features,
    model: ModelState,
    state: Vocoder,
    pending: Option<Pending>,
    previous: Option<Pcm>,
    older: Option<Pcm>,
}
fn fade(pcm: &mut [i16], fade_in: bool) {
    let denominator = pcm.len().saturating_sub(1).max(1) as f32;
    for (i, x) in pcm.iter_mut().enumerate() {
        let gain = 0.5 - 0.5 * (PI * i as f32 / denominator).cos();
        *x = round_even(*x as f32 * if fade_in { gain } else { 1.0 - gain }) as i16;
    }
}
fn mix(raw: &Pcm, input: &Input) -> Pcm {
    let mut pcm = std::array::from_fn(|i| {
        round_even(input.offset[i] + raw[i] as f32 * input.weight[i]) as i16
    });
    if input.fade_in {
        fade(&mut pcm[..80], true);
    }
    if input.fade_out {
        fade(&mut pcm[240..], false);
    }
    pcm
}
impl Planner {
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn push(&mut self, input: Input) -> Result<Option<Output>, ()> {
        let reset = self.pending.is_none() && self.previous.is_none();
        if let Some(p) = &self.pending {
            if input.key as u64 != p.input.key as u64 + 1 {
                return Err(());
            }
        }
        let features = self
            .features
            .process(&input.payload, &input.controls, reset)?;
        let target = self.model.process(&features, &input.controls, reset)?;
        let result = self.render(Some((&input, &target)))?;
        self.pending = Some(Pending {
            input,
            target,
            reset,
        });
        Ok(result)
    }
    pub fn finish(&mut self) -> Result<Option<Output>, ()> {
        self.render(None)
    }
    fn render(&mut self, ahead: Option<(&Input, &Target)>) -> Result<Option<Output>, ()> {
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        if pending.reset {
            self.state.reset();
            self.previous = None;
            self.older = None;
        }
        let mut trials: [Vocoder; 3] = std::array::from_fn(|_| self.state.clone());
        let mut current = [[0i16; 320]; 3];
        let mut scores = [0.0; 3];
        let mut base = 0;
        for a in 0..3 {
            let raw =
                trials[a].process_with_action(&pending.input.controls, true, true, Some(4 * a))?;
            if a != 0 && base != trials[a].original_choice {
                return Err(());
            }
            base = trials[a].original_choice;
            current[a] = mix(&raw, &pending.input);
        }
        for a in 0..3 {
            scores[a] = frame_cost(
                &current[a],
                &pending.target,
                self.previous.as_ref(),
                self.older.as_ref(),
            );
            if let Some((input, target)) = ahead {
                let mut minimum = 0.0;
                for b in 0..3 {
                    let mut future = trials[a].clone();
                    let raw =
                        future.process_with_action(&input.controls, true, true, Some(4 * b))?;
                    let pcm = mix(&raw, input);
                    let cost = frame_cost(&pcm, target, Some(&current[a]), self.previous.as_ref());
                    if !cost.is_finite() {
                        return Err(());
                    }
                    if b == 0 || cost < minimum {
                        minimum = cost;
                    }
                }
                scores[a] += minimum;
            }
            if !scores[a].is_finite() {
                return Err(());
            }
        }
        let mut chosen = 0;
        for a in 1..3 {
            if scores[a] < scores[chosen] {
                chosen = a;
            }
        }
        const MARGIN: f32 = (0.001f64 / (0.17f64 * 0.17f64)) as f32;
        if scores[chosen] >= scores[base / 4] - MARGIN || pending.reset {
            chosen = base / 4;
        }
        self.state = trials[chosen].clone();
        self.older = self.previous;
        self.previous = Some(current[chosen]);
        Ok(Some(Output {
            index: pending.input.key,
            pcm: current[chosen],
        }))
    }
}

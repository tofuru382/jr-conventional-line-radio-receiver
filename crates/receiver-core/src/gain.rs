//! Automatic gain optimisation (自動ゲインの「雑音から最適化」): picks the
//! tuner gain from how the noise floor responds to it, instead of the highest
//! gain that does not clip (the sources' own automatic gain).
//!
//! At low gain the ADC's own noise dominates: more gain lifts the signals but
//! not the floor, so reception improves. Further up the tuner's noise
//! dominates and floor and signals rise together: more gain buys nothing but
//! less headroom for strong signals. Once strong stations (in or out of the
//! band) overload the tuner, intermodulation lifts the floor faster than the
//! gain, even before the ADC clips. A sweep therefore measures the floor (in
//! channelizer bins away from every receive channel) at a few gains and picks
//! the lowest gain whose floor stands [`FLOOR_RISE_DB`] above the ADC's (the
//! ADC noise then costs well under 1 dB of S/N), lower if the floor already
//! rises faster than the gain there or the samples clip or come close to it.
//!
//! Afterwards clipping steps the gain down at once, and the sweep is
//! repeated, only while no call is active, when the floor has moved for a
//! while (another place, another interference) and every half hour. Every
//! gain change is a write to a dongle that is streaming, so they stay rare.

/// Gains (dB) the sweep measures; the nearest table entries are used.
const SWEEP_DB: [f64; 11] = [
    7.7, 12.5, 16.6, 20.7, 25.4, 29.7, 33.8, 37.2, 40.2, 43.9, 49.6,
];
/// The chosen gain's floor stands this far above the ADC's (lowest gain's).
pub const FLOOR_RISE_DB: f64 = 8.0;
/// A floor rising faster than this many dB per dB of gain is
/// intermodulation (the tuner's own noise rises 1:1).
const IMD_SLOPE: f64 = 1.5;
/// Largest sample (share of full scale) a chosen gain may produce.
const PEAK_LIMIT: f32 = 0.7;
/// Clipped I/Q components per 1000 samples that step the gain down at once,
/// and windows of lighter clipping that do.
const HEAVY_CLIPPING: u64 = 10;
const LIGHT_CLIPPING_WINDOWS: u32 = 10;
/// The floor moving this far for this long starts a new sweep (when idle
/// and not within [`RESWEEP_MIN_S`] of the last).
const DRIFT_DB: f64 = 4.0;
const DRIFT_S: f64 = 30.0;
const RESWEEP_MIN_S: f64 = 300.0;
const RESWEEP_EVERY_S: f64 = 1800.0;

/// One block's measurements.
#[derive(Clone, Copy, Debug, Default)]
pub struct Window {
    /// Noise floor (dB, any fixed reference), when it could be measured.
    pub floor_db: Option<f64>,
    /// Clipped I/Q components, samples, and the largest component (0–1).
    pub clipped: u64,
    pub samples: u64,
    pub peak: f32,
}

/// For the app's display.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct GainStatus {
    /// "sweeping", "tracking" or "unavailable".
    pub phase: &'static str,
    pub gain_db: Option<f64>,
    /// What happened last, in words.
    pub note: Option<String>,
    /// Noise floor above the ADC's, at the chosen gain (dB).
    pub floor_rise_db: Option<f64>,
}
impl GainStatus {
    pub fn unavailable(note: &str) -> Self {
        Self {
            phase: "unavailable",
            gain_db: None,
            note: Some(note.into()),
            floor_rise_db: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Point {
    index: usize,
    floor_db: f64,
    clipping: u64,
    peak: f32,
}

#[derive(Default)]
struct Accumulator {
    samples: u64,
    clipped: u64,
    peak: f32,
    floor: f64,
    floors: u64,
}
impl Accumulator {
    fn add(&mut self, w: &Window) {
        self.samples += w.samples;
        self.clipped += w.clipped;
        self.peak = self.peak.max(w.peak);
        if let Some(f) = w.floor_db {
            self.floor += 10f64.powf(f / 10.0);
            self.floors += 1;
        }
    }
    fn floor_db(&self) -> Option<f64> {
        (self.floors > 0).then(|| 10.0 * (self.floor / self.floors as f64).log10())
    }
    /// Clipped components per 1000 samples.
    fn clipping(&self) -> u64 {
        self.clipped * 1000 / self.samples.max(1)
    }
}

enum Phase {
    Sweep { next: usize, points: Vec<Point> },
    Track,
}

pub struct GainOptimizer {
    /// Gain (dB) at each tuner index.
    table: Vec<f64>,
    rate: f64,
    /// Samples to ignore after a change (the new gain reaches the stream
    /// only after the dongle's and the network's buffers), and to measure.
    settle: u64,
    measure: u64,
    skip: u64,
    index: usize,
    phase: Phase,
    acc: Accumulator,
    /// The floor after the last sweep (at the chosen gain), its rise above
    /// the ADC's, and how long it has been off by more than [`DRIFT_DB`].
    baseline: Option<f64>,
    rise: Option<f64>,
    drift: u64,
    strikes: u32,
    since_sweep: u64,
    note: Option<String>,
}

impl GainOptimizer {
    /// `table` holds the tuner's gains in 0.1 dB (rtl_tcp's / librtlsdr's
    /// R820T table). Returns the optimizer and the first gain to set.
    pub fn new(table: &[u16], rate: f64) -> Option<(Self, f64)> {
        if table.len() < 4 || !rate.is_finite() || rate <= 0.0 {
            return None;
        }
        let table: Vec<f64> = table.iter().map(|&g| f64::from(g) / 10.0).collect();
        let mut optimizer = Self {
            settle: 131_072 + (rate / 5.0) as u64,
            measure: (rate / 2.0) as u64,
            rate,
            skip: 0,
            index: 0,
            phase: Phase::Track,
            acc: Accumulator::default(),
            baseline: None,
            rise: None,
            drift: 0,
            strikes: 0,
            since_sweep: 0,
            note: None,
            table,
        };
        let first = optimizer.start_sweep(None);
        Some((optimizer, first))
    }

    pub fn status(&self) -> GainStatus {
        GainStatus {
            phase: match self.phase {
                Phase::Sweep { .. } => "sweeping",
                Phase::Track => "tracking",
            },
            gain_db: Some(self.table[self.index]),
            note: self.note.clone(),
            floor_rise_db: self.rise,
        }
    }

    /// The tuner indices the sweep visits, from low to high gain.
    fn sweep_points(&self) -> Vec<usize> {
        let mut points: Vec<usize> = SWEEP_DB
            .iter()
            .map(|&db| {
                (0..self.table.len())
                    .min_by(|&a, &b| {
                        (self.table[a] - db)
                            .abs()
                            .total_cmp(&(self.table[b] - db).abs())
                    })
                    .unwrap()
            })
            .collect();
        points.dedup();
        points
    }

    fn start_sweep(&mut self, why: Option<&str>) -> f64 {
        let points = self.sweep_points();
        self.index = points[0];
        self.phase = Phase::Sweep {
            next: 1,
            points: Vec::new(),
        };
        self.note = Some(match why {
            Some(why) => format!("{why}ため、最適なゲインを探しています"),
            None => "最適なゲインを探しています".into(),
        });
        self.set_index(self.index)
    }

    fn set_index(&mut self, index: usize) -> f64 {
        self.index = index;
        self.skip = self.settle;
        self.acc = Accumulator::default();
        self.table[index]
    }

    /// Feeds one block; `idle` means no call is active (a sweep may start).
    /// Returns a gain (dB) to set now.
    pub fn observe(&mut self, w: &Window, idle: bool) -> Option<f64> {
        self.since_sweep += w.samples;
        if self.skip > 0 {
            self.skip = self.skip.saturating_sub(w.samples);
            return None;
        }
        self.acc.add(w);
        if self.acc.samples < self.measure {
            return None;
        }
        let acc = std::mem::take(&mut self.acc);
        let plan = self.sweep_points();
        match &mut self.phase {
            Phase::Sweep { next, points } => {
                points.push(Point {
                    index: self.index,
                    floor_db: acc.floor_db().unwrap_or(f64::NAN),
                    clipping: acc.clipping(),
                    peak: acc.peak,
                });
                if *next < plan.len() {
                    let index = plan[*next];
                    *next += 1;
                    return Some(self.set_index(index));
                }
                let points = std::mem::take(points);
                let (index, rise) = choose(&self.table, &points);
                self.phase = Phase::Track;
                self.since_sweep = 0;
                self.baseline = None;
                self.drift = 0;
                self.strikes = 0;
                self.rise = rise;
                self.note = Some(match rise {
                    Some(rise) => format!(
                        "雑音の量から {:.1} dB を選びました（ADC の雑音より {rise:.1} dB 上）",
                        self.table[index]
                    ),
                    None => format!("{:.1} dB を選びました", self.table[index]),
                });
                Some(self.set_index(index))
            }
            Phase::Track => self.track(&acc, idle),
        }
    }

    fn track(&mut self, acc: &Accumulator, idle: bool) -> Option<f64> {
        // Clipping: down at once (a burst of light clipping waits a while).
        let clipping = acc.clipping();
        let step = match clipping {
            0 => {
                self.strikes = 0;
                0
            }
            c if c < HEAVY_CLIPPING => {
                self.strikes += 1;
                if self.strikes < LIGHT_CLIPPING_WINDOWS {
                    0
                } else {
                    1
                }
            }
            c if c < 4 * HEAVY_CLIPPING => 2,
            _ => 3,
        };
        if step > 0 && self.index > 0 {
            self.strikes = 0;
            let from = self.table[self.index];
            let index = self.index.saturating_sub(step);
            self.note = Some(format!(
                "飽和したため {from:.1} → {:.1} dB に下げました",
                self.table[index]
            ));
            // The floor follows the gain: measure it anew.
            self.baseline = None;
            self.drift = 0;
            return Some(self.set_index(index));
        }
        let seconds = self.since_sweep as f64 / self.rate;
        if let Some(floor) = acc.floor_db() {
            match self.baseline {
                None => self.baseline = Some(floor),
                Some(base) if (floor - base).abs() > DRIFT_DB => self.drift += acc.samples,
                Some(_) => self.drift = 0,
            }
        }
        if idle && seconds >= RESWEEP_MIN_S && self.drift as f64 / self.rate >= DRIFT_S {
            return Some(self.start_sweep(Some("雑音の量が変わった")));
        }
        if idle && seconds >= RESWEEP_EVERY_S {
            return Some(self.start_sweep(Some("一定時間たった")));
        }
        None
    }
}

/// The sweep's choice: the lowest gain whose floor stands [`FLOOR_RISE_DB`]
/// above the lowest gain's, not past intermodulation, clipping or
/// [`PEAK_LIMIT`]; and that floor rise.
fn choose(table: &[f64], points: &[Point]) -> (usize, Option<f64>) {
    let usable: Vec<&Point> = points.iter().filter(|p| p.floor_db.is_finite()).collect();
    let Some(adc) = usable.first().map(|p| p.floor_db) else {
        // No floor at all: the highest gain that neither clips nor peaks.
        let index = points
            .iter()
            .take_while(|p| p.clipping == 0 && p.peak <= PEAK_LIMIT)
            .last()
            .map_or(points[0].index, |p| p.index);
        return (index, None);
    };
    let mut choice: Option<&Point> = None;
    for (i, p) in usable.iter().enumerate() {
        if p.clipping > 0 || p.peak > PEAK_LIMIT {
            break;
        }
        if i > 0 {
            let q = usable[i - 1];
            let slope = (p.floor_db - q.floor_db) / (table[p.index] - table[q.index]);
            if slope > IMD_SLOPE && p.floor_db - adc > 3.0 {
                break;
            }
        }
        choice = Some(p);
        if p.floor_db - adc >= FLOOR_RISE_DB {
            break;
        }
    }
    match choice {
        Some(p) => (p.index, Some(p.floor_db - adc)),
        // Even the lowest gain clips: start there, clipping steps lower.
        None => (usable[0].index, None),
    }
}

/// Bins next to a receive channel's that still see its signal: each bin
/// passes about ±12 kHz, two bin spacings.
const CHANNEL_SPREAD_BINS: i64 = 3;

/// The noise floor of a block from the channelizer's per-bin power (index
/// k: k bins above the centre, wrapping): a low percentile over bins at
/// least two away from DC and [`CHANNEL_SPREAD_BINS`] away from every
/// receive channel's bin, within 85 % of the band. (The dongle's filters
/// roll off near the edges, but they attenuate the ADC's noise and the
/// tuner's alike, so the floor still follows the gain the same way.)
pub fn noise_floor_db(power: &[f64], channel_bins: &[usize]) -> Option<f64> {
    let m = power.len() as i64;
    if m < 8 {
        return None;
    }
    let edge = (m as f64 * 0.425).floor() as i64;
    let mut levels: Vec<f64> = (0..m)
        .filter_map(|k| {
            let signed = if k < m / 2 { k } else { k - m };
            let near = channel_bins.iter().any(|&c| {
                let d = (k - c as i64).rem_euclid(m);
                d <= CHANNEL_SPREAD_BINS || d >= m - CHANNEL_SPREAD_BINS
            });
            let p = power[k as usize];
            (signed.abs() >= 2 && signed.abs() <= edge && !near && p > 0.0)
                .then(|| 10.0 * p.log10())
        })
        .collect();
    if levels.len() < 3 {
        return None;
    }
    levels.sort_by(f64::total_cmp);
    Some(levels[levels.len() / 4])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtl_tcp::GAINS_TENTH_DB;

    const RATE: f64 = 240_000.0;

    /// A receiver whose floor is the ADC's (-60 dB) plus the tuner's noise
    /// (-90 dB + gain), plus intermodulation rising 3 dB a dB above
    /// `imd_from`, clipping above `clip_from`.
    struct Model {
        imd_from: f64,
        clip_from: f64,
        /// Extra floor (interference), dB.
        extra: f64,
    }
    impl Model {
        fn window(&self, gain: f64) -> Window {
            let tuner = -90.0 + gain;
            let imd = -75.0 + 3.0 * (gain - self.imd_from);
            let floor = self.extra
                + 10.0
                    * (10f64.powf(-6.0) + 10f64.powf(tuner / 10.0) + 10f64.powf(imd / 10.0))
                        .log10();
            let samples = (RATE * 0.04) as u64;
            Window {
                floor_db: Some(floor),
                clipped: if gain > self.clip_from {
                    samples / 20
                } else {
                    0
                },
                samples,
                peak: (0.002 * 10f64.powf(gain / 20.0)).min(1.0) as f32,
            }
        }
    }
    /// Runs the optimizer against `model` for `seconds`; returns the gains
    /// it set.
    fn run(
        model: &Model,
        opt: &mut GainOptimizer,
        gain: &mut f64,
        seconds: f64,
        idle: bool,
    ) -> Vec<f64> {
        let mut set = Vec::new();
        for _ in 0..(seconds / 0.04) as usize {
            if let Some(g) = opt.observe(&model.window(*gain), idle) {
                *gain = g;
                set.push(g);
            }
        }
        set
    }

    #[test]
    fn the_sweep_picks_the_knee_where_tuner_noise_overtakes_the_adc() {
        let model = Model {
            imd_from: 100.0,
            clip_from: 100.0,
            extra: 0.0,
        };
        let (mut opt, mut gain) = GainOptimizer::new(&GAINS_TENTH_DB, RATE).unwrap();
        assert_eq!(opt.status().phase, "sweeping");
        let set = run(&model, &mut opt, &mut gain, 20.0, true);
        assert_eq!(opt.status().phase, "tracking");
        // About 11 points at 1.25 s each.
        assert!(set.len() >= 10, "{set:?}");
        // ADC floor -60 dB; tuner noise -90 + g: 8 dB above needs g ≈ 37.
        assert!((36.0..=41.0).contains(&gain), "{gain}");
        let rise = opt.status().floor_rise_db.unwrap();
        assert!(rise >= FLOOR_RISE_DB, "{rise}");
        // Then it stays put.
        assert!(run(&model, &mut opt, &mut gain, 60.0, true).is_empty());
    }

    #[test]
    fn intermodulation_and_clipping_keep_the_gain_lower() {
        // Strong out-of-band stations: the floor shoots up from 25 dB on.
        let imd = Model {
            imd_from: 25.0,
            clip_from: 100.0,
            extra: 0.0,
        };
        let (mut opt, mut gain) = GainOptimizer::new(&GAINS_TENTH_DB, RATE).unwrap();
        run(&imd, &mut opt, &mut gain, 20.0, true);
        assert!(gain <= 30.0, "{gain}");
        // Clipping from 30 dB: the choice stays below.
        let clip = Model {
            imd_from: 100.0,
            clip_from: 30.0,
            extra: 0.0,
        };
        let (mut opt, mut gain) = GainOptimizer::new(&GAINS_TENTH_DB, RATE).unwrap();
        run(&clip, &mut opt, &mut gain, 20.0, true);
        assert!(gain <= 30.0, "{gain}");
    }

    #[test]
    fn clipping_while_tracking_steps_down_at_once() {
        let mut model = Model {
            imd_from: 100.0,
            clip_from: 100.0,
            extra: 0.0,
        };
        let (mut opt, mut gain) = GainOptimizer::new(&GAINS_TENTH_DB, RATE).unwrap();
        run(&model, &mut opt, &mut gain, 20.0, true);
        let chosen = gain;
        // A strong station appears.
        model.clip_from = chosen - 1.0;
        let set = run(&model, &mut opt, &mut gain, 10.0, false);
        assert!(!set.is_empty() && gain < chosen, "{set:?}");
        assert!(opt.status().note.unwrap().contains("飽和したため"));
        assert_eq!(opt.status().phase, "tracking");
    }

    #[test]
    fn a_moved_floor_resweeps_only_when_idle_and_not_too_soon() {
        let model = Model {
            imd_from: 100.0,
            clip_from: 100.0,
            extra: 0.0,
        };
        let (mut opt, mut gain) = GainOptimizer::new(&GAINS_TENTH_DB, RATE).unwrap();
        run(&model, &mut opt, &mut gain, 20.0, true);
        // The floor jumps 10 dB (new interference) for a minute: within five
        // minutes of the sweep nothing happens.
        let louder = Model {
            imd_from: 100.0,
            clip_from: 100.0,
            extra: 10.0,
        };
        assert!(run(&louder, &mut opt, &mut gain, 60.0, true).is_empty());
        // Later, during a call: still nothing.
        assert!(run(&louder, &mut opt, &mut gain, 300.0, false).is_empty());
        // Idle: a new sweep starts.
        let set = run(&louder, &mut opt, &mut gain, 1.0, true);
        assert_eq!(set.len(), 1);
        assert_eq!(opt.status().phase, "sweeping");
        assert!(opt.status().note.unwrap().contains("雑音の量が変わった"));
    }

    #[test]
    fn the_floor_skips_dc_band_edges_and_channel_bins() {
        let mut power = vec![1e-6; 40];
        power[0] = 1.0; // DC spike
        power[2..=8].fill(1.0); // a channel at bin 5 and its spread
        power[20] = 1.0; // the band edge
        for k in [13, 14, 15, 16, 24, 25, 26, 27] {
            power[k] = 1e-7;
        }
        // Bins 2–17 and 23–38 qualify, except around the channel (2–8).
        let floor = noise_floor_db(&power, &[5]).unwrap();
        assert!((-70.1..=-69.9).contains(&floor), "{floor}");
        let none_near = noise_floor_db(&power, &[]).unwrap();
        assert!(none_near < 0.0);
        assert_eq!(noise_floor_db(&power[..4], &[]), None);
    }
}

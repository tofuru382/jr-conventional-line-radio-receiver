//! Android monitor playback through AAudio.
//!
//! The WebView's timers are throttled once the app is in the background, so on
//! Android the selected channel is played here instead of through Web Audio.
//! The UI only reports what to play; a dedicated thread drains the session's
//! monitor queue and writes PCM to the device, with the screen on or off.
//!
//! The decoder produces 8 kHz PCM. Rather than asking the device for an 8 kHz
//! stream (some devices route such low-rate streams through paths that glitch),
//! the stream opens at the device's own rate and the PCM is resampled here.
use std::sync::{Arc, Mutex};

/// Sample rate of the decoder's PCM.
pub const INPUT_RATE: u32 = 8000;

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct Settings {
    /// "Listen" is on and a session is receiving.
    pub enabled: bool,
    pub selected: Option<String>,
    pub automatic: bool,
    /// Linear gain, 0.0–1.0 (the UI's volume slider / 100).
    pub volume: f32,
    /// The WebView's Web Audio player owns the monitor queue: do not drain it.
    #[serde(default)]
    pub inactive: bool,
}

/// Playback health shown in the app (to tell choppy reception from choppy
/// playback).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Stats {
    /// Times playback ran dry while a call was still arriving.
    pub underruns: u32,
    /// Current jitter buffer target in milliseconds (grows after underruns).
    pub buffer_ms: u32,
    /// Output sample rate of the open stream (0 when closed).
    pub rate: u32,
}

#[derive(Default, Clone)]
pub struct NativeMonitor {
    settings: Arc<Mutex<Settings>>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    stats: Arc<Mutex<Stats>>,
    /// A test sound to play once, independent of reception and "Listen".
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    test: Arc<Mutex<Vec<i16>>>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    started: Arc<std::sync::OnceLock<()>>,
}

impl NativeMonitor {
    pub fn update(&self, settings: Settings, receiver: &crate::Receiver) -> Result<(), String> {
        *self.settings.lock().map_err(|e| e.to_string())? = settings;
        self.start(receiver)
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Plays [`test_tone`] through the same output path as received audio.
    pub fn play_test(&self, receiver: &crate::Receiver) -> Result<(), String> {
        *self.test.lock().map_err(|e| e.to_string())? = test_tone();
        self.start(receiver)
    }

    fn start(&self, receiver: &crate::Receiver) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            let (settings, test, stats, receiver) = (
                self.settings.clone(),
                self.test.clone(),
                self.stats.clone(),
                receiver.clone(),
            );
            self.started.get_or_init(move || {
                std::thread::Builder::new()
                    .name("monitor-audio".into())
                    .spawn(move || loop {
                        // An audio-driver panic must not silence the app for
                        // the rest of the process; restart the player instead.
                        let (settings, test, stats, receiver) = (
                            settings.clone(),
                            test.clone(),
                            stats.clone(),
                            receiver.clone(),
                        );
                        let run = std::panic::AssertUnwindSafe(move || {
                            android::run(settings, test, stats, receiver)
                        });
                        let _ = std::panic::catch_unwind(run);
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    })
                    .expect("Unable to start the audio monitor thread");
            });
            Ok(())
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = receiver;
            Err("端末側の音声出力は Android 版のみ対応しています。".into())
        }
    }
}

/// Three rising tones ("pon, pon, pon"), 8 kHz. Heard cleanly and at the
/// right pitch, the output path itself is fine.
pub fn test_tone() -> Vec<i16> {
    let rate = INPUT_RATE as f32;
    let tone = (rate * 0.3) as usize;
    let gap = (rate * 0.1) as usize;
    let fade = (rate * 0.015) as usize;
    let mut pcm = Vec::with_capacity(3 * (tone + gap));
    for hz in [523.25f32, 659.25, 783.99] {
        for i in 0..tone {
            let edge = (i.min(tone - 1 - i) as f32 / fade as f32).min(1.0);
            let s = (std::f32::consts::TAU * hz * i as f32 / rate).sin();
            pcm.push((s * edge * 0.5 * 32767.0) as i16);
        }
        pcm.extend(std::iter::repeat_n(0, gap));
    }
    pcm
}

/// Streaming band-limited resampler (windowed sinc) for 8 kHz voice PCM.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub struct Resampler {
    input: u64,
    output: u64,
    /// Position of the next output: `history[base]` plus `phase / output`
    /// input samples (kept exact so chunking never changes the result).
    base: usize,
    phase: u64,
    history: Vec<f32>,
    passthrough: bool,
}

/// Kernel half-width in input samples (1 ms of latency at 8 kHz).
const HALF: usize = 8;
/// Low-pass cutoff relative to the input Nyquist frequency (3.6 kHz).
const CUTOFF: f64 = 0.9;

fn kernel(x: f64) -> f64 {
    if x.abs() >= HALF as f64 {
        return 0.0;
    }
    let t = std::f64::consts::PI * CUTOFF * x;
    let sinc = if t == 0.0 { 1.0 } else { t.sin() / t };
    let window = 0.5 + 0.5 * (std::f64::consts::PI * x / HALF as f64).cos();
    CUTOFF * sinc * window
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
impl Resampler {
    pub fn new(input: u32, output: u32) -> Self {
        let mut r = Self {
            input: input as u64,
            output: output as u64,
            base: 0,
            phase: 0,
            history: Vec::new(),
            passthrough: input == output,
        };
        r.reset();
        r
    }

    /// Forgets buffered input (call after a discontinuity).
    pub fn reset(&mut self) {
        self.history.clear();
        self.history.resize(HALF, 0.0);
        self.base = HALF;
        self.phase = 0;
    }

    /// Input received but not yet fully output.
    pub fn has_tail(&self) -> bool {
        !self.passthrough && self.history.len() > HALF
    }

    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if self.passthrough {
            return input.to_vec();
        }
        self.history.extend(input.iter().map(|&s| s as f32));
        let mut out =
            Vec::with_capacity(input.len() * self.output as usize / self.input as usize + 2);
        while self.base + HALF < self.history.len() {
            let fraction = self.phase as f64 / self.output as f64;
            let mut sum = 0.0;
            for k in self.base + 1 - HALF..=self.base + HALF {
                sum += self.history[k] as f64 * kernel(fraction + (self.base as f64 - k as f64));
            }
            out.push(sum.round().clamp(-32768.0, 32767.0) as i16);
            self.phase += self.input;
            self.base += (self.phase / self.output) as usize;
            self.phase %= self.output;
        }
        let consumed = (self.base + 1).saturating_sub(HALF).min(self.history.len());
        self.history.drain(..consumed);
        self.base -= consumed;
        out
    }

    /// Outputs the buffered tail (end of a talk spurt) and resets.
    pub fn flush(&mut self) -> Vec<i16> {
        if !self.has_tail() {
            self.reset();
            return Vec::new();
        }
        let out = self.process(&[0; HALF]);
        self.reset();
        out
    }
}

/// Jitter buffer for live playback, independent of the audio device so it can
/// be tested. Decoded PCM (8 kHz) arrives in bursts: RTL-TCP delivers ~0.5 s at
/// once, synthesis looks ahead, and busy DSP threads fall behind for a moment.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod playout {
    use std::{
        collections::VecDeque,
        time::{Duration, Instant},
    };
    const MS: usize = (super::INPUT_RATE as usize) / 1000;
    /// Initial amount collected before (re)starting playback.
    pub const START: usize = 400 * MS;
    /// Added after each underrun in the middle of a call, up to `MAX`.
    pub const STEP: usize = 300 * MS;
    pub const MAX: usize = 1600 * MS;
    /// Start anyway once input has paused this long: the end of a short
    /// call. Longer than the gap between RTL-TCP bursts (~0.5 s), so a burst
    /// gap does not start playback before the buffer target is reached.
    pub const WAIT: Duration = Duration::from_millis(700);
    /// Audio resuming this soon after playback ran dry belongs to the same
    /// call: the buffer was too small.
    pub const RESUME: Duration = Duration::from_millis(1500);
    /// Written per turn (100 ms), so mute and channel changes apply soon.
    pub const CHUNK: usize = 100 * MS;
    /// A burst this short, with at least `QUIET` of silence before it and
    /// nothing after it, is a noise frame accepted as voice ("ブツ").
    pub const NOISE_BURST: usize = 160 * MS;
    pub const QUIET: Duration = Duration::from_secs(1);

    #[derive(Debug, PartialEq)]
    pub enum Step {
        /// Nothing to do yet.
        Wait,
        /// Write this PCM (8 kHz) to the device.
        Play(Vec<i16>),
        /// Input ended a while ago and everything was handed to the device.
        Idle,
    }
    pub struct Playout {
        pending: VecDeque<i16>,
        playing: bool,
        target: usize,
        last_audio: Option<Instant>,
        /// Last time audio arrived or the device was still playing.
        last_active: Option<Instant>,
        /// When playback last ran dry.
        dry_at: Option<Instant>,
        /// The PCM waiting to start follows a quiet second (may be noise).
        isolated: bool,
        pub underruns: u32,
    }
    impl Default for Playout {
        fn default() -> Self {
            Self {
                pending: VecDeque::new(),
                playing: false,
                target: START,
                last_audio: None,
                last_active: None,
                dry_at: None,
                isolated: false,
                underruns: 0,
            }
        }
    }
    impl Playout {
        pub fn target_ms(&self) -> u32 {
            (self.target / MS) as u32
        }
        pub fn push(&mut self, pcm: impl IntoIterator<Item = i16>, now: Instant) {
            let before = self.pending.len();
            self.pending.extend(pcm);
            if self.pending.len() == before {
                return;
            }
            if let Some(dry) = self.dry_at.take() {
                if now.duration_since(dry) < RESUME {
                    // Starved in the middle of a call: buffer more next time.
                    self.underruns += 1;
                    self.target = (self.target + STEP).min(MAX);
                }
            }
            if before == 0 && !self.playing {
                self.isolated = self
                    .last_active
                    .is_none_or(|t| now.duration_since(t) >= QUIET);
            }
            self.last_audio = Some(now);
            self.last_active = Some(now);
        }
        /// Muted or switched away: drop everything at once.
        pub fn clear(&mut self) {
            self.pending.clear();
            self.playing = false;
            self.dry_at = None;
        }
        /// `queued` and `low_water` are in device frames; `low_water` is the
        /// level below which the device has effectively run dry.
        pub fn step(&mut self, now: Instant, queued: i64, low_water: i64) -> Step {
            if self.playing || queued > 0 {
                self.last_active = Some(now);
            }
            let paused = self
                .last_audio
                .is_none_or(|t| now.duration_since(t) >= WAIT);
            if self.playing && self.pending.is_empty() && queued < low_water {
                self.playing = false;
                self.dry_at = Some(now);
            }
            if !self.playing
                && self.isolated
                && paused
                && !self.pending.is_empty()
                && self.pending.len() < NOISE_BURST
            {
                self.pending.clear();
            }
            if !self.playing
                && !self.pending.is_empty()
                && (self.pending.len() >= self.target || paused)
            {
                self.playing = true;
                self.isolated = false;
            }
            if self.playing && !self.pending.is_empty() {
                let n = self.pending.len().min(CHUNK);
                return Step::Play(self.pending.drain(..n).collect());
            }
            if self.pending.is_empty() && paused {
                Step::Idle
            } else {
                Step::Wait
            }
        }
    }
}

#[cfg(target_os = "android")]
mod android {
    use super::{
        playout::{Playout, Step},
        Resampler, Settings, Stats, INPUT_RATE,
    };
    use ndk::audio::{
        AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStream,
        AudioStreamBuilder,
    };
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    const IDLE_CLOSE: Duration = Duration::from_secs(10);
    /// Device-side buffer. The DSP threads keep the CPU busy; a deep device
    /// buffer rides out moments when this thread is not scheduled in time.
    const DEVICE_BUFFER_MS: i32 = 300;

    struct Output {
        stream: AudioStream,
        resampler: Resampler,
        /// Device frames below which playback has effectively run dry (40 ms).
        low_water: i64,
    }

    fn open() -> Result<Output, String> {
        // No sample rate: the device's native rate, converted by `Resampler`.
        let stream = AudioStreamBuilder::new()
            .and_then(|b| {
                b.direction(AudioDirection::Output)
                    .sharing_mode(AudioSharingMode::Shared)
                    .performance_mode(AudioPerformanceMode::None)
                    .channel_count(1)
                    .format(AudioFormat::PCM_I16)
                    // About one second at 48 kHz; the device may grant less.
                    .buffer_capacity_in_frames(48_000)
                    // Usage/content type need API 28; the default is media.
                    .open_stream()
            })
            .map_err(|e| format!("{e:?}"))?;
        let rate = stream.sample_rate();
        if rate <= 0 || stream.channel_count() != 1 || stream.format() != AudioFormat::PCM_I16 {
            return Err(format!(
                "unexpected stream: {rate} Hz, {} ch, {:?}",
                stream.channel_count(),
                stream.format()
            ));
        }
        let wanted = (rate * DEVICE_BUFFER_MS / 1000).min(stream.buffer_capacity_in_frames());
        let _ = stream.set_buffer_size_in_frames(wanted);
        stream.request_start().map_err(|e| format!("{e:?}"))?;
        Ok(Output {
            stream,
            resampler: Resampler::new(INPUT_RATE, rate as u32),
            low_water: rate as i64 * 40 / 1000,
        })
    }

    /// Frames written to the device but not yet played.
    fn queued(stream: &AudioStream) -> i64 {
        (stream.frames_written() - stream.frames_read()).max(0)
    }

    /// Blocking write; the device's consumption paces the loop.
    fn write(stream: &AudioStream, pcm: &[i16]) -> Result<(), String> {
        let mut offset = 0;
        while offset < pcm.len() {
            let rest = &pcm[offset..];
            // SAFETY: `rest` is a valid i16 mono buffer of `rest.len()` frames,
            // matching the stream's PCM_I16 / 1-channel format.
            let written =
                unsafe { stream.write(rest.as_ptr().cast(), rest.len() as i32, 500_000_000) }
                    .map_err(|e| format!("{e:?}"))? as usize;
            if written == 0 {
                return Err("audio device stalled".into());
            }
            offset += written;
        }
        Ok(())
    }

    pub fn run(
        settings: Arc<Mutex<Settings>>,
        test: Arc<Mutex<Vec<i16>>>,
        stats: Arc<Mutex<Stats>>,
        receiver: crate::Receiver,
    ) {
        // Audio priority (Android's THREAD_PRIORITY_AUDIO), so the busy DSP
        // threads do not starve playback. Best effort.
        // SAFETY: plain syscall on this thread's id.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, -16);
        }
        let mut output: Option<Output> = None;
        let mut playout = Playout::default();
        let mut listening = false;
        let mut idle_since: Option<Instant> = None;
        loop {
            let current = settings.lock().map(|s| s.clone()).unwrap_or_default();
            // Drain the queue unless Web Audio owns it, so unheard PCM never
            // piles up and replays later.
            let audio = if current.inactive {
                Vec::new()
            } else {
                match receiver.0.lock() {
                    Ok(guard) => guard
                        .as_ref()
                        .map(|s| {
                            s.take_monitor_audio(current.selected.as_deref(), current.automatic)
                        })
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                }
            };
            let now_listening = current.enabled && !current.inactive;
            if listening && !now_listening {
                // Muted or switched to Web Audio: stop at once.
                playout.clear();
                if let Some(o) = output.as_mut() {
                    o.resampler.reset();
                }
            }
            listening = now_listening;
            let now = Instant::now();
            if listening && !audio.is_empty() {
                let gain = current.volume.clamp(0.0, 1.0);
                playout.push(
                    audio
                        .iter()
                        .flat_map(|f| f.pcm.iter().map(move |&s| (s as f32 * gain) as i16)),
                    now,
                );
            }
            let tone = test
                .lock()
                .map(|mut t| std::mem::take(&mut *t))
                .unwrap_or_default();
            playout.push(tone, now);
            let (queued, low_water) = output
                .as_ref()
                .map_or((0, 1), |o| (queued(&o.stream), o.low_water));
            let step = playout.step(now, queued, low_water);
            if let Ok(mut s) = stats.lock() {
                s.underruns = playout.underruns;
                s.buffer_ms = playout.target_ms();
                s.rate = output.as_ref().map_or(0, |o| o.stream.sample_rate() as u32);
            }
            let pcm = match step {
                Step::Play(pcm) => {
                    idle_since = None;
                    pcm
                }
                Step::Idle => {
                    if let Some(o) = output.as_mut() {
                        // The talk spurt ended: play the resampler's last 1 ms.
                        if o.resampler.has_tail() {
                            let tail = o.resampler.flush();
                            if write(&o.stream, &tail).is_err() {
                                output = None;
                            }
                        } else if idle_since.get_or_insert(now).elapsed() > IDLE_CLOSE {
                            output = None;
                            idle_since = None;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Step::Wait => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
            };
            if output.is_none() {
                match open() {
                    Ok(o) => output = Some(o),
                    Err(e) => {
                        eprintln!("audio monitor: cannot open output: {e}");
                        playout.clear();
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }
            let o = output.as_mut().unwrap();
            let pcm = o.resampler.process(&pcm);
            if let Err(e) = write(&o.stream, &pcm) {
                // e.g. headphones unplugged: reopen on the next audio.
                eprintln!("audio monitor: {e}");
                output = None;
                playout.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, rate: f64, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| ((std::f64::consts::TAU * hz * i as f64 / rate).sin() * 16000.0) as i16)
            .collect()
    }

    fn crossings(pcm: &[i16]) -> usize {
        pcm.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count()
    }

    #[test]
    fn resampling_keeps_pitch_length_and_level() {
        for rate in [44_100u32, 48_000] {
            let input = tone(1000.0, 8000.0, 8000);
            let mut r = Resampler::new(8000, rate);
            let mut out = r.process(&input);
            out.extend(r.flush());
            let expected = rate as usize;
            assert!(out.len().abs_diff(expected) <= 2, "{rate}: {}", out.len());
            // One second of 1 kHz: about 2000 zero crossings at any rate.
            let middle = &out[rate as usize / 10..rate as usize * 9 / 10];
            assert!(
                crossings(middle).abs_diff(1600) <= 4,
                "{}",
                crossings(middle)
            );
            let peak = middle.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!((15_000..=17_000).contains(&peak), "{rate}: peak {peak}");
        }
    }

    #[test]
    fn chunked_input_matches_one_pass() {
        let input = tone(700.0, 8000.0, 4000);
        let mut whole = Resampler::new(8000, 48_000);
        let expected = whole.process(&input);
        let mut parts = Resampler::new(8000, 48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(320) {
            out.extend(parts.process(chunk));
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn equal_rates_pass_through() {
        let input = tone(440.0, 8000.0, 100);
        let mut r = Resampler::new(8000, 8000);
        assert_eq!(r.process(&input), input);
        assert!(r.flush().is_empty());
    }

    /// Simulates a real-time device consuming 8 kHz PCM in 10 ms ticks.
    /// `arrivals` are (time in ms, samples); returns (underruns, samples
    /// played, ticks during a call when the device was empty).
    fn simulate(
        playout: &mut playout::Playout,
        arrivals: &[(u64, usize)],
        end_ms: u64,
    ) -> (u32, usize, u32) {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut queued: i64 = 0;
        let (mut played, mut gaps) = (0usize, 0u32);
        let mut started = false;
        for ms in (0..end_ms).step_by(10) {
            let now = start + Duration::from_millis(ms);
            for &(_, n) in arrivals.iter().filter(|(t, _)| *t == ms) {
                playout.push(std::iter::repeat_n(1000i16, n), now);
            }
            if let playout::Step::Play(pcm) = playout.step(now, queued, 320) {
                queued += pcm.len() as i64;
                played += pcm.len();
                started = true;
            }
            let last = arrivals.last().unwrap().0;
            if started && queued == 0 && ms < last {
                gaps += 1;
            }
            queued = (queued - 80).max(0);
        }
        (playout.underruns, played, gaps)
    }

    #[test]
    fn late_bursts_grow_the_buffer_until_playback_is_continuous() {
        // A 20 s call delivered in 0.6 s bursts; every third burst is 0.5 s late
        // (busy DSP threads), so a fixed 0.4 s buffer keeps running dry.
        let mut arrivals = Vec::new();
        for k in 0..33u64 {
            let late = if k % 3 == 2 { 500 } else { 0 };
            arrivals.push((k * 600 + late, 4800));
        }
        let mut p = playout::Playout::default();
        let (underruns, played, _) = simulate(&mut p, &arrivals, 25_000);
        assert!((1..=3).contains(&underruns), "{underruns}");
        assert!(p.target_ms() > 400 && p.target_ms() <= 1600);
        assert_eq!(played, 33 * 4800, "nothing may be lost");
        // Once adapted, the same pattern plays without running dry.
        let mut again = Vec::new();
        for k in 0..33u64 {
            let late = if k % 3 == 2 { 500 } else { 0 };
            again.push((30_000 + k * 600 + late, 4800));
        }
        let before = p.underruns;
        let (after, _, _) = simulate(&mut p, &again, 55_000);
        assert_eq!(after, before);
    }

    #[test]
    fn isolated_noise_is_dropped_but_a_tail_after_speech_is_played() {
        // 80 ms alone after silence: a falsely accepted noise frame.
        let mut p = playout::Playout::default();
        let (_, played, _) = simulate(&mut p, &[(1000, 640)], 3000);
        assert_eq!(played, 0);
        // The same 80 ms 0.3 s after a sentence: the end of speech.
        let mut p = playout::Playout::default();
        let (_, played, _) = simulate(&mut p, &[(0, 8000), (1300, 640)], 4000);
        assert_eq!(played, 8640);
    }

    #[test]
    fn test_tone_is_three_audible_notes() {
        let pcm = test_tone();
        assert_eq!(pcm.len(), 8000 * 6 / 5);
        let loud = pcm.iter().filter(|s| s.unsigned_abs() > 8000).count();
        assert!(loud > 8000 / 2, "{loud}");
        assert_eq!(pcm[0], 0);
    }
}

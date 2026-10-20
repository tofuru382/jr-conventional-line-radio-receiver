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

#[derive(Default, Clone)]
pub struct NativeMonitor {
    settings: Arc<Mutex<Settings>>,
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

    /// Plays [`test_tone`] through the same output path as received audio.
    pub fn play_test(&self, receiver: &crate::Receiver) -> Result<(), String> {
        *self.test.lock().map_err(|e| e.to_string())? = test_tone();
        self.start(receiver)
    }

    fn start(&self, receiver: &crate::Receiver) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            let (settings, test, receiver) =
                (self.settings.clone(), self.test.clone(), receiver.clone());
            self.started.get_or_init(move || {
                std::thread::Builder::new()
                    .name("monitor-audio".into())
                    .spawn(move || loop {
                        // An audio-driver panic must not silence the app for
                        // the rest of the process; restart the player instead.
                        let (settings, test, receiver) =
                            (settings.clone(), test.clone(), receiver.clone());
                        let run = std::panic::AssertUnwindSafe(move || {
                            android::run(settings, test, receiver)
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

#[cfg(target_os = "android")]
mod android {
    use super::{Resampler, Settings, INPUT_RATE};
    use ndk::audio::{
        AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStream,
        AudioStreamBuilder,
    };
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    const IDLE_CLOSE: Duration = Duration::from_secs(10);
    /// Jitter buffer: decoded PCM arrives in bursts (synthesis lookahead,
    /// RTL-TCP delivering ~0.5 s at once). Collect this much before (re)starting
    /// playback so the device never runs dry in the middle of a word.
    const PREBUFFER: usize = (INPUT_RATE as usize) * 400 / 1000;
    /// Start anyway once input has paused this long (the end of a short call).
    const PREBUFFER_WAIT: Duration = Duration::from_millis(250);
    /// Written per loop turn (100 ms), so mute and channel changes apply soon.
    const CHUNK: usize = (INPUT_RATE as usize) / 10;
    /// A burst this short with nothing around it is a falsely accepted noise
    /// frame (heard as a "ブツ"), not speech: it is not played (160 ms).
    const NOISE_BURST: usize = (INPUT_RATE as usize) * 160 / 1000;

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
        receiver: crate::Receiver,
    ) {
        let mut output: Option<Output> = None;
        let mut last_audio = Instant::now();
        let mut pending: VecDeque<i16> = VecDeque::new();
        let mut playing = false;
        let mut listening = false;
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
                pending.clear();
                playing = false;
                if let Some(o) = output.as_mut() {
                    o.resampler.reset();
                }
            }
            listening = now_listening;
            if listening && !audio.is_empty() {
                last_audio = Instant::now();
                let gain = current.volume.clamp(0.0, 1.0);
                for frame in &audio {
                    pending.extend(frame.pcm.iter().map(|&s| (s as f32 * gain) as i16));
                }
            }
            let tone = test
                .lock()
                .map(|mut t| std::mem::take(&mut *t))
                .unwrap_or_default();
            if !tone.is_empty() {
                last_audio = Instant::now();
                pending.extend(tone);
            }
            // Ran dry (nothing pending and the device nearly empty): the next
            // audio is buffered again instead of being played frame by frame.
            if playing
                && pending.is_empty()
                && output
                    .as_ref()
                    .is_none_or(|o| queued(&o.stream) < o.low_water)
            {
                playing = false;
            }
            if !playing
                && !pending.is_empty()
                && pending.len() < NOISE_BURST
                && last_audio.elapsed() >= PREBUFFER_WAIT
            {
                pending.clear();
            }
            if !playing
                && !pending.is_empty()
                && (pending.len() >= PREBUFFER || last_audio.elapsed() >= PREBUFFER_WAIT)
            {
                playing = true;
            }
            if !playing || pending.is_empty() {
                if let Some(o) = output.as_mut() {
                    if pending.is_empty() && last_audio.elapsed() >= PREBUFFER_WAIT {
                        // The talk spurt ended: play the resampler's last 1 ms.
                        if o.resampler.has_tail() {
                            let tail = o.resampler.flush();
                            if write(&o.stream, &tail).is_err() {
                                output = None;
                            }
                        } else if last_audio.elapsed() > IDLE_CLOSE {
                            output = None;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            if output.is_none() {
                match open() {
                    Ok(o) => output = Some(o),
                    Err(e) => {
                        eprintln!("audio monitor: cannot open output: {e}");
                        pending.clear();
                        playing = false;
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }
            let n = pending.len().min(CHUNK);
            let chunk: Vec<i16> = pending.drain(..n).collect();
            let o = output.as_mut().unwrap();
            let pcm = o.resampler.process(&chunk);
            if let Err(e) = write(&o.stream, &pcm) {
                // e.g. headphones unplugged: reopen on the next audio.
                eprintln!("audio monitor: {e}");
                output = None;
                playing = false;
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

    #[test]
    fn test_tone_is_three_audible_notes() {
        let pcm = test_tone();
        assert_eq!(pcm.len(), 8000 * 6 / 5);
        let loud = pcm.iter().filter(|s| s.unsigned_abs() > 8000).count();
        assert!(loud > 8000 / 2, "{loud}");
        assert_eq!(pcm[0], 0);
    }
}

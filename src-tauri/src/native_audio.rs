//! Android monitor playback through AAudio.
//!
//! The WebView's timers are throttled once the app is in the background, so on
//! Android the selected channel is played here instead of through Web Audio.
//! The UI only reports what to play; a dedicated thread drains the session's
//! monitor queue and writes PCM to the device, with the screen on or off.
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct Settings {
    /// "Listen" is on and a session is receiving.
    pub enabled: bool,
    pub selected: Option<String>,
    pub automatic: bool,
    /// Linear gain, 0.0–1.0 (the UI's volume slider / 100).
    pub volume: f32,
}

#[derive(Default, Clone)]
pub struct NativeMonitor {
    settings: Arc<Mutex<Settings>>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    started: Arc<std::sync::OnceLock<()>>,
}

impl NativeMonitor {
    pub fn update(&self, settings: Settings, receiver: &crate::Receiver) -> Result<(), String> {
        *self.settings.lock().map_err(|e| e.to_string())? = settings;
        #[cfg(target_os = "android")]
        {
            let (settings, receiver) = (self.settings.clone(), receiver.clone());
            self.started.get_or_init(move || {
                std::thread::Builder::new()
                    .name("monitor-audio".into())
                    .spawn(move || loop {
                        // An audio-driver panic must not silence the app for
                        // the rest of the process; restart the player instead.
                        let (settings, receiver) = (settings.clone(), receiver.clone());
                        let run =
                            std::panic::AssertUnwindSafe(move || android::run(settings, receiver));
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

#[cfg(target_os = "android")]
mod android {
    use super::Settings;
    use ndk::audio::{
        AudioContentType, AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode,
        AudioStream, AudioStreamBuilder, AudioUsage,
    };
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    const SAMPLE_RATE: i32 = 8000;
    const IDLE_CLOSE: Duration = Duration::from_secs(10);

    fn open() -> Result<AudioStream, String> {
        let stream = AudioStreamBuilder::new()
            .and_then(|b| {
                b.direction(AudioDirection::Output)
                    .sharing_mode(AudioSharingMode::Shared)
                    .performance_mode(AudioPerformanceMode::PowerSaving)
                    .sample_rate(SAMPLE_RATE)
                    .channel_count(1)
                    .format(AudioFormat::PCM_I16)
                    .usage(AudioUsage::Media)
                    .content_type(AudioContentType::Speech)
                    .open_stream()
            })
            .map_err(|e| format!("{e:?}"))?;
        stream.request_start().map_err(|e| format!("{e:?}"))?;
        Ok(stream)
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

    pub fn run(settings: Arc<Mutex<Settings>>, receiver: crate::Receiver) {
        let mut stream: Option<AudioStream> = None;
        let mut last_audio = Instant::now();
        let mut scaled = Vec::new();
        loop {
            let current = settings.lock().map(|s| s.clone()).unwrap_or_default();
            // Always drain the queue so unheard PCM never piles up and replays
            // later; it is only written to the device while listening.
            let audio = match receiver.0.lock() {
                Ok(guard) => guard
                    .as_ref()
                    .map(|s| s.take_monitor_audio(current.selected.as_deref(), current.automatic))
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            if !current.enabled || audio.is_empty() {
                if stream.is_some() && (!current.enabled || last_audio.elapsed() > IDLE_CLOSE) {
                    stream = None;
                }
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            last_audio = Instant::now();
            if stream.is_none() {
                match open() {
                    Ok(s) => stream = Some(s),
                    Err(e) => {
                        eprintln!("audio monitor: cannot open output: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }
            let gain = current.volume.clamp(0.0, 1.0);
            for frame in audio {
                scaled.clear();
                scaled.extend(frame.pcm.iter().map(|&s| (s as f32 * gain) as i16));
                if let Err(e) = write(stream.as_ref().unwrap(), &scaled) {
                    // e.g. headphones unplugged: reopen on the next frame.
                    eprintln!("audio monitor: {e}");
                    stream = None;
                    break;
                }
            }
        }
    }
}

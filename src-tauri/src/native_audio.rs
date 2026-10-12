//! Android monitor playback through AAudio.
//!
//! The WebView's timers are throttled once the app is in the background, so on
//! Android the selected channel is played here instead of through Web Audio.
//! The UI only reports what to play; a dedicated thread drains the session's
//! monitor queue and writes PCM to the device, with the screen on or off.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

/// Set when the audio route is lost (headphones unplugged, Bluetooth dropped).
/// Playback then stays silent rather than falling back to the loudspeaker,
/// until the user turns listening on again.
static OUTPUT_LOST: AtomicBool = AtomicBool::new(false);
/// Counts route losses so the UI can notice each one exactly once.
static OUTPUT_LOST_EVENTS: AtomicU64 = AtomicU64::new(0);

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn mark_output_lost() {
    OUTPUT_LOST.store(true, Ordering::SeqCst);
    OUTPUT_LOST_EVENTS.fetch_add(1, Ordering::SeqCst);
}
pub fn output_lost_events() -> u64 {
    OUTPUT_LOST_EVENTS.load(Ordering::SeqCst)
}

/// Called by NativeBridge.audioBecomingNoisy() (Kotlin) when Android reports
/// AudioManager.ACTION_AUDIO_BECOMING_NOISY.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_io_jrradio_receiver_NativeBridge_audioBecomingNoisy(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    mark_output_lost();
}

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
        {
            let mut current = self.settings.lock().map_err(|e| e.to_string())?;
            // Turning listening back on is the user's consent to play again.
            if settings.enabled && !current.enabled {
                OUTPUT_LOST.store(false, Ordering::SeqCst);
            }
            *current = settings;
        }
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
    use super::{mark_output_lost, Settings, OUTPUT_LOST};
    use ndk::audio::{
        AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStream,
        AudioStreamBuilder,
    };
    use std::{
        sync::{atomic::Ordering, Arc, Mutex},
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
                    // Usage/content type need API 28; the default is media.
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
            let enabled = current.enabled && !OUTPUT_LOST.load(Ordering::SeqCst);
            if !enabled || audio.is_empty() {
                if stream.is_some() && (!enabled || last_audio.elapsed() > IDLE_CLOSE) {
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
                    // Usually the output device went away. Never reopen on
                    // whatever route is left (the loudspeaker) by ourselves.
                    eprintln!("audio monitor: {e}");
                    stream = None;
                    mark_output_lost();
                    break;
                }
            }
        }
    }
}

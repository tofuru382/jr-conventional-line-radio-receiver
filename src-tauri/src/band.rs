//! Android: the band map in the ongoing notification. The notification is
//! drawn by the app's foreground service (Kotlin), which asks this module for
//! the live state once a second, also while the WebView sleeps in the
//! background. The layout (every channel of the preset, received or not)
//! comes from the page.
use crate::{native_audio::NativeMonitor, Receiver};

/// `{"state": "receiving" | "reconnecting" | …, "heard": id or null,
/// "channels": {id: "locked" | "calling" | "error" | "idle"}}` for the
/// received channels, or `{"state": "stopped"}` without a session.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn state_json(receiver: &Receiver, monitor: &NativeMonitor) -> String {
    let Some(session) = receiver.current() else {
        return r#"{"state":"stopped"}"#.into();
    };
    // The channel the native player plays (the page's choice, or the
    // automatic one), as the page would show it. Status only: the monitor
    // audio is left to the player.
    let settings = monitor.settings();
    let status = session.status(settings.selected.as_deref(), settings.automatic);
    drop(session);
    let channels: serde_json::Map<String, serde_json::Value> = status
        .channels
        .iter()
        .map(|c| {
            let id = c.config.id.clone();
            let state = if c.error.is_some() {
                "error"
            } else if status.active_calls.contains(&id) {
                "calling"
            } else if c.status.frame_locked {
                "locked"
            } else {
                "idle"
            };
            (id, state.into())
        })
        .collect();
    let state = if status.reconnecting {
        "reconnecting"
    } else {
        status.state.as_str()
    };
    serde_json::json!({
        "state": state,
        "heard": status.selected_channel,
        "channels": channels,
    })
    .to_string()
}

/// The app's receiver and player, for the notification's JNI call.
#[cfg(target_os = "android")]
static SOURCES: std::sync::OnceLock<(Receiver, NativeMonitor)> = std::sync::OnceLock::new();
#[cfg(target_os = "android")]
pub fn register(receiver: Receiver, monitor: NativeMonitor) {
    let _ = SOURCES.set((receiver, monitor));
}

/// Stops reception and saving, from the notification (the page may be gone).
#[cfg(target_os = "android")]
fn stop() {
    let Some((receiver, _)) = SOURCES.get() else {
        return;
    };
    let Some(session) = receiver.current() else {
        return;
    };
    receiver_core::diag::note("通知から受信を停止しました");
    // Called on Android's main thread: signal the stop at once, then join the
    // workers and finalize the recording on a thread of its own, so the
    // notification never freezes the app (ANR) while files are closed.
    session.request_stop();
    std::thread::spawn(move || {
        session.stop();
        session.stop_recording();
    });
}

/// JNI entry point for io.jrradio.receiver.NativeBand (Kotlin).
#[cfg(target_os = "android")]
mod jni_entry {
    use jni::{objects::JClass, sys::jstring, JNIEnv};
    #[no_mangle]
    pub extern "system" fn Java_io_jrradio_receiver_NativeBand_stop<'local>(
        _env: JNIEnv<'local>,
        _class: JClass<'local>,
    ) {
        super::stop();
    }
    #[no_mangle]
    pub extern "system" fn Java_io_jrradio_receiver_NativeBand_state<'local>(
        env: JNIEnv<'local>,
        _class: JClass<'local>,
    ) -> jstring {
        let text = super::SOURCES
            .get()
            .map(|(receiver, monitor)| super::state_json(receiver, monitor))
            .unwrap_or_else(|| r#"{"state":"stopped"}"#.into());
        env.new_string(text)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_session_the_band_is_stopped() {
        let json: serde_json::Value =
            serde_json::from_str(&state_json(&Receiver::default(), &NativeMonitor::default()))
                .unwrap();
        assert_eq!(json["state"], "stopped");
    }
}

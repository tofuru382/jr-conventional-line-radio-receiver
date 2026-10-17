#[cfg(target_os = "macos")]
mod icon;
mod native_audio;
mod preferences;
mod recording_directory;

use receiver_core::{
    pipeline,
    session::{Config, Session},
    source,
};
use std::sync::{Arc, Mutex};
use tauri::Manager;

#[derive(Default, Clone)]
struct Receiver(Arc<Mutex<Option<Session>>>);
#[derive(Default, Clone)]
struct PreferencesStore(Arc<preferences::Store>);
#[tauri::command]
async fn load_preferences(
    app: tauri::AppHandle,
    state: tauri::State<'_, PreferencesStore>,
) -> Result<Option<preferences::Preferences>, String> {
    let store = state.0.clone();
    let path = preferences::path(&app)?;
    tauri::async_runtime::spawn_blocking(move || store.load(&path))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn save_preferences(
    settings: preferences::Preferences,
    app: tauri::AppHandle,
    state: tauri::State<'_, PreferencesStore>,
) -> Result<(), String> {
    let store = state.0.clone();
    let path = preferences::path(&app)?;
    tauri::async_runtime::spawn_blocking(move || store.save(&path, &settings))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn devices() -> Result<Vec<source::Device>, String> {
    tauri::async_runtime::spawn_blocking(source::enumerate)
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn presets(region: String) -> Result<Vec<pipeline::ChannelConfig>, String> {
    Ok(receiver_core::presets::resolve(&region)?.channels)
}
#[tauri::command]
async fn preset_catalog(
    path: Option<String>,
) -> Result<Vec<receiver_core::presets::ReceivePreset>, String> {
    tauri::async_runtime::spawn_blocking(move || match path {
        Some(path) => receiver_core::presets::load_file(std::path::Path::new(&path)),
        None => Ok(receiver_core::presets::catalog()?.to_vec()),
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn start(config: Config, state: tauri::State<'_, Receiver>) -> Result<(), String> {
    let receiver = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = receiver.lock().map_err(|e| e.to_string())?;
        if let Some(session) = guard.as_ref() {
            if session.snapshot(None).0.running {
                return Err("受信中です。停止してから設定を変更してください。".into());
            }
        }
        *guard = Some(Session::start(config)?);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn stop(
    state: tauri::State<'_, Receiver>,
) -> Result<receiver_core::recording::RecordingStatus, String> {
    let receiver = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = receiver.lock().map_err(|e| e.to_string())?;
        if let Some(s) = guard.as_mut() {
            s.stop();
            return Ok(s.stop_recording());
        }
        Ok(receiver_core::recording::RecordingStatus::default())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn default_recording_directory(app: tauri::AppHandle) -> Result<String, String> {
    recording_directory::default_directory(&app)?
        .into_os_string()
        .into_string()
        .map_err(|_| {
            "初期保存先の文字を読み取れませんでした。別の保存先フォルダーを選択してください。"
                .into()
        })
}
#[tauri::command]
async fn start_recording(
    directory: String,
    options: Option<receiver_core::recording::OutputOptions>,
    app: tauri::AppHandle,
    state: tauri::State<'_, Receiver>,
) -> Result<receiver_core::recording::RecordingStatus, String> {
    let receiver = state.0.clone();
    let default = recording_directory::default_directory(&app).ok();
    tauri::async_runtime::spawn_blocking(move || {
        let guard = receiver.lock().map_err(|e| e.to_string())?;
        let session = guard
            .as_ref()
            .ok_or("受信を開始してから録音してください。")?;
        let directory = std::path::Path::new(&directory);
        recording_directory::prepare(directory, default.as_deref())?;
        session.start_recording_with_options(directory, options.unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn stop_recording(
    state: tauri::State<'_, Receiver>,
) -> Result<receiver_core::recording::RecordingStatus, String> {
    let receiver = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let guard = receiver.lock().map_err(|e| e.to_string())?;
        Ok(guard
            .as_ref()
            .ok_or("録音中の受信セッションがありません。")?
            .stop_recording())
    })
    .await
    .map_err(|e| e.to_string())?
}
/// `audio: false` leaves the monitor PCM for the Android native player.
#[tauri::command]
fn snapshot(
    selected: Option<String>,
    automatic: Option<bool>,
    audio: Option<bool>,
    state: tauri::State<'_, Receiver>,
) -> Result<serde_json::Value, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    match guard.as_ref() {
        None => Ok(serde_json::Value::Null),
        Some(s) => {
            let (status, audio) = s.snapshot_with_options(
                selected.as_deref(),
                automatic.unwrap_or(false),
                audio.unwrap_or(true),
            );
            Ok(serde_json::json!({"status":status,"audio":audio}))
        }
    }
}
#[tauri::command]
fn set_native_monitor(
    settings: native_audio::Settings,
    receiver: tauri::State<'_, Receiver>,
    monitor: tauri::State<'_, native_audio::NativeMonitor>,
) -> Result<(), String> {
    monitor.update(settings, &receiver)
}
/// What the frontend needs to adapt to the host: Android has no SoapySDR and
/// no folder picker, but can start an on-device RTL-TCP driver app.
#[derive(serde::Serialize)]
struct Platform {
    android: bool,
    soapy: bool,
    /// The simplified JR-CLRR layout (Android and macOS builds of this fork).
    simple: bool,
    name: Option<&'static str>,
    /// "v1.3" when the lighter voice decoder (crates/jr-voice-lite) is used.
    voice_decoder: &'static str,
}
#[tauri::command]
fn platform() -> Platform {
    Platform {
        android: cfg!(target_os = "android"),
        soapy: cfg!(not(target_os = "android")),
        simple: cfg!(any(target_os = "android", target_os = "macos")),
        name: if cfg!(target_os = "android") {
            Some("JR-CLRR For Android")
        } else if cfg!(target_os = "macos") {
            Some("JR-CLRR For Mac")
        } else {
            None
        },
        voice_decoder: if cfg!(target_os = "android") {
            "v1.3"
        } else {
            "v1.5"
        },
    }
}
/// Starts the "Rtl-sdr driver" app (marto.rtl_tcp_andro), which owns the USB
/// dongle and serves it as an rtl_tcp server on this phone.
#[tauri::command]
fn start_rtl_driver(app: tauri::AppHandle, port: u16) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(format!("iqsrc://-a 127.0.0.1 -p {port} -n 1"), None::<&str>)
            .map_err(|e| {
                format!(
                    "RTL-SDR ドライバーを起動できませんでした。「Rtl-sdr driver」アプリがインストールされているか確認してください。{e}"
                )
            })
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, port);
        Err("ドライバーの起動は Android 版のみ対応しています。".into())
    }
}
/// Inside "JR-CLRR For Mac.app" the SoapySDR RTL-SDR module is bundled in
/// Contents/Resources/SoapySDR/modules0.8 (see .github/workflows/macos.yml), so
/// the dongle works without Homebrew. A user-set plugin path wins.
#[cfg(target_os = "macos")]
fn use_bundled_soapy_modules() {
    if std::env::var_os("SOAPY_SDR_PLUGIN_PATH").is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if let Some(dir) = exe
        .parent()
        .map(|d| d.join("../Resources/SoapySDR/modules0.8"))
        .filter(|d| d.is_dir())
    {
        std::env::set_var("SOAPY_SDR_PLUGIN_PATH", dir);
    }
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "macos")]
    use_bundled_soapy_modules();
    let builder = tauri::Builder::default();
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_opener::init());
    builder
        .setup(|_app| {
            #[cfg(target_os = "macos")]
            icon::set_dock_icon()?;
            Ok(())
        })
        .manage(Receiver::default())
        .manage(PreferencesStore::default())
        .manage(native_audio::NativeMonitor::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            platform,
            start_rtl_driver,
            set_native_monitor,
            devices,
            load_preferences,
            save_preferences,
            presets,
            preset_catalog,
            start,
            stop,
            default_recording_directory,
            start_recording,
            stop_recording,
            snapshot
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                if let Some(mut session) = window.state::<Receiver>().0.lock().unwrap().take() {
                    session.stop();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Unable to run receiver");
}

use std::path::{Path, PathBuf};
use tauri::Manager;

pub fn default_directory(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    // Android: prefer the shared Download folder, which file manager and music
    // apps can open and which survives uninstalling the app. Android 11+ lets
    // apps create files there without permissions; otherwise (or if it fails)
    // fall back to the app-specific Music folder below.
    #[cfg(target_os = "android")]
    {
        let root = std::env::var_os("EXTERNAL_STORAGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/storage/emulated/0"));
        let shared = root.join("Download").join("JR-CLRR").join("Recordings");
        if std::fs::create_dir_all(&shared).is_ok() && writable(&shared) {
            return Ok(shared);
        }
    }
    app_music_directory(app)
}

/// The original default: the OS Music folder (on Android the app-specific
/// Android/data/<id>/files/Music, which is removed on uninstall).
pub fn app_music_directory(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let music = app
        .path()
        .audio_dir()
        .or_else(|_| app.path().home_dir().map(|home| home.join("Music")))
        .map_err(|e| {
            format!("初期保存先を取得できませんでした。保存先フォルダーを選択してください。{e}")
        })?;
    Ok(music
        .join("JR Cconventional-line Radio Receiver")
        .join("Recordings"))
}

#[cfg(target_os = "android")]
fn writable(directory: &Path) -> bool {
    let probe = directory.join(".jr-clrr-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

// Only the application's default is created automatically. Custom paths retain
// the recorder's existing-directory validation (including mistyped paths).
pub fn prepare(directory: &Path, default: Option<&Path>) -> Result<(), String> {
    if Some(directory) == default {
        std::fs::create_dir_all(directory).map_err(|e| {
            format!(
                "初期保存先「{}」を作成できませんでした。別の保存先フォルダーを選択してください。{e}",
                directory.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use receiver_core::{pipeline::ChannelConfig, recording::Recorder};

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "jr-recording-default-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn first_recording_creates_default_and_reuses_it_without_overwriting() {
        let temp = Temp::new();
        let directory = temp.0.join("Music/受信アプリ/Recordings");
        prepare(&directory, Some(&directory)).unwrap();
        let channels = [ChannelConfig {
            id: "test".into(),
            label: "テスト".into(),
            frequency_hz: 352_537_500.0,
            format: receiver_core::data::Format::Common,
        }];
        let recorder = Recorder::default();
        let first = recorder.start(&directory, &channels).unwrap();
        assert_eq!(recorder.stop().state, "completed");
        let metadata = PathBuf::from(first.directory.unwrap()).join("session.json");
        let saved = std::fs::read(&metadata).unwrap();
        prepare(&directory, Some(&directory)).unwrap();
        let second = recorder.start(&directory, &channels).unwrap();
        assert_eq!(recorder.stop().state, "completed");
        assert_ne!(
            metadata.parent().unwrap(),
            Path::new(&second.directory.unwrap())
        );
        assert_eq!(std::fs::read(metadata).unwrap(), saved);
    }

    #[test]
    fn failed_default_does_not_overwrite_files_and_custom_paths_are_not_created() {
        let temp = Temp::new();
        let blocked = temp.0.join("Music");
        std::fs::write(&blocked, b"existing file").unwrap();
        let directory = blocked.join("Recordings");
        assert!(prepare(&directory, Some(&directory))
            .unwrap_err()
            .contains("別の保存先"));
        assert_eq!(std::fs::read(blocked).unwrap(), b"existing file");
        let custom = temp.0.join("custom");
        prepare(&custom, Some(&directory)).unwrap();
        assert!(!custom.exists());
        prepare(&custom, None).unwrap();
        assert!(!custom.exists());
        std::fs::create_dir(&custom).unwrap();
        prepare(&custom, None).unwrap();
        assert!(custom.is_dir());
    }
}

//! Versioned user preferences; reception always requires an explicit start.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

const MAX_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Preferences {
    pub version: u32,
    pub preset_file: Option<String>,
    pub input: InputSettings,
    pub output: OutputSettings,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            preset_file: None,
            input: InputSettings::default(),
            output: OutputSettings::default(),
        }
    }
}
// Strings preserve empty fields and unfinished input. Reception still performs
// its normal range/bandwidth validation before opening a device or socket.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct InputSettings {
    pub kind: String,
    pub profile: String,
    pub center: String,
    pub rate: String,
    pub gain: String,
    pub host: String,
    pub port: String,
    pub ppm: String,
    pub args: String,
    pub rx_channel: String,
    pub antenna: String,
    pub path: String,
}
impl Default for InputSettings {
    fn default() -> Self {
        Self {
            kind: "rtl_tcp".into(),
            profile: "east".into(),
            center: "352.603125".into(),
            rate: "240000".into(),
            gain: String::new(),
            host: String::new(),
            port: String::new(),
            ppm: "0".into(),
            args: String::new(),
            rx_channel: "0".into(),
            antenna: String::new(),
            path: String::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputSettings {
    pub enabled: bool,
    pub directory: Option<String>,
    pub audio: bool,
    pub raw_frames: bool,
    pub data_messages: bool,
}
impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: None,
            audio: true,
            raw_frames: false,
            data_messages: false,
        }
    }
}
impl Preferences {
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("このバージョンの設定ファイルには対応していません。対応するアプリで開いてください。".into());
        }
        let valid_profile =
            if self.preset_file.is_some() {
                !self.input.profile.is_empty()
                    && self.input.profile.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                    })
            } else {
                receiver_core::presets::resolve(&self.input.profile).is_ok()
            };
        if !["rtl_tcp", "sdr", "wave", "usb"].contains(&self.input.kind.as_str()) || !valid_profile
        {
            return Err("設定ファイルの入力方式または受信プリセットが不正です。".into());
        }
        let i = &self.input;
        for s in [
            &i.profile,
            &i.center,
            &i.rate,
            &i.gain,
            &i.host,
            &i.port,
            &i.ppm,
            &i.args,
            &i.rx_channel,
            &i.antenna,
            &i.path,
        ] {
            if s.len() > 8192 || s.contains('\0') {
                return Err("設定の入力値が長すぎるか、不正な文字を含んでいます。".into());
            }
        }
        // Do not read the catalog here: missing or temporarily invalid files
        // must not prevent restoring/editing the other connection preferences.
        if self
            .preset_file
            .as_ref()
            .is_some_and(|s| s.len() > 8192 || s.contains('\0') || !Path::new(s).is_absolute())
        {
            return Err("プリセットファイルには有効な絶対パスを指定してください。".into());
        }
        if self
            .output
            .directory
            .as_ref()
            .is_some_and(|s| s.len() > 8192 || s.contains('\0'))
        {
            return Err("保存先のパスが長すぎるか、不正な文字を含んでいます。".into());
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct Store(Mutex<()>);
impl Store {
    pub fn load(&self, path: &Path) -> Result<Option<Preferences>, String> {
        let _guard = self.0.lock().map_err(|e| e.to_string())?;
        read(path)
    }
    pub fn save(&self, path: &Path, settings: &Preferences) -> Result<(), String> {
        let _guard = self.0.lock().map_err(|e| e.to_string())?;
        settings.validate()?;
        // Never replace a corrupt file or one written by a newer application.
        // The load error identifies the file so it can be repaired or moved.
        read(path)?;
        let parent = path
            .parent()
            .ok_or("設定フォルダーを取得できませんでした。")?;
        fs::create_dir_all(parent).map_err(|e| format!("設定フォルダーを作成できません：{e}"))?;
        let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
        if bytes.len() as u64 + 1 > MAX_BYTES {
            return Err("設定のサイズが大きすぎます。入力した値を短くしてください。".into());
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let (temporary, mut file) = loop {
            let temporary = parent.join(format!(
                ".settings-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match options.open(&temporary) {
                Ok(file) => break (temporary, file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(format!(
                    "設定を書き込めません。設定フォルダーの権限と空き容量を確認してください：{e}"
                ))
                }
            }
        };
        let result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "設定を保存できません。設定フォルダーの権限と空き容量を確認してください：{e}"
            ));
        }
        Ok(())
    }
}
fn read(path: &Path) -> Result<Option<Preferences>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "設定ファイル「{}」を読み込めません：{e}",
                path.display()
            ))
        }
    };
    let checked = || -> Result<Preferences, String> {
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("設定ファイルが大きすぎます。".into());
        }
        let settings: Preferences =
            serde_json::from_slice(&bytes).map_err(|_| "設定ファイルの JSON 形式が不正です。")?;
        settings.validate()?;
        Ok(settings)
    };
    checked().map(Some).map_err(|e| {
        format!(
            "{e} 設定ファイル「{}」を修正するか退避してから、再読み込みしてください。",
            path.display()
        )
    })
}
pub fn path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path()
        .app_config_dir()
        .map(|p| p.join("settings.json"))
        .map_err(|e| format!("設定フォルダーを取得できません：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "jr-preferences-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn first_run_is_empty_and_saved_settings_survive_new_store_instances() {
        let temp = Temp::new();
        let path = temp.0.join("config/settings.json");
        assert!(Store::default().load(&path).unwrap().is_none());
        assert!(!path.exists());
        let mut p = Preferences::default();
        p.input.host = "::1".into();
        p.input.port = "1234".into();
        p.input.kind = "sdr".into();
        p.input.args = "driver=rtlsdr,serial=test".into();
        p.input.rate = "1024000".into();
        p.input.gain = "19.7".into();
        p.input.antenna = "RX".into();
        p.input.rx_channel = "1".into();
        p.output.directory = Some("/tmp/受信データ".into());
        p.output.enabled = true;
        p.output.audio = false;
        p.output.raw_frames = true;
        Store::default().save(&path, &p).unwrap();
        assert_eq!(Store::default().load(&path).unwrap(), Some(p.clone()));
        p.input.port.clear();
        p.output.enabled = false;
        p.output.directory = Some(String::new());
        Store::default().save(&path, &p).unwrap();
        assert_eq!(Store::default().load(&path).unwrap(), Some(p));
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn corrupt_and_future_files_are_reported_and_never_overwritten() {
        let temp = Temp::new();
        let path = temp.0.join("settings.json");
        for bytes in [
            b"broken".to_vec(),
            br#"{"version":2}"#.to_vec(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            fs::write(&path, &bytes).unwrap();
            let store = Store::default();
            assert!(store.load(&path).is_err());
            assert!(store.save(&path, &Preferences::default()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn partial_settings_keep_empty_endpoint_defaults_and_invalid_updates_keep_old_file() {
        let temp = Temp::new();
        let path = temp.0.join("settings.json");
        fs::write(&path, br#"{"version":1,"output":{"raw_frames":true}}"#).unwrap();
        let store = Store::default();
        let mut p = store.load(&path).unwrap().unwrap();
        assert!(p.input.host.is_empty() && p.input.port.is_empty());
        assert!(p.output.audio && p.output.raw_frames);
        assert!(!p.output.enabled);
        assert!(p.output.directory.is_none());
        assert!(p.preset_file.is_none());
        let old = fs::read(&path).unwrap();
        p.input.kind = "unknown".into();
        assert!(store.save(&path, &p).is_err());
        assert_eq!(fs::read(&path).unwrap(), old);
        let blocked = temp.0.join("file");
        fs::write(&blocked, b"preserve").unwrap();
        assert!(store
            .save(&blocked.join("settings.json"), &Preferences::default())
            .is_err());
        assert_eq!(fs::read(blocked).unwrap(), b"preserve");
    }
    #[test]
    fn external_profile_and_missing_catalog_path_survive_restart() {
        let temp = Temp::new();
        let path = temp.0.join("settings.json");
        let mut p = Preferences {
            preset_file: Some(temp.0.join("未作成.json").to_string_lossy().into()),
            ..Preferences::default()
        };
        p.input.profile = "custom-line-1".into();
        Store::default().save(&path, &p).unwrap();
        assert_eq!(Store::default().load(&path).unwrap(), Some(p.clone()));
        let original = fs::read(&path).unwrap();
        for invalid_path in [None, Some("".into()), Some("relative.json".into())] {
            p.preset_file = invalid_path;
            assert!(Store::default().save(&path, &p).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        p.input.profile = "east".into();
        p.preset_file = None;
        Store::default().save(&path, &p).unwrap();
        assert_eq!(Store::default().load(&path).unwrap(), Some(p));
    }
}

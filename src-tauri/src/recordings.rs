//! Browse and play saved recordings inside the app. On Android the
//! app-specific folder (Android/data/...) cannot be opened from the phone's
//! file manager, so the app lists the recording folders itself.
use serde::Serialize;
use std::path::{Path, PathBuf};

const MAX_SESSIONS: usize = 100;
const MAX_WAV_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq)]
pub struct RecordingFile {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    /// Last write (end of the call), Unix seconds; 0 when unknown.
    pub modified: u64,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct RecordingSession {
    pub name: String,
    pub directory: String,
    pub root: String,
    /// Still recording (or ended abnormally): WAV headers may be unfinished.
    pub incomplete: bool,
    pub files: Vec<RecordingFile>,
}

/// Existing folders that may hold `recording-*` sessions, without duplicates.
pub fn roots(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = Vec::new();
    for path in candidates {
        if let Ok(real) = path.canonicalize() {
            if real.is_dir() && !result.contains(&real) {
                result.push(real);
            }
        }
    }
    result
}

fn is_session(path: &Path) -> bool {
    path.is_dir()
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("recording-"))
}

/// Newest sessions first (folder names start with a sortable UTC timestamp).
pub fn list(roots: &[PathBuf]) -> Vec<RecordingSession> {
    let mut sessions = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            if !is_session(&directory) {
                continue;
            }
            let mut files: Vec<RecordingFile> = std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|f| {
                    let path = f.path();
                    let name = path.file_name()?.to_str()?.to_string();
                    let wav = path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
                    let meta = f.metadata().ok()?;
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                    (wav && meta.is_file()).then(|| RecordingFile {
                        name,
                        path: path.to_string_lossy().into_owned(),
                        bytes: meta.len(),
                        modified,
                    })
                })
                .collect();
            if files.is_empty() {
                // No call yet (or data-only JSONL): nothing to listen to.
                continue;
            }
            files.sort_by(|a, b| a.name.cmp(&b.name));
            sessions.push(RecordingSession {
                name: entry.file_name().to_string_lossy().into_owned(),
                incomplete: directory.join("INCOMPLETE").exists(),
                directory: directory.to_string_lossy().into_owned(),
                root: root.to_string_lossy().into_owned(),
                files,
            });
        }
    }
    sessions.sort_by(|a, b| b.name.cmp(&a.name));
    sessions.truncate(MAX_SESSIONS);
    sessions
}

/// Reads a WAV only from a `recording-*` folder directly inside a known root.
pub fn read(roots: &[PathBuf], path: &str) -> Result<Vec<u8>, String> {
    let file = Path::new(path)
        .canonicalize()
        .map_err(|e| format!("録音ファイルが見つかりません。{e}"))?;
    let session = file.parent().filter(|p| is_session(p));
    let allowed = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
        && session
            .and_then(Path::parent)
            .is_some_and(|root| roots.iter().any(|r| r == root));
    if !allowed {
        return Err("この場所のファイルは再生できません。".into());
    }
    let meta = std::fs::metadata(&file).map_err(|e| e.to_string())?;
    if meta.len() > MAX_WAV_BYTES {
        return Err("録音ファイルが大きすぎます。".into());
    }
    std::fs::read(&file).map_err(|e| format!("録音ファイルを読み込めませんでした。{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("jr-recordings-{name}-{nonce}"));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lists_sessions_newest_first_and_reads_only_their_wavs() {
        let temp = Temp::new("list");
        let root = temp.0.join("Recordings");
        let old = root.join("recording-20261017T000000.000Z-001");
        let new = root.join("recording-20261018T120000.000Z-001");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("352606250Hz-000001.wav"), b"RIFFold").unwrap();
        std::fs::write(new.join("352543750Hz-000002.wav"), b"RIFF2").unwrap();
        std::fs::write(new.join("352543750Hz-000001.wav"), b"RIFF1").unwrap();
        std::fs::write(new.join("session.json"), b"{}").unwrap();
        std::fs::write(new.join("INCOMPLETE"), b"").unwrap();
        std::fs::create_dir_all(root.join("unrelated")).unwrap();
        // A session without any call is not listed.
        let silent = root.join("recording-20261019T000000.000Z-001");
        std::fs::create_dir_all(&silent).unwrap();
        std::fs::write(silent.join("session.json"), b"{}").unwrap();
        let roots = roots([root.clone(), root.clone(), temp.0.join("missing")]);
        assert_eq!(roots.len(), 1);
        let sessions = list(&roots);
        assert_eq!(
            sessions.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            [
                "recording-20261018T120000.000Z-001",
                "recording-20261017T000000.000Z-001"
            ]
        );
        assert!(sessions[0].incomplete && !sessions[1].incomplete);
        assert_eq!(
            sessions[0]
                .files
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["352543750Hz-000001.wav", "352543750Hz-000002.wav"]
        );
        assert_eq!(read(&roots, &sessions[0].files[0].path).unwrap(), b"RIFF1");
        // Nothing outside a recording session folder of a known root.
        assert!(read(&roots, &new.join("session.json").to_string_lossy()).is_err());
        let outside = temp.0.join("recording-x");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("a.wav"), b"RIFF").unwrap();
        assert!(read(&roots, &outside.join("a.wav").to_string_lossy()).is_err());
        assert!(read(
            &roots,
            &new.join("../../recording-x/a.wav").to_string_lossy()
        )
        .is_err());
    }
}

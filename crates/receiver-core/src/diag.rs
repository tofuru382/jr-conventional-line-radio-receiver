//! Notes about the input (start, USB dongle, errors) kept in memory and shown
//! in the app, so a failure on a phone can be reported without a debugger.
//!
//! With [`persist_to`], they also go to a file, so the notes of a run that the
//! phone killed (in the background) can still be read after the next start.
use std::{
    collections::VecDeque,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const KEEP: usize = 300;
static NOTES: Mutex<VecDeque<(u64, String)>> = Mutex::new(VecDeque::new());
static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
/// A log file is cut to its last half beyond this size.
const FILE_LIMIT: u64 = 256 * 1024;

fn previous_path(path: &Path) -> PathBuf {
    path.with_extension("prev.log")
}

/// Writes every later note to `path` as "<unix ms>\t<text>" lines. The file
/// of the previous run is kept as `<name>.prev.log` (see [`previous`]).
pub fn persist_to(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if path.exists() {
        let _ = std::fs::rename(&path, previous_path(&path));
    }
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

/// The notes of the previous run (before this process started), oldest first.
pub fn previous() -> Vec<(u64, String)> {
    let Some(path) = FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return Vec::new();
    };
    parse(&std::fs::read_to_string(previous_path(&path)).unwrap_or_default())
}

fn parse(text: &str) -> Vec<(u64, String)> {
    text.lines()
        .filter_map(|line| {
            let (ms, note) = line.split_once('\t')?;
            Some((ms.parse().ok()?, note.to_string()))
        })
        .collect()
}

fn append(path: &Path, ms: u64, text: &str) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > FILE_LIMIT) {
        if let Ok(old) = std::fs::read_to_string(path) {
            let half = old.len() / 2;
            let cut = old[half..].find('\n').map_or(old.len(), |i| half + i + 1);
            let _ = std::fs::write(path, &old[cut..]);
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{ms}\t{}", text.replace('\n', " "));
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Adds a note (the oldest is dropped beyond 300).
pub fn note(text: impl Into<String>) {
    let (ms, text) = (now_ms(), text.into());
    if let Some(path) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_deref() {
        append(path, ms, &text);
    }
    let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    if notes.len() == KEEP {
        notes.pop_front();
    }
    notes.push_back((ms, text));
}

/// All notes, oldest first, as (unix milliseconds, text).
pub fn notes() -> Vec<(u64, String)> {
    NOTES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_latest_notes_in_order() {
        for i in 0..KEEP + 5 {
            note(format!("diag-test {i}"));
        }
        let ours: Vec<_> = notes()
            .into_iter()
            .filter(|(_, t)| t.starts_with("diag-test "))
            .collect();
        assert!(ours.len() <= KEEP);
        assert_eq!(ours.last().unwrap().1, format!("diag-test {}", KEEP + 4));
        assert!(ours.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    #[test]
    fn a_log_file_survives_into_the_next_run_and_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("jr-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connection.log");
        append(&path, 1, "first\nline");
        append(&path, 2, "second");
        // As persist_to does at the next start.
        std::fs::rename(&path, previous_path(&path)).unwrap();
        let text = std::fs::read_to_string(previous_path(&path)).unwrap();
        assert_eq!(
            parse(&text),
            vec![(1, "first line".to_string()), (2, "second".to_string())]
        );
        let long = "x".repeat(1000);
        for i in 0..600 {
            append(&path, i, &long);
        }
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size <= FILE_LIMIT + 2 * 1010, "{size}");
        let kept = parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(kept.last().unwrap().0, 599);
        assert!(kept.len() < 600);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

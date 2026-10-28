//! Notes about the input (start, USB dongle, errors) kept in memory and shown
//! in the app, so a failure on a phone can be reported without a debugger.
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const KEEP: usize = 300;
static NOTES: Mutex<VecDeque<(u64, String)>> = Mutex::new(VecDeque::new());

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Adds a note (the oldest is dropped beyond 300).
pub fn note(text: impl Into<String>) {
    let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    if notes.len() == KEEP {
        notes.pop_front();
    }
    notes.push_back((now_ms(), text.into()));
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
}

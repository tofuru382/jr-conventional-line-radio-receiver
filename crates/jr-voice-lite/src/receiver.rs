//! Safe owned interface; preserves the C ABI acceptance and retirement policy.
use super::Decoder;
pub use crate::stream::Output;

#[derive(Default)]
pub struct Voice(Decoder);
impl Voice {
    pub fn push(&mut self, index: u32, payload: Option<[u8; 20]>) -> Result<(), String> {
        if self.0.ready(index) != 0 {
            return Err("Voice decoder is not ready; drain output or restart".into());
        }
        if payload.is_some_and(|p| p[19] & 0xc0 != 0) {
            return Err("Invalid CELP payload".into());
        }
        let code = self.0.guard(
            |d| {
                d.last_index = Some(index);
                match payload {
                    Some(p) => d.voice.push(index, p),
                    None => d.voice.advance(),
                }
            },
            false,
        );
        if code == 0 {
            Ok(())
        } else {
            Err("CELP synthesis failed; restart reception".into())
        }
    }
    pub fn read(&mut self) -> Option<Output> {
        self.0.voice.read()
    }
    pub fn flush(&mut self) -> Result<(), String> {
        if self.0.status.failed != 0 {
            return Err("Voice decoder failed".into());
        }
        if self.0.status.finished != 0 {
            return Ok(());
        }
        let code = self.0.guard(
            |d| {
                d.voice.flush()?;
                d.status.finished = 1;
                Ok(())
            },
            true,
        );
        if code == 0 {
            Ok(())
        } else {
            Err("CELP flush failed".into())
        }
    }
}

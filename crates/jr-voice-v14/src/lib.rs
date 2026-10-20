//! Rust CELP decoder, C ABI 2. Radio/channel decoding belongs to the receiver host.
// Explicit indexed loops make the binary32 reduction order auditable.
#![allow(clippy::needless_range_loop)]
mod chime;
mod correction;
mod models;
mod numeric;
mod plan_acoustics;
mod plan_features;
mod plan_target;
mod planner;
mod predictor;
pub mod receiver;
mod stream;
#[cfg(test)]
mod tests;
mod vocoder;

use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use stream::VoiceStream;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct VoiceStatus {
    pub finished: u32,
    pub failed: u32,
    pub synthesized: u32,
    pub output: u32,
    pub last_decode_us: u32,
    pub maximum_frame_us: u32,
    pub maximum_flush_us: u32,
}
#[derive(Default)]
struct Decoder {
    voice: VoiceStream,
    last_index: Option<u32>,
    status: VoiceStatus,
}
impl Decoder {
    fn guard(&mut self, action: impl FnOnce(&mut Self) -> Result<(), ()>, flush: bool) -> i32 {
        let start = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| action(self)));
        self.status.synthesized = self.voice.synthesized;
        self.status.output = self.voice.emitted;
        self.status.last_decode_us = start.elapsed().as_micros().min(u32::MAX as u128) as u32;
        let maximum = if flush {
            &mut self.status.maximum_flush_us
        } else {
            &mut self.status.maximum_frame_us
        };
        *maximum = (*maximum).max(self.status.last_decode_us);
        if matches!(result, Ok(Ok(()))) {
            0
        } else {
            self.status.failed = 1;
            -4
        }
    }
    fn ready(&self, index: u32) -> i32 {
        if self.status.failed != 0
            || self.status.finished != 0
            || self.last_index.is_some_and(|last| index <= last)
        {
            -2
        } else if self.voice.has_output() {
            -3
        } else {
            0
        }
    }
}

// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub extern "C" fn jr_voice_abi_version() -> i32 {
    2
}
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub extern "C" fn jr_voice_status_size() -> i32 {
    std::mem::size_of::<VoiceStatus>() as i32
}
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub extern "C" fn jr_voice_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// # Safety
/// `handle` must point to writable pointer storage. Caller owns the result.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_create(handle: *mut *mut c_void) -> i32 {
    if handle.is_null() {
        return -1;
    }
    *handle = std::ptr::null_mut();
    match catch_unwind(|| Box::new(Decoder::default())) {
        Ok(decoder) => {
            *handle = Box::into_raw(decoder).cast();
            0
        }
        Err(_) => -4,
    }
}
/// # Safety
/// `handle` is null or a live handle from create; destroy exactly once.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(Box::from_raw(handle.cast::<Decoder>()));
    }
}
/// # Safety
/// All handle operations require a live handle and exclusive serialized access.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_reset(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let d = &mut *handle.cast::<Decoder>();
    match catch_unwind(AssertUnwindSafe(|| *d = Decoder::default())) {
        Ok(()) => 0,
        Err(_) => {
            d.status.failed = 1;
            -4
        }
    }
}
/// # Safety
/// `payload` points to 20 readable bytes, `handle` is exclusively borrowed.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_process(
    handle: *mut c_void,
    index: u32,
    payload: *const u8,
    length: i32,
) -> i32 {
    if handle.is_null() || payload.is_null() || length != 20 {
        return -1;
    }
    let input: [u8; 20] = std::slice::from_raw_parts(payload, 20).try_into().unwrap();
    if input[19] & 0xc0 != 0 {
        return -1;
    }
    let d = &mut *handle.cast::<Decoder>();
    let result = d.ready(index);
    if result != 0 {
        return result;
    }
    d.guard(
        |d| {
            d.last_index = Some(index);
            d.voice.push(index, input)
        },
        false,
    )
}
/// Advance boundary work for a rejected/nonvoice radio slot, without a payload.
/// # Safety
/// `handle` is live and exclusively borrowed.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_advance(handle: *mut c_void, index: u32) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let d = &mut *handle.cast::<Decoder>();
    let result = d.ready(index);
    if result != 0 {
        return result;
    }
    d.guard(
        |d| {
            d.last_index = Some(index);
            d.voice.missing_frame()
        },
        false,
    )
}
/// # Safety
/// `handle` is live and exclusively borrowed.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_flush(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let d = &mut *handle.cast::<Decoder>();
    if d.status.failed != 0 {
        return -2;
    }
    if d.status.finished != 0 {
        return 0;
    }
    if d.voice.has_output() {
        return -3;
    }
    d.guard(
        |d| {
            d.status.finished = 1;
            d.voice.flush()
        },
        true,
    )
}
/// # Safety
/// `index` and `pcm` point to disjoint writable storage (4 and 640 bytes).
/// `handle` is live and exclusively borrowed; no caller buffers are retained.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_read(
    handle: *mut c_void,
    index: *mut u32,
    pcm: *mut u8,
    length: i32,
) -> i32 {
    if handle.is_null() || index.is_null() || pcm.is_null() || length != 640 {
        return -1;
    }
    let d = &mut *handle.cast::<Decoder>();
    if d.status.failed != 0 {
        return -2;
    }
    let Some(frame) = d.voice.read() else {
        return 0;
    };
    *index = frame.index;
    let output = std::slice::from_raw_parts_mut(pcm, 640);
    for (i, value) in frame.pcm.iter().enumerate() {
        output[2 * i..2 * i + 2].copy_from_slice(&value.to_le_bytes());
    }
    1
}
/// # Safety
/// `status` points to writable VoiceStatus storage; handle is live/exclusive.
// (v1.4 copy: C symbol export removed so it can link beside jr-voice)
pub unsafe extern "C" fn jr_voice_status(handle: *mut c_void, status: *mut VoiceStatus) -> i32 {
    if handle.is_null() || status.is_null() {
        return -1;
    }
    *status = (*handle.cast::<Decoder>()).status;
    0
}

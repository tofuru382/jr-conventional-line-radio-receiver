//! Versioned C ABI for batch IQ processing. Handles are exclusively owned by
//! one serialized caller. Buffers must be valid, aligned and non-overlapping;
//! the library never retains pointers into caller memory.
#![deny(unsafe_op_in_unsafe_fn)]

mod dsp;
mod equalizer;
mod sequence;
mod signal;
use dsp::Demodulator;
pub use dsp::{Complex32, Snapshot};
pub mod receiver;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

const OK: i32 = 0;
const INVALID_RATE: i32 = -1;
const INVALID_OFFSET: i32 = -2;
const INVALID_PROFILE: i32 = -3;
const INVALID_BUFFER: i32 = -4;
const INVALID_HANDLE: i32 = -5;
const NONFINITE_IQ: i32 = -6;
const INTERNAL_ERROR: i32 = -7;

#[no_mangle]
pub extern "C" fn jr_dsp_abi_version() -> i32 {
    2
}

#[no_mangle]
pub extern "C" fn jr_dsp_snapshot_size() -> i32 {
    std::mem::size_of::<Snapshot>() as i32
}

/// # Safety
/// `output` points to writable pointer storage. The caller must destroy each
/// successful handle exactly once after all calls using it have completed.
#[no_mangle]
pub unsafe extern "C" fn jr_dsp_create(
    rate: f64,
    offset: f64,
    profile: i32,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return INVALID_BUFFER;
    }
    unsafe {
        *output = std::ptr::null_mut();
    }
    if !rate.is_finite() || !(9600.0..=20000000.0).contains(&rate) {
        return INVALID_RATE;
    }
    if !offset.is_finite() || offset.abs() + 4200.0 >= rate / 2.0 {
        return INVALID_OFFSET;
    }
    if !(0..=1).contains(&profile) {
        return INVALID_PROFILE;
    }
    match catch_unwind(|| Box::new(Demodulator::new(rate, offset, profile))) {
        Ok(state) => {
            unsafe {
                *output = Box::into_raw(state).cast();
            }
            OK
        }
        Err(_) => INTERNAL_ERROR,
    }
}

/// # Safety
/// A non-null handle must have been returned by create and not yet destroyed.
/// No other call may access the handle concurrently with destruction.
#[no_mangle]
pub unsafe extern "C" fn jr_dsp_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle.cast::<Demodulator>()));
        }
    }
}

/// Emit one dibit (0..3) per output byte. No partial packed byte is hidden.
/// Capacity must be at least `2 * input_len + 4` for nonempty input. This
/// covers the maximum work-rate expansion at 9.6 kHz, including phase edges.
/// Invalid arguments or nonfinite IQ are rejected before advancing state.
/// An unexpected panic retires the handle; destroy it and create a new one.
///
/// # Safety
/// The handle is live and exclusively accessed. Positive-length buffers are
/// valid, aligned and disjoint, and written/snapshot point to writable storage.
/// Null data pointers are allowed only with zero corresponding length.
#[no_mangle]
pub unsafe extern "C" fn jr_dsp_process(
    handle: *mut c_void,
    input: *const Complex32,
    input_len: i32,
    output: *mut u8,
    capacity: i32,
    written: *mut i32,
    snapshot: *mut Snapshot,
) -> i32 {
    if written.is_null() || snapshot.is_null() {
        return INVALID_BUFFER;
    }
    unsafe {
        *written = 0;
    }
    if handle.is_null() {
        return INVALID_HANDLE;
    }
    if input_len < 0
        || capacity < 0
        || (input_len > 0 && input.is_null())
        || (capacity > 0 && output.is_null())
    {
        return INVALID_BUFFER;
    }
    let needed = if input_len == 0 {
        0
    } else {
        input_len as i64 * 2 + 4
    };
    if (capacity as i64) < needed || input_len as u64 * 8 > isize::MAX as u64 {
        return INVALID_BUFFER;
    }
    // A null pointer is not valid for from_raw_parts, even with a zero length.
    let samples = if input_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, input_len as usize) }
    };
    if samples
        .iter()
        .any(|sample| !sample.real.is_finite() || !sample.imaginary.is_finite())
    {
        return NONFINITE_IQ;
    }
    let destination = if capacity == 0 {
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(output, capacity as usize) }
    };
    let state = unsafe { &mut *handle.cast::<Demodulator>() };
    if state.failed {
        return INTERNAL_ERROR;
    }
    match catch_unwind(AssertUnwindSafe(|| state.process(samples, destination))) {
        Ok(count) => {
            unsafe {
                *written = count as i32;
                *snapshot = state.snapshot();
            }
            OK
        }
        Err(_) => {
            state.failed = true;
            INTERNAL_ERROR
        }
    }
}

/// Read normalized differential symbols without consuming them or advancing DSP.
/// The source is the unsliced observation after receive filtering, timing and
/// carrier recovery, and ISI/complex-gain removal using the current best sequence
/// survivor's channel estimate. It is not the hard traceback output. The known
/// pi/4 increment is restored for the four diagonal constellation references.
/// Capacity is 0..1024; zero input power returns an empty view and zero EVM.
///
/// # Safety
/// The live handle is exclusively accessed. Output (when capacity > 0), written
/// and evm point to valid aligned, writable, non-overlapping storage.
#[no_mangle]
pub unsafe extern "C" fn jr_dsp_read_signal(
    handle: *mut c_void,
    output: *mut Complex32,
    capacity: i32,
    written: *mut i32,
    evm: *mut f64,
) -> i32 {
    if written.is_null() || evm.is_null() {
        return INVALID_BUFFER;
    }
    unsafe {
        *written = 0;
        *evm = 0.0;
    }
    if handle.is_null() {
        return INVALID_HANDLE;
    }
    if !(0..=signal::CAPACITY as i32).contains(&capacity) || (capacity > 0 && output.is_null()) {
        return INVALID_BUFFER;
    }
    let state = unsafe { &mut *handle.cast::<Demodulator>() };
    if state.failed {
        return INTERNAL_ERROR;
    }
    let destination = if capacity == 0 {
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(output, capacity as usize) }
    };
    match catch_unwind(AssertUnwindSafe(|| state.read_signal(destination))) {
        Ok((count, error)) => {
            unsafe {
                *written = count as i32;
                *evm = error;
            }
            OK
        }
        Err(_) => {
            state.failed = true;
            INTERNAL_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Handle(*mut c_void);
    impl Handle {
        fn new(rate: f64, offset: f64, profile: i32) -> Self {
            let mut raw = std::ptr::null_mut();
            assert_eq!(
                unsafe { jr_dsp_create(rate, offset, profile, &mut raw) },
                OK
            );
            Self(raw)
        }
        fn process(&self, input: &[Complex32]) -> (Vec<u8>, Snapshot) {
            let mut output = vec![0xFF; input.len() * 2 + 4];
            let mut count = -1;
            let mut snapshot = Snapshot::default();
            assert_eq!(
                unsafe {
                    jr_dsp_process(
                        self.0,
                        input.as_ptr(),
                        input.len() as i32,
                        output.as_mut_ptr(),
                        output.len() as i32,
                        &mut count,
                        &mut snapshot,
                    )
                },
                OK
            );
            output.truncate(count as usize);
            assert!(output.iter().all(|x| *x <= 3));
            (output, snapshot)
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                jr_dsp_destroy(self.0);
            }
        }
    }

    #[test]
    fn abi_layout_and_empty_null_buffers() {
        assert_eq!(jr_dsp_abi_version(), 2);
        assert_eq!(jr_dsp_snapshot_size(), 40);
        assert_eq!(std::mem::size_of::<Complex32>(), 8);
        assert_eq!(std::mem::offset_of!(Snapshot, frequency_offset_hz), 24);
        assert_eq!(std::mem::offset_of!(Snapshot, carrier_locked), 32);
        let handle = Handle::new(48000.0, 0.0, 0);
        let mut written = -1;
        let mut snapshot = Snapshot::default();
        assert_eq!(
            unsafe {
                jr_dsp_process(
                    handle.0,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut written,
                    &mut snapshot,
                )
            },
            OK
        );
        assert_eq!(written, 0);
        assert_eq!(snapshot, Snapshot::default());
        unsafe {
            jr_dsp_destroy(std::ptr::null_mut());
        }
    }

    #[test]
    fn signal_read_checks_buffers_and_leaves_dsp_unchanged() {
        let handle = Handle::new(48000.0, 0.0, 0);
        let mut written = -1;
        let mut evm = -1.0;
        assert_eq!(
            unsafe {
                jr_dsp_read_signal(handle.0, std::ptr::null_mut(), 0, &mut written, &mut evm)
            },
            OK
        );
        assert_eq!((written, evm), (0, 0.0));
        for capacity in [-1, 1, 1025] {
            assert_eq!(
                unsafe {
                    jr_dsp_read_signal(
                        handle.0,
                        std::ptr::null_mut(),
                        capacity,
                        &mut written,
                        &mut evm,
                    )
                },
                INVALID_BUFFER
            );
        }
        assert_eq!(
            unsafe {
                jr_dsp_read_signal(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    &mut written,
                    &mut evm,
                )
            },
            INVALID_HANDLE
        );
        let input = [Complex32 {
            real: 0.1,
            imaginary: 0.2,
        }; 8192];
        let before = handle.process(&input);
        let mut points = [Complex32::default(); 1024];
        assert_eq!(
            unsafe {
                jr_dsp_read_signal(handle.0, points.as_mut_ptr(), 1024, &mut written, &mut evm)
            },
            OK
        );
        assert!(written > 0 && written <= 1024 && evm.is_finite());
        assert_eq!(handle.process(&[]).1, before.1);
    }

    #[test]
    fn invalid_configuration_never_creates_a_handle() {
        for (rate, offset, profile, expected) in [
            (f64::NAN, 0.0, 2, INVALID_RATE),
            (9599.0, 0.0, 2, INVALID_RATE),
            (20000001.0, 0.0, 2, INVALID_RATE),
            (9600.0, 600.0, 2, INVALID_OFFSET),
            (48000.0, f64::INFINITY, 2, INVALID_OFFSET),
            (48000.0, 0.0, 3, INVALID_PROFILE),
        ] {
            let mut handle = std::ptr::dangling_mut();
            assert_eq!(
                unsafe { jr_dsp_create(rate, offset, profile, &mut handle) },
                expected
            );
            assert!(handle.is_null());
        }
    }

    #[test]
    fn invalid_buffers_and_nonfinite_iq_do_not_consume_state() {
        let handle = Handle::new(9600.0, 0.0, 0);
        let input = vec![Complex32::default(); 2000];
        let mut out = vec![0; 4004];
        let mut written = -1;
        let mut snapshot = Snapshot::default();
        assert_eq!(
            unsafe {
                jr_dsp_process(
                    handle.0,
                    input.as_ptr(),
                    2000,
                    out.as_mut_ptr(),
                    4003,
                    &mut written,
                    &mut snapshot,
                )
            },
            INVALID_BUFFER
        );
        assert_eq!(written, 0);
        assert_eq!(
            unsafe {
                jr_dsp_process(
                    handle.0,
                    std::ptr::null(),
                    1,
                    out.as_mut_ptr(),
                    4004,
                    &mut written,
                    &mut snapshot,
                )
            },
            INVALID_BUFFER
        );
        assert_eq!(
            unsafe {
                jr_dsp_process(
                    handle.0,
                    input.as_ptr(),
                    -1,
                    out.as_mut_ptr(),
                    4004,
                    &mut written,
                    &mut snapshot,
                )
            },
            INVALID_BUFFER
        );
        assert_eq!(
            unsafe {
                jr_dsp_process(
                    std::ptr::null_mut(),
                    input.as_ptr(),
                    2000,
                    out.as_mut_ptr(),
                    4004,
                    &mut written,
                    &mut snapshot,
                )
            },
            INVALID_HANDLE
        );
        let mut bad = input.clone();
        bad[1999].imaginary = f32::NAN;
        assert_eq!(
            unsafe {
                jr_dsp_process(
                    handle.0,
                    bad.as_ptr(),
                    2000,
                    out.as_mut_ptr(),
                    4004,
                    &mut written,
                    &mut snapshot,
                )
            },
            NONFINITE_IQ
        );
        assert_eq!(
            handle.process(&input),
            Handle::new(9600.0, 0.0, 0).process(&input)
        );
    }

    #[test]
    fn chunk_boundaries_and_fresh_sessions_preserve_the_complete_output() {
        for rate in [9600.0, 44100.0, 960000.0, 20000000.0] {
            let input: Vec<_> = (0..16000)
                .map(|i| Complex32 {
                    real: ((i as f64 * 0.071).sin() * 0.2) as f32,
                    imaginary: ((i as f64 * 0.071).cos() * 0.2) as f32,
                })
                .collect();
            let expected = Handle::new(rate, 0.0, 0).process(&input);
            let handle = Handle::new(rate, 0.0, 0);
            let mut actual = Vec::new();
            let mut last = Snapshot::default();
            for chunk in input.chunks(7) {
                let (output, snapshot) = handle.process(chunk);
                actual.extend(output);
                last = snapshot;
            }
            assert_eq!((actual, last), expected);
        }
    }
}

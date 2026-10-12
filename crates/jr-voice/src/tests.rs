use super::*;
use crate::numeric::{round_even, Numeric};
use crate::vocoder::{Vocoder, VoiceNoise};

#[test]
fn abi_validates_payload_before_advancing_and_requires_drain() {
    unsafe {
        assert_eq!(jr_voice_abi_version(), 2);
        assert_eq!(jr_voice_status_size(), 28);
        let mut handle = std::ptr::null_mut();
        assert_eq!(jr_voice_create(&mut handle), 0);
        let mut payload = [0; 20];
        payload[19] = 0x80;
        assert_eq!(jr_voice_process(handle, 0, payload.as_ptr(), 20), -1);
        payload[19] = 0;
        assert_eq!(jr_voice_process(handle, 0, payload.as_ptr(), 48), -1);
        for i in 0..5 {
            assert_eq!(jr_voice_process(handle, i, payload.as_ptr(), 20), 0);
        }
        assert_eq!(jr_voice_advance(handle, 5), -3);
        assert_eq!(jr_voice_flush(handle), -3);
        let mut output = [0xa5; 642];
        let mut index = u32::MAX;
        assert_eq!(
            jr_voice_read(handle, &mut index, output.as_mut_ptr().add(1), 639),
            -1
        );
        assert_eq!(
            jr_voice_read(handle, &mut index, output.as_mut_ptr().add(1), 640),
            1
        );
        assert_eq!(index, 0);
        assert_eq!(output[0], 0xa5);
        assert_eq!(output[641], 0xa5);
        assert_eq!(jr_voice_advance(handle, 4), -2);
        assert_eq!(jr_voice_advance(handle, 5), 0);
        assert_eq!(jr_voice_flush(handle), -3);
        assert_eq!(
            jr_voice_read(handle, &mut index, output.as_mut_ptr().add(1), 640),
            1
        );
        assert_eq!(index, 1);
        assert_eq!(jr_voice_flush(handle), 0);
        assert_eq!(jr_voice_flush(handle), 0);
        assert_eq!(jr_voice_process(handle, 6, payload.as_ptr(), 20), -2);
        assert_eq!(jr_voice_reset(handle), 0);
        assert_eq!(jr_voice_process(handle, u32::MAX, payload.as_ptr(), 20), 0);
        assert_eq!(jr_voice_flush(handle), 0);
        assert_eq!(
            jr_voice_read(handle, &mut index, output.as_mut_ptr().add(1), 640),
            1
        );
        assert_eq!(index, u32::MAX);
        jr_voice_destroy(handle);
        jr_voice_destroy(std::ptr::null_mut());
        assert_eq!(jr_voice_create(std::ptr::null_mut()), -1);
        assert_eq!(jr_voice_advance(std::ptr::null_mut(), 0), -1);
    }
}

#[test]
fn panic_and_synthesis_failure_retire_handle() {
    let mut d = Decoder::default();
    assert_eq!(d.guard(|_| panic!("injected decoder fault"), false), -4);
    assert_eq!(d.ready(0), -2);
    d = Decoder::default();
    assert_eq!(d.guard(|_| Err(()), false), -4);
    assert_eq!(d.ready(0), -2);
}

#[test]
fn numeric_edges_remain_finite_and_normalized() {
    for (input, expected) in [
        (-2.5, -2.0),
        (-1.5, -2.0),
        (-0.5, 0.0),
        (0.5, 0.0),
        (1.5, 2.0),
        (2.5, 2.0),
    ] {
        assert_eq!(round_even(input), expected);
    }
    let mut numeric = Numeric::default();
    for shape in [[0.0; 32], [0.01; 32], [30.0; 32], [-30.0; 32]] {
        let mut impulse = [0.0; 256];
        assert!(numeric.envelope_impulse(&shape, &mut impulse));
        assert!(impulse.iter().all(|x| x.is_finite()));
        let power = impulse.iter().fold(0.0, |sum, x| sum + x * x);
        assert!((power - 1.0).abs() < 1e-5);
    }
    let mut vocoder = Vocoder::default();
    let mut parameters = [0.0; 82];
    parameters[0] = f32::NAN;
    assert!(vocoder.process(&parameters, true, true).is_err());
}

#[test]
fn noise_reset_is_deterministic_and_bounded() {
    // Fixed deterministic noise sequence for the default seed.
    let mut reference = VoiceNoise::default();
    for expected in [
        0xfc9abc4f5279a3eb,
        0x627ab0f5a910d99a,
        0x2d399fa263b9f449,
        0x49db60aaa1abea5f,
        0xc3a06605dac203db,
        0xdbd8c6d7d7cfd4a6,
        0xcda8f7efa53eca94,
        0xbbae304332706acd,
    ] {
        assert_eq!(reference.next_u64(), expected);
    }
    let mut a = VoiceNoise::default();
    let mut b = VoiceNoise::default();
    for _ in 0..10000 {
        let x = a.uniform();
        assert_eq!(x.to_bits(), b.uniform().to_bits());
        assert!(x.abs() <= 3.0_f32.sqrt());
    }
}

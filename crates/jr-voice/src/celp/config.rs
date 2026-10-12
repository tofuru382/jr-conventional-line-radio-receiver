//! Packed little-endian tables: 128 pitch gains, 128 log gains, one bias,
//! two 64-by-5 spectral codebooks, and two 4-by-10 prediction matrices.
const DATA: &[u8; 3908] = include_bytes!("../models/celp_parameters.bin");

fn scalar(offset: usize) -> f32 {
    f32::from_le_bytes(DATA[offset..offset + 4].try_into().unwrap())
}
pub fn pitch_gain(i: usize) -> f32 {
    scalar(4 * i)
}
pub fn log_gain(i: usize) -> f32 {
    scalar(512 + 4 * i)
}
pub fn gain_bias() -> f32 {
    scalar(1024)
}
pub fn low(i: usize, k: usize) -> f32 {
    scalar(1028 + 4 * (5 * i + k))
}
pub fn high(i: usize, k: usize) -> f32 {
    scalar(2308 + 4 * (5 * i + k))
}
pub fn ma(mode: usize, tap: usize, k: usize) -> f32 {
    scalar(3588 + 4 * (mode * 40 + tap * 10 + k))
}

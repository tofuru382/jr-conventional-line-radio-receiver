use super::bits::{self, FrameParams};
use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidPadding;

impl fmt::Display for InvalidPadding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("celp158 bits 158 and 159 must be zero padding (LSB-first packing)")
    }
}
impl Error for InvalidPadding {}

/// Reorder an accepted payload into the parameter layout used by the CELP core.
/// Positions 80..84 are reserved and are not read by the parameter unpacker.
pub fn receiver_bits(payload: &[u8; 20]) -> Result<[u8; 158], InvalidPadding> {
    if payload[19] & 0xc0 != 0 {
        return Err(InvalidPadding);
    }
    let bit = |i: usize| (payload[i / 8] >> (i % 8)) & 1;
    let mut out = [0; 158];
    for (i, value) in out[..80].iter_mut().enumerate() {
        *value = bit(78 + i);
    }
    for i in [1, 5, 7] {
        out[i] ^= 1;
    }
    for i in 0..74 {
        out[84 + i] = bit(i);
    }
    Ok(out)
}

/// Unpack one accepted payload into the two CELP parameter groups.
pub fn unpack(payload: &[u8; 20]) -> Result<FrameParams, InvalidPadding> {
    Ok(bits::unpack(&receiver_bits(payload)?))
}

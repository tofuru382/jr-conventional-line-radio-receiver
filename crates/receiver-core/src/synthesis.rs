//! Frame encoders for development tools (the demodulator evaluator). The
//! receiver itself only decodes; this module exists with the `synthesis`
//! feature, which release builds leave off.
use crate::coding::{crc16, scramble, Galois};

/// A valid voice envelope for `payload` (158 bits; the top two bits of byte
/// 19 are ignored), the inverse of [`crate::voice_frame::decode`].
pub fn voice(payload: &[u8; 20], alternate: bool) -> [u8; 48] {
    crate::voice_frame::encode(payload, alternate)
}

/// A valid data envelope carrying 28 information bytes: CRC-16, then
/// Reed–Solomon (42, 30) parity, the inverse of [`crate::data::decode_frame`].
pub fn data(info: &[u8; 28]) -> [u8; 48] {
    let mut word = [0u8; 42];
    word[..28].copy_from_slice(info);
    let crc = crc16(info);
    word[28..30].copy_from_slice(&crc.to_be_bytes());
    Galois::get().encode(&mut word, 12);
    scramble(&word)
}

#[cfg(test)]
mod tests {
    #[test]
    fn encoders_round_trip_through_the_decoders() {
        let mut payload = [0u8; 20];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(41) ^ 0x3c;
        }
        payload[19] &= 0x3f;
        let speech = crate::voice_frame::decode(&super::voice(&payload, true)).unwrap();
        assert_eq!(speech.payload, payload);
        let info: [u8; 28] = std::array::from_fn(|i| (i as u8).wrapping_mul(13));
        assert_eq!(
            crate::data::decode_frame(&super::data(&info)).unwrap().0,
            info
        );
    }
}

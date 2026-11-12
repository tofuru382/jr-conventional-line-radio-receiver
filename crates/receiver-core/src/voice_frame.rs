//! Rust port of the reference FrameDecoder, including bounded joint correction.
// Keep protocol bit positions explicit for comparison with the reference.
#![allow(clippy::needless_range_loop)]
use crate::coding::{crc16, descramble, Galois};
const COORDINATES: [usize; 188] = include!("voice_coordinates.in");
const CRC_POS: [usize; 8] = [0, 1, 2, 3, 82, 83, 84, 85];

#[derive(Debug, PartialEq)]
pub struct Speech {
    pub payload: [u8; 20],
    pub corrected_bits: u16,
}
fn bit(data: &[u8], i: usize) -> u8 {
    (data[i / 8] >> (i % 8)) & 1
}
fn set(data: &mut [u8], i: usize, v: u8) {
    data[i / 8] |= v << (i % 8);
}
fn parity(v: usize) -> u8 {
    (v.count_ones() & 1) as u8
}
fn crc8(data: &[u8]) -> u8 {
    let mut state = 255u16;
    for i in 0..86 {
        state = (state << 1) | if i < 78 { data[i] as u16 } else { 0 };
        if state & 256 != 0 {
            state ^= 0x19b;
        }
    }
    state as u8
}
fn convolution(input: &[u8; 94]) -> [u8; 176] {
    let mut result = [0; 176];
    let mut state = 0;
    for i in 0..94 {
        state = ((state << 1) | input[i] as usize) & 511;
        if COORDINATES[2 * i] != 255 {
            result[COORDINATES[2 * i]] = parity(state & 0x1af);
        }
        result[COORDINATES[2 * i + 1]] = parity(state & 0x11d);
    }
    result
}
fn protected(word: &[u8; 42]) -> [u8; 176] {
    let mut r = [0; 176];
    for i in 0..88 {
        r[2 * i] = bit(&word[5..], i);
        r[2 * i + 1] = bit(&word[21..], i);
    }
    r
}
fn write_protected(word: &mut [u8; 42], data: &[u8; 176]) {
    word[5..16].fill(0);
    word[21..32].fill(0);
    for i in 0..88 {
        set(&mut word[5..], i, data[2 * i]);
        set(&mut word[21..], i, data[2 * i + 1]);
    }
}
fn inverse(received: &[u8; 176]) -> [u8; 94] {
    let mut r = [0; 94];
    let mut state = 0;
    for t in 0..94 {
        r[t] = received[COORDINATES[2 * t + 1]] ^ parity(state & (0x11d >> 1));
        state = ((state << 1) | r[t] as usize) & 255;
    }
    r
}
fn viterbi(received: &[u8; 176]) -> [u8; 94] {
    let mut costs = [255u16; 256];
    costs[0] = 0;
    let mut next = [0u16; 256];
    let mut survivors = [[0u8; 32]; 94];
    for t in 0..94 {
        let punctured = COORDINATES[2 * t] == 255;
        let observed = if punctured { 1 } else { 2 };
        let a = received[COORDINATES[2 * t + 1]];
        let b = if punctured {
            0
        } else {
            received[COORDINATES[2 * t]]
        };
        for prior in 0..128 {
            let symbol = prior * 2;
            let distance = (parity(symbol & 0x11d) ^ a) as u16
                + if punctured {
                    0
                } else {
                    (parity(symbol & 0x1af) ^ b) as u16
                };
            let complement = observed - distance;
            let (left, right) = (costs[prior], costs[prior + 128]);
            next[2 * prior] = 255.min((left + distance).min(right + complement));
            next[2 * prior + 1] = 255.min((left + complement).min(right + distance));
            set(
                &mut survivors[t],
                2 * prior,
                u8::from(right + complement < left + distance),
            );
            set(
                &mut survivors[t],
                2 * prior + 1,
                u8::from(right + distance < left + complement),
            );
        }
        std::mem::swap(&mut costs, &mut next);
    }
    let mut state = 0;
    let mut r = [0; 94];
    for t in (0..94).rev() {
        r[t] = (state & 1) as u8;
        state = (state >> 1) | ((bit(&survivors[t], state) as usize) << 7);
    }
    r
}
/// Viterbi decoding with per-bit weights: a disagreeing coded bit costs its
/// reliability, so erased bits (weight 0) carry no evidence.
fn soft_viterbi(received: &[u8; 176], weights: &[u8; 176]) -> [u8; 94] {
    const UNREACHED: u32 = u32::MAX / 4;
    let mut costs = [UNREACHED; 256];
    costs[0] = 0;
    let mut next = [0u32; 256];
    let mut survivors = [[0u8; 32]; 94];
    for t in 0..94 {
        let punctured = COORDINATES[2 * t] == 255;
        let a = received[COORDINATES[2 * t + 1]];
        let wa = weights[COORDINATES[2 * t + 1]] as u32;
        let (b, wb) = if punctured {
            (0, 0)
        } else {
            (
                received[COORDINATES[2 * t]],
                weights[COORDINATES[2 * t]] as u32,
            )
        };
        for prior in 0..128 {
            let symbol = prior * 2;
            let distance =
                (parity(symbol & 0x11d) ^ a) as u32 * wa + (parity(symbol & 0x1af) ^ b) as u32 * wb;
            let complement = wa + wb - distance;
            let (left, right) = (costs[prior], costs[prior + 128]);
            next[2 * prior] = (left + distance).min(right + complement);
            next[2 * prior + 1] = (left + complement).min(right + distance);
            set(
                &mut survivors[t],
                2 * prior,
                u8::from(right + complement < left + distance),
            );
            set(
                &mut survivors[t],
                2 * prior + 1,
                u8::from(right + distance < left + complement),
            );
        }
        let floor = *next.iter().min().unwrap_or(&0);
        for (c, n) in costs.iter_mut().zip(&next) {
            *c = (n - floor).min(UNREACHED);
        }
    }
    let mut state = 0;
    let mut r = [0; 94];
    for t in (0..94).rev() {
        r[t] = (state & 1) as u8;
        state = (state >> 1) | ((bit(&survivors[t], state) as usize) << 7);
    }
    r
}
/// Reliability of each descrambled word bit (LSB-first index) from the
/// envelope's per-bit confidence (MSB-first).
fn word_confidence(confidence: &[u8]) -> [u8; 336] {
    let mut out = [0; 336];
    for (i, o) in out.iter_mut().enumerate() {
        *o = confidence[128 * (i / 112) + 16 + i % 112];
    }
    out
}
fn tail(word: &mut [u8; 42]) {
    let crc = crc16(&word[28..34]);
    word[34..36].copy_from_slice(&crc.to_be_bytes());
    let mut work = [0; 14];
    work[..8].copy_from_slice(&word[28..36]);
    let gf = Galois::get();
    for i in 0..8 {
        let c = work[i];
        for (j, g) in [1, 63, 1, 218, 32, 227, 38].iter().enumerate() {
            work[i + j] ^= gf.mul(c, *g);
        }
    }
    word[36..].copy_from_slice(&work[8..]);
}
fn checked(word: &[u8; 42], received: &[u8; 176], input: &[u8; 94], max: u16) -> Option<Speech> {
    if input[86..].iter().any(|b| *b != 0) {
        return None;
    }
    let checksum = CRC_POS
        .iter()
        .enumerate()
        .fold(0, |v, (i, p)| v | (input[*p] << i));
    if checksum != crc8(&input[4..]) {
        return None;
    }
    let encoded = convolution(input);
    let distance: u16 = encoded
        .iter()
        .zip(received)
        .map(|(a, b)| (a ^ b) as u16)
        .sum();
    if distance > max {
        return None;
    }
    let mut corrected = *word;
    write_protected(&mut corrected, &encoded);
    tail(&mut corrected);
    if corrected[34..] != word[34..] {
        return None;
    }
    let mut payload = [0; 20];
    for i in 0..78 {
        set(&mut payload, i, input[4 + i]);
    }
    for i in 0..40 {
        set(&mut payload, 78 + i, bit(word, i));
        set(&mut payload, 118 + i, bit(&word[16..], i));
    }
    Some(Speech {
        payload,
        corrected_bits: distance,
    })
}
fn mode(word: &[u8; 42]) -> bool {
    word[32] == 0x80 && word[33] <= 1
}
fn joint(word: &[u8; 42], received: &[u8; 176], source: &[u8; 94]) -> Option<Speech> {
    let mut c = *word;
    write_protected(&mut c, &convolution(source));
    c[32] = 0x80;
    for alternate in 0..2 {
        c[33] = (word[33] & 1) ^ alternate;
        if (word[32] ^ c[32]).count_ones() + (word[33] ^ c[33]).count_ones() > 2 {
            continue;
        }
        tail(&mut c);
        let (mut outer_bits, mut outer_symbols, mut extra) = (0, 0, 0);
        for i in 28..42 {
            let bits = (c[i] ^ word[i]).count_ones();
            outer_bits += bits;
            outer_symbols += u32::from(bits != 0);
            if i >= 32 {
                extra += bits;
            }
        }
        if outer_bits > 6 || outer_symbols > 4 {
            continue;
        }
        if let Some(mut speech) = checked(&c, received, source, 8) {
            if speech.corrected_bits as u32 + extra <= 12 {
                speech.corrected_bits += extra as u16;
                return Some(speech);
            }
        }
    }
    None
}
/// Build a valid voice envelope for `payload` (158 bits; the top two bits of
/// byte 19 are ignored). This mirrors [`decode`] and exists so tests and
/// development tools can synthesize radio signals without recordings.
#[cfg(any(test, feature = "synthesis"))]
pub(crate) fn encode(payload: &[u8; 20], alternate: bool) -> [u8; 48] {
    let mut input = [0u8; 94];
    for i in 0..78 {
        input[4 + i] = bit(payload, i);
    }
    let checksum = crc8(&input[4..]);
    for (i, &p) in CRC_POS.iter().enumerate() {
        input[p] = (checksum >> i) & 1;
    }
    let mut word = [0u8; 42];
    for i in 0..40 {
        set(&mut word, i, bit(payload, 78 + i));
        set(&mut word[16..], i, bit(payload, 118 + i));
    }
    write_protected(&mut word, &convolution(&input));
    word[32] = 0x80;
    word[33] = u8::from(alternate);
    tail(&mut word);
    crate::coding::scramble(&word)
}
/// Largest Hamming distance accepted after soft decoding. Acceptance still
/// requires CRC-8 over the protected payload and the re-encoded tail
/// (CRC-16 and Reed–Solomon parity, 64 bits) to match the received tail.
const SOFT_DISTANCE: u16 = 24;

/// [`decode`], then, if that fails and per-bit reliabilities are available,
/// soft-decision Viterbi decoding and errors-and-erasures repair of the
/// Reed–Solomon tail with its least reliable bytes erased.
pub fn decode_soft(envelope: &[u8; 48], confidence: &[u8]) -> Option<Speech> {
    if let Some(speech) = decode(envelope) {
        return Some(speech);
    }
    if confidence.len() != 384 {
        return None;
    }
    let word = descramble(envelope);
    let reliability = word_confidence(confidence);
    let received = protected(&word);
    let mut weights = [0u8; 176];
    for i in 0..88 {
        weights[2 * i] = reliability[40 + i];
        weights[2 * i + 1] = reliability[168 + i];
    }
    let decoded = soft_viterbi(&received, &weights);
    // Tail bytes 28..42 from least to most reliable.
    let mut order: Vec<usize> = (0..14).collect();
    order.sort_by_key(|&b| {
        let byte = 28 + b;
        (0..8).map(|k| reliability[8 * byte + k]).min().unwrap_or(0)
    });
    let gf = Galois::get();
    let mut candidates = vec![word];
    for erased in [0, 2, 4, 6] {
        let mut repaired = word;
        let mut positions = order[..erased].to_vec();
        positions.sort_unstable();
        if gf
            .correct_with_erasures(&mut repaired[28..], 6, &positions)
            .is_some()
            && !candidates.contains(&repaired)
        {
            candidates.push(repaired);
        }
    }
    candidates.into_iter().find_map(|c| {
        if !mode(&c) || crc16(&c[28..34]) != u16::from_be_bytes([c[34], c[35]]) {
            return None;
        }
        checked(&c, &received, &decoded, SOFT_DISTANCE)
    })
}
pub fn decode(envelope: &[u8; 48]) -> Option<Speech> {
    let word = descramble(envelope);
    let received = protected(&word);
    let inverted = inverse(&received);
    let is_mode = mode(&word);
    if is_mode {
        if let Some(s) = checked(&word, &received, &inverted, 8) {
            return Some(s);
        }
    }
    let mut repaired = word;
    if Galois::get().correct(&mut repaired[28..], 6).is_some()
        && mode(&repaired)
        && crc16(&repaired[28..34]) == u16::from_be_bytes([repaired[34], repaired[35]])
        && repaired != word
    {
        let outer = protected(&repaired);
        let accepted = checked(&repaired, &received, &inverse(&outer), 8)
            .or_else(|| checked(&repaired, &received, &viterbi(&outer), 8));
        if let Some(mut s) = accepted {
            s.corrected_bits += (32..42)
                .map(|i| (word[i] ^ repaired[i]).count_ones() as u16)
                .sum::<u16>();
            return Some(s);
        }
    }
    let mut decoded = [0; 94];
    if is_mode {
        decoded = viterbi(&received);
        if let Some(s) = checked(&word, &received, &decoded, 8) {
            return Some(s);
        }
    }
    if (word[32] ^ 0x80).count_ones() + (word[33] & 0xfe).count_ones() > 2 {
        return None;
    }
    if let Some(s) = joint(&word, &received, &inverted) {
        return Some(s);
    }
    if !is_mode {
        decoded = viterbi(&received);
    }
    joint(&word, &received, &decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_frames_decode_to_the_same_payload_and_survive_bit_errors() {
        let mut state = 0x2545_f491_u32;
        for alternate in [false, true] {
            for _ in 0..50 {
                let mut payload = [0u8; 20];
                for byte in &mut payload {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    *byte = state as u8;
                }
                payload[19] &= 0x3f;
                let mut envelope = encode(&payload, alternate);
                assert_eq!(descramble(&envelope)[32], 0x80);
                let speech = decode(&envelope).expect("clean frame");
                assert_eq!(speech.payload, payload);
                assert_eq!(speech.corrected_bits, 0);
                // Two scattered errors in the convolutionally protected area
                // (word bytes 5..16 and 21..32); other payload bits are raw.
                for word_bit in [48, 203] {
                    let pos = 128 * (word_bit / 112) + 16 + word_bit % 112;
                    envelope[pos / 8] ^= 1 << (7 - pos % 8);
                }
                assert_eq!(decode(&envelope).expect("corrected").payload, payload);
            }
        }
    }

    #[test]
    fn soft_decoding_recovers_frames_hard_decoding_rejects() {
        let mut payload = [0u8; 20];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37) ^ 0x5a;
        }
        payload[19] &= 0x3f;
        let mut envelope = encode(&payload, false);
        let mut confidence = vec![200u8; 384];
        // Twelve errors spread over the protected bits, each flagged unreliable,
        // plus one corrupted tail byte that the erasure repair must restore.
        for k in 0..12 {
            let word_bit = if k % 2 == 0 { 40 + 7 * k } else { 168 + 7 * k };
            let pos = 128 * (word_bit / 112) + 16 + word_bit % 112;
            envelope[pos / 8] ^= 1 << (7 - pos % 8);
            confidence[pos] = 3;
        }
        for word_bit in 8 * 38..8 * 39 {
            let pos = 128 * (word_bit / 112) + 16 + word_bit % 112;
            envelope[pos / 8] ^= 1 << (7 - pos % 8);
            confidence[pos] = 1;
        }
        assert!(decode(&envelope).is_none());
        let speech = decode_soft(&envelope, &confidence).expect("soft decode");
        assert_eq!(speech.payload, payload);
        // Without reliabilities the soft path is the hard path.
        assert!(decode_soft(&envelope, &[]).is_none());
    }

    #[test]
    fn random_frames_are_never_accepted() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let mut envelope = [0u8; 48];
            for chunk in envelope.chunks_mut(8) {
                chunk.copy_from_slice(&next().to_le_bytes());
            }
            let confidence: Vec<u8> = (0..384).map(|_| (next() >> 56) as u8).collect();
            assert!(decode_soft(&envelope, &confidence).is_none());
            assert!(crate::data::decode_frame_soft(&envelope, &confidence).is_none());
        }
    }
}

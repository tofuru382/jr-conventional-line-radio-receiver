//! Acquisition and cadence follow the reference's 384-bit radio envelope.
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug)]
pub struct Frame {
    pub bit_offset: u64,
    pub sync_matched: bool,
    pub marker_errors: u32,
    pub received: [u8; 48],
    /// Per-bit reliability (384 entries, MSB first; 0 = erased) when the
    /// demodulator supplies soft decisions, otherwise empty.
    pub confidence: Vec<u8>,
}

const MARKERS: [u16; 3] = [0x3d22, 0xe867, 0xe867];
// Bits looked ahead so a frame can follow a one-symbol slip in each segment.
const SLIP_LOOKAHEAD: usize = 8;
// Marker bit errors (of the 14 compared bits) still accepted while acquiring,
// and while locked, with [`crate::tuning::MARKER_TOLERANCE`]. Acquisition
// stays strict because it searches every bit position; a locked frame's
// markers sit where the cadence predicts them. Shifted markers differ from
// the expected pattern in far more bits, so slips remain unambiguous.
const ACQUIRE_TOLERANCE: u32 = 1;
const LOCKED_TOLERANCE: u32 = 2;
const MARKER_MASK: u16 = 0x3fff;

/// Decides whether a candidate frame decodes; see [`Synchronizer::with_validator`].
pub type Validator = fn(&Frame) -> bool;

#[derive(Default)]
pub struct Synchronizer {
    bits: VecDeque<u8>,
    confidence: VecDeque<u8>,
    soft: bool,
    // The two bits most recently consumed, restorable when a dropped symbol
    // pulls the next frame start one symbol earlier.
    recent: VecDeque<(u8, u8)>,
    // Net bits skipped (+) or restored (-) to follow clock slips; subtracted
    // from reported offsets so locked frames stay on a 384-bit grid.
    realigned: i64,
    pub slips: u64,
    origin: u64,
    failures: u8,
    pub locked: bool,
    pub frames: u64,
    pub synchronized_frames: u64,
    /// Frames that kept the lock because they decoded although their markers
    /// were damaged (see [`crate::tuning::FLYWHEEL`]).
    pub flywheel_frames: u64,
    /// Acquisitions on a single decodable frame.
    pub single_frame_acquisitions: u64,
    validator: Option<Validator>,
    // Absolute bit position up to which single-frame candidates were tried.
    single_checked: u64,
}
impl Synchronizer {
    /// A synchronizer that may also acquire on one frame whose markers match
    /// and that `validator` decodes, and that keeps its lock through frames
    /// with damaged markers as long as they decode. A false acquisition costs
    /// nothing beyond the frames it emits until the lock is lost again: every
    /// frame is still checked by its own FEC and CRC.
    pub fn with_validator(validator: Validator) -> Self {
        Self {
            validator: Some(validator),
            ..Self::default()
        }
    }
    fn marker_value(&self, position: usize) -> u16 {
        let mut value = 0u16;
        for i in 0..16 {
            value = (value << 1) | self.bits[position + i] as u16;
        }
        value
    }
    /// Errors in the 14 compared bits, and in all 16 bits.
    fn marker_errors(&self, position: usize, segment: usize) -> (u32, u32) {
        let difference = self.marker_value(position) ^ MARKERS[segment];
        (
            (difference & MARKER_MASK).count_ones(),
            difference.count_ones(),
        )
    }
    fn marker(&self, position: usize, segment: usize) -> (bool, u32) {
        let (compared, all) = self.marker_errors(position, segment);
        (compared == 0, all)
    }
    fn tolerance(locked: bool) -> u32 {
        match (crate::tuning::on(crate::tuning::MARKER_TOLERANCE), locked) {
            (false, _) => 0,
            (true, false) => ACQUIRE_TOLERANCE,
            (true, true) => LOCKED_TOLERANCE,
        }
    }
    fn markers(&self, offset: usize, tolerance: u32) -> (bool, u32) {
        let mut matching = 0;
        let mut errors = 0;
        for m in 0..3 {
            let (compared, all) = self.marker_errors(offset + 128 * m, m);
            matching += usize::from(compared <= tolerance);
            errors += all;
        }
        (matching >= 2, errors)
    }
    fn drop_bits(&mut self, count: usize) {
        for (bit, confidence) in self.bits.drain(..count).zip(self.confidence.drain(..count)) {
            if self.recent.len() == 2 {
                self.recent.pop_front();
            }
            self.recent.push_back((bit, confidence));
        }
        self.origin += count as u64;
    }
    fn restore_bits(&mut self) {
        while let Some((bit, confidence)) = self.recent.pop_back() {
            self.bits.push_front(bit);
            self.confidence.push_front(confidence);
            self.origin -= 1;
        }
    }
    fn frame_at(&self, starts: [usize; 3], sync_matched: bool, marker_errors: u32) -> Frame {
        let mut received = [0; 48];
        let mut confidence = Vec::with_capacity(if self.soft { 384 } else { 0 });
        for i in 0..384 {
            let at = starts[i / 128] + i % 128;
            received[i / 8] |= self.bits[at] << (7 - i % 8);
            if self.soft {
                confidence.push(self.confidence[at]);
            }
        }
        Frame {
            bit_offset: (self.origin as i64 - self.realigned).max(0) as u64,
            sync_matched,
            marker_errors,
            received,
            confidence,
        }
    }
    pub fn push(&mut self, dibit: u8, output: &mut Vec<Frame>) {
        self.push_bits(dibit, [u8::MAX; 2], output);
    }
    /// As [`Self::push`] with per-bit reliabilities (MSB first) that travel
    /// with the bits into each [`Frame::confidence`].
    pub fn push_soft(&mut self, dibit: u8, confidence: [u8; 2], output: &mut Vec<Frame>) {
        self.soft = true;
        self.push_bits(dibit, confidence, output);
    }
    /// Search for a frame start while unlocked: three consecutive frames with
    /// matching markers at the trailing edge of the buffer, or (with a
    /// validator) one decodable frame at its leading edge, which finds short
    /// transmissions of one or two frames that the first never would.
    fn acquire(&mut self) {
        let tolerance = Self::tolerance(false);
        let len = self.bits.len();
        if let Some(validator) = self
            .validator
            .filter(|_| crate::tuning::on(crate::tuning::SINGLE_FRAME_ACQUISITION))
        {
            if len >= 384 {
                let first = self.single_checked.saturating_sub(self.origin) as usize;
                for pos in first..=len - 384 {
                    if self.markers(pos, tolerance).0 {
                        let (_, errors) = self.markers(pos, tolerance);
                        let frame = self.frame_at([pos, pos + 128, pos + 256], true, errors);
                        if validator(&frame) {
                            self.single_checked = self.origin + pos as u64 + 1;
                            self.drop_bits(pos);
                            self.single_frame_acquisitions += 1;
                            self.locked = true;
                            self.failures = 0;
                            return;
                        }
                    }
                }
                self.single_checked = self.origin + (len - 384) as u64 + 1;
            }
        }
        if len >= 1152 {
            let mut pos = 0;
            while pos + 1152 <= len {
                if self.markers(pos, tolerance).0
                    && self.markers(pos + 384, tolerance).0
                    && self.markers(pos + 768, tolerance).0
                {
                    self.locked = true;
                    self.failures = 0;
                    break;
                }
                pos += 1;
            }
            self.drop_bits(pos);
        }
    }
    fn push_bits(&mut self, dibit: u8, confidence: [u8; 2], output: &mut Vec<Frame>) {
        assert!(dibit < 4);
        self.bits.push_back(dibit >> 1);
        self.bits.push_back(dibit & 1);
        self.confidence.extend(confidence);
        if !self.locked {
            self.acquire();
        }
        let tolerant = crate::tuning::on(crate::tuning::MARKER_TOLERANCE);
        let tolerance = Self::tolerance(true);
        // A marker is considered moved by one symbol only when it is clearly
        // wrong in place and clearly right at the shifted position.
        let shifted = |sync: &Self, from: usize, to: usize, segment: usize| -> bool {
            if tolerant {
                sync.marker_errors(from, segment).0 > tolerance
                    && sync.marker_errors(to, segment).0 <= ACQUIRE_TOLERANCE
            } else {
                sync.marker(to, segment).0
            }
        };
        while self.locked && self.bits.len() >= 384 {
            // A clock slip inserts or drops one symbol (two bits). Each
            // 128-bit segment opens with a marker, so follow a marker that
            // moved by one symbol instead of losing lock and three frames.
            // Frames whose three markers sit in place need no lookahead.
            let in_place = (0..3).all(|segment| self.marker(128 * segment, segment).0);
            if !in_place && self.bits.len() < 384 + SLIP_LOOKAHEAD {
                break;
            }
            if !in_place && !self.marker(0, 0).0 {
                if shifted(self, 0, 2, 0) {
                    self.drop_bits(2);
                    self.realigned += 2;
                    self.slips += 1;
                } else if self.recent.len() == 2 {
                    self.restore_bits();
                    if shifted(self, 2, 0, 0) {
                        self.realigned -= 2;
                        self.slips += 1;
                    } else {
                        self.drop_bits(2);
                    }
                }
            }
            let mut starts = [0usize; 3];
            let mut shift = 0isize;
            for (segment, start) in starts.iter_mut().enumerate() {
                let base = (128 * segment) as isize + shift;
                if !in_place && segment > 0 && !self.marker(base as usize, segment).0 {
                    if shifted(self, base as usize, (base + 2) as usize, segment) {
                        shift += 2;
                        self.slips += 1;
                    } else if base >= 2
                        && shifted(self, base as usize, (base - 2) as usize, segment)
                    {
                        shift -= 2;
                        self.slips += 1;
                    }
                }
                *start = ((128 * segment) as isize + shift) as usize;
            }
            let mut matching = 0;
            let mut marker_errors = 0;
            for (segment, &start) in starts.iter().enumerate() {
                let (compared, all) = self.marker_errors(start, segment);
                matching += usize::from(compared <= tolerance);
                marker_errors += all;
            }
            let sync_matched = matching >= 2;
            let frame = self.frame_at(starts, sync_matched, marker_errors);
            if sync_matched {
                self.failures = 0;
                self.synchronized_frames += 1;
            } else if self
                .validator
                .filter(|_| crate::tuning::on(crate::tuning::FLYWHEEL))
                .is_some_and(|validator| validator(&frame))
            {
                // Damaged markers, intact frame: the cadence is still right.
                self.failures = 0;
                self.flywheel_frames += 1;
            } else {
                self.failures += 1;
            }
            self.frames += 1;
            output.push(frame);
            let consumed = (384 + shift) as usize;
            self.drop_bits(consumed);
            self.realigned += shift as i64;
            if self.failures >= 3 {
                self.locked = false;
                // Bits after the lost frames may start a new transmission.
                self.single_checked = self.origin;
            }
        }
    }
    /// Offset below which this synchronizer will emit no further frame. While
    /// searching it still holds buffered bits from here, so frames it emits on
    /// acquiring start at or after this offset too.
    pub fn settled(&self) -> Option<u64> {
        Some((self.origin as i64 - self.realigned).max(0) as u64)
    }
    pub fn packed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        for byte in bytes {
            for shift in [6, 4, 2, 0] {
                self.push((byte >> shift) & 3, &mut frames);
            }
        }
        frames
    }
}

/// Merges frames from two synchronizers fed by decision streams of equal
/// latency (primary first). Each position is released once both streams have
/// either delivered it or moved past it, choosing the primary frame unless
/// only the alternate one passes the caller's check.
// Frames from the two synchronizers that start less than half a frame apart
// cover the same transmission: their alignments can differ after slips or
// reacquisition.
const SAME_FRAME_BITS: u64 = 191;
// Released frames snap to the previous frame's 384-bit grid unless the gap
// is long enough to be a new transmission.
const GRID_FRAMES: u64 = 16;

/// Origin of a frame released by the [`Merger`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Detector {
    #[default]
    Primary,
    Alternate,
    /// Bit by bit from whichever detector was more confident; tried only when
    /// neither frame decoded on its own.
    Combined,
}
/// A decoding result the merger can rank frames by.
pub trait Decodes {
    fn ok(&self) -> bool;
}
impl Decodes for bool {
    fn ok(&self) -> bool {
        *self
    }
}
fn bit_at(bytes: &[u8; 48], i: usize) -> u8 {
    (bytes[i / 8] >> (7 - i % 8)) & 1
}
/// One frame from two detectors' frames at the same alignment: each bit from
/// the more reliable detector, its reliability the margin over the other
/// (agreeing bits keep the larger). Needs soft decisions from both.
pub fn combine(primary: &Frame, alternate: &Frame) -> Option<Frame> {
    if primary.bit_offset != alternate.bit_offset
        || primary.confidence.len() != 384
        || alternate.confidence.len() != 384
    {
        return None;
    }
    let mut out = primary.clone();
    out.received = [0; 48];
    for i in 0..384 {
        let (bp, ba) = (bit_at(&primary.received, i), bit_at(&alternate.received, i));
        let (cp, ca) = (primary.confidence[i], alternate.confidence[i]);
        let (bit, confidence) = if bp == ba {
            (bp, cp.max(ca))
        } else if cp >= ca {
            (bp, cp - ca)
        } else {
            (ba, ca - cp)
        };
        out.received[i / 8] |= bit << (7 - i % 8);
        out.confidence[i] = confidence;
    }
    Some(out)
}

/// Merges frames from two synchronizers fed by decision streams of equal
/// latency (primary first). Each position is released once both streams have
/// either delivered it or moved past it, choosing the primary frame unless
/// only the alternate one, or both combined, decodes.
#[derive(Default)]
pub struct Merger {
    pending: BTreeMap<u64, [Option<Frame>; 2]>,
    released: Option<u64>,
}
impl Merger {
    pub fn push(&mut self, stream: usize, frame: Frame) {
        let offset = frame.bit_offset;
        if self
            .released
            .is_some_and(|r| offset + SAME_FRAME_BITS < r + 384)
        {
            return;
        }
        let key = self
            .pending
            .range(offset.saturating_sub(SAME_FRAME_BITS)..=offset + SAME_FRAME_BITS)
            .map(|(&k, _)| k)
            .next()
            .unwrap_or(offset);
        let slot = &mut self.pending.entry(key).or_default()[stream];
        if slot.is_none() {
            *slot = Some(frame);
        }
    }
    /// Release settled positions in order. `settled` holds each stream's
    /// [`Synchronizer::settled`] offset. `flush` releases everything. Every
    /// released frame is decoded exactly once by `decode`, and the result is
    /// returned with it so that no caller decodes it again.
    pub fn release<D: Decodes>(
        &mut self,
        settled: [Option<u64>; 2],
        flush: bool,
        mut decode: impl FnMut(&Frame) -> D,
    ) -> Vec<(Frame, Detector, D)> {
        let mut out = Vec::new();
        while let Some((&key, slots)) = self.pending.iter().next() {
            let ready = flush
                || (0..2).all(|k| {
                    slots[k].is_some()
                        || settled[k].is_none_or(|position| position > key + SAME_FRAME_BITS)
                });
            if !ready {
                break;
            }
            let [primary, alternate] = self.pending.remove(&key).unwrap_or_default();
            let chosen = match (primary, alternate) {
                (Some(p), Some(a)) => {
                    let first = decode(&p);
                    if first.ok() {
                        (p, Detector::Primary, first)
                    } else if a.received == p.received && a.confidence == p.confidence {
                        // The detectors agreed bit for bit: nothing new to try.
                        (p, Detector::Primary, first)
                    } else {
                        let second = decode(&a);
                        if second.ok() {
                            (a, Detector::Alternate, second)
                        } else if let Some(c) = crate::tuning::on(crate::tuning::COMBINE)
                            .then(|| combine(&p, &a))
                            .flatten()
                        {
                            let third = decode(&c);
                            if third.ok() {
                                (c, Detector::Combined, third)
                            } else {
                                (p, Detector::Primary, first)
                            }
                        } else {
                            (p, Detector::Primary, first)
                        }
                    }
                }
                (Some(p), None) => {
                    let d = decode(&p);
                    (p, Detector::Primary, d)
                }
                (None, Some(a)) => {
                    let d = decode(&a);
                    (a, Detector::Alternate, d)
                }
                (None, None) => continue,
            };
            let (mut frame, alternate, decoded) = chosen;
            let offset = match self.released {
                Some(last)
                    if frame.bit_offset > last && frame.bit_offset - last < GRID_FRAMES * 384 =>
                {
                    last + ((frame.bit_offset - last + 192) / 384).max(1) * 384
                }
                _ => frame.bit_offset,
            };
            if self.released.is_some_and(|last| offset <= last) {
                continue;
            }
            self.released = Some(offset);
            frame.bit_offset = offset;
            out.push((frame, alternate, decoded));
        }
        out
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.released = None;
    }
}

#[cfg(test)]
mod tests {
    fn envelope_bits(envelope: &[u8; 48]) -> impl Iterator<Item = u8> + '_ {
        (0..192).map(move |s| {
            let byte = envelope[s / 4];
            (byte >> (6 - 2 * (s % 4))) & 3
        })
    }

    fn valid_voice() -> [u8; 48] {
        let mut payload = [0u8; 20];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(29) ^ 0x33;
        }
        payload[19] &= 0x3f;
        crate::voice_frame::encode(&payload, false)
    }

    fn noise_dibits(count: usize, seed: &mut u32) -> Vec<u8> {
        (0..count)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                (*seed & 3) as u8
            })
            .collect()
    }

    #[test]
    fn a_single_decodable_frame_is_acquired_and_short_bursts_are_not_lost() {
        let mut seed = 0x1234_5678;
        for burst in 1..=2 {
            let mut sync = super::Synchronizer::with_validator(|f| {
                crate::voice_frame::decode(&f.received).is_some()
            });
            let mut plain = super::Synchronizer::default();
            let mut dibits = noise_dibits(700, &mut seed);
            let envelope = valid_voice();
            for _ in 0..burst {
                dibits.extend(envelope_bits(&envelope));
            }
            dibits.extend(noise_dibits(700, &mut seed));
            let (mut with, mut without) = (Vec::new(), Vec::new());
            for &d in &dibits {
                sync.push(d, &mut with);
                plain.push(d, &mut without);
            }
            let decoded = with
                .iter()
                .filter(|f| crate::voice_frame::decode(&f.received).is_some())
                .count();
            assert_eq!(decoded, burst, "burst of {burst}");
            assert_eq!(sync.single_frame_acquisitions, 1);
            // Three consecutive frames are needed without a validator.
            assert!(without.is_empty());
        }
    }

    #[test]
    fn damaged_markers_keep_the_lock_while_frames_decode() {
        let clean = valid_voice();
        // Three bit errors in two of the three markers: no longer a marker
        // match even with the locked tolerance, but the frame is intact.
        let mut damaged = clean;
        damaged[16] ^= 0x07;
        damaged[32] ^= 0x07;
        let run = |flywheel: bool| {
            let mut sync = if flywheel {
                super::Synchronizer::with_validator(|f| {
                    crate::voice_frame::decode(&f.received).is_some()
                })
            } else {
                super::Synchronizer::default()
            };
            let mut frames = Vec::new();
            for envelope in std::iter::repeat_n(&clean, 4).chain(std::iter::repeat_n(&damaged, 20))
            {
                for d in envelope_bits(envelope) {
                    sync.push(d, &mut frames);
                }
            }
            // A frame with damaged markers waits for a few bits of lookahead.
            for _ in 0..8 {
                sync.push(0, &mut frames);
            }
            let decoded = frames
                .iter()
                .filter(|f| crate::voice_frame::decode(&f.received).is_some())
                .count();
            (decoded, sync.flywheel_frames)
        };
        assert_eq!(run(true), (24, 20));
        // Without it, three damaged frames end the lock.
        assert!(run(false).0 < 10);
    }

    #[test]
    fn markers_with_one_bit_error_still_acquire() {
        let mut envelope = valid_voice();
        envelope[0] ^= 0x01;
        envelope[16] ^= 0x01;
        let mut sync = super::Synchronizer::default();
        let mut frames = Vec::new();
        for _ in 0..6 {
            for d in envelope_bits(&envelope) {
                sync.push(d, &mut frames);
            }
        }
        assert!(frames.len() >= 3);
        assert!(frames.iter().all(|f| f.sync_matched));
    }
}

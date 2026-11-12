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
}
impl Synchronizer {
    fn marker(&self, position: usize, segment: usize) -> (bool, u32) {
        let mut value = 0u16;
        for i in 0..16 {
            value = (value << 1) | self.bits[position + i] as u16;
        }
        let expected = MARKERS[segment];
        (
            value & 0x3fff == expected & 0x3fff,
            (value ^ expected).count_ones(),
        )
    }
    fn markers(&self, offset: usize) -> (bool, u32) {
        let mut matching = 0;
        let mut errors = 0;
        for (m, expected) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
            let mut value = 0u16;
            for i in 0..16 {
                value = (value << 1) | self.bits[offset + 128 * m + i] as u16;
            }
            matching += usize::from(value & 0x3fff == expected & 0x3fff);
            errors += (value ^ expected).count_ones();
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
    pub fn push(&mut self, dibit: u8, output: &mut Vec<Frame>) {
        self.push_bits(dibit, [u8::MAX; 2], output);
    }
    /// As [`Self::push`] with per-bit reliabilities (MSB first) that travel
    /// with the bits into each [`Frame::confidence`].
    pub fn push_soft(&mut self, dibit: u8, confidence: [u8; 2], output: &mut Vec<Frame>) {
        self.soft = true;
        self.push_bits(dibit, confidence, output);
    }
    fn push_bits(&mut self, dibit: u8, confidence: [u8; 2], output: &mut Vec<Frame>) {
        assert!(dibit < 4);
        self.bits.push_back(dibit >> 1);
        self.bits.push_back(dibit & 1);
        self.confidence.extend(confidence);
        if !self.locked && self.bits.len() >= 1152 {
            let mut pos = 0;
            while pos + 1152 <= self.bits.len() {
                if self.markers(pos).0 && self.markers(pos + 384).0 && self.markers(pos + 768).0 {
                    self.locked = true;
                    self.failures = 0;
                    break;
                }
                pos += 1;
            }
            self.drop_bits(pos);
        }
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
                if self.marker(2, 0).0 {
                    self.drop_bits(2);
                    self.realigned += 2;
                    self.slips += 1;
                } else if self.recent.len() == 2 {
                    self.restore_bits();
                    if self.marker(0, 0).0 {
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
                    if self.marker((base + 2) as usize, segment).0 {
                        shift += 2;
                        self.slips += 1;
                    } else if self.marker((base - 2) as usize, segment).0 {
                        shift -= 2;
                        self.slips += 1;
                    }
                }
                *start = ((128 * segment) as isize + shift) as usize;
            }
            let mut matching = 0;
            let mut marker_errors = 0;
            for (segment, &start) in starts.iter().enumerate() {
                let (ok, errors) = self.marker(start, segment);
                matching += usize::from(ok);
                marker_errors += errors;
            }
            let sync_matched = matching >= 2;
            let mut received = [0; 48];
            let mut confidence = Vec::with_capacity(if self.soft { 384 } else { 0 });
            for i in 0..384 {
                let at = starts[i / 128] + i % 128;
                received[i / 8] |= self.bits[at] << (7 - i % 8);
                if self.soft {
                    confidence.push(self.confidence[at]);
                }
            }
            if sync_matched {
                self.failures = 0;
                self.synchronized_frames += 1;
            } else {
                self.failures += 1;
            }
            self.frames += 1;
            output.push(Frame {
                bit_offset: (self.origin as i64 - self.realigned).max(0) as u64,
                sync_matched,
                marker_errors,
                received,
                confidence,
            });
            let consumed = (384 + shift) as usize;
            self.drop_bits(consumed);
            self.realigned += shift as i64;
            if self.failures >= 3 {
                self.locked = false;
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

/// Merges frames from two synchronizers fed by decision streams of equal
/// latency (primary first). Each position is released once both streams have
/// either delivered it or moved past it, choosing the primary frame unless
/// only the alternate one passes the caller's check.
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
    /// [`Synchronizer::settled`] offset. `flush` releases everything.
    pub fn release(
        &mut self,
        settled: [Option<u64>; 2],
        flush: bool,
        mut decodes: impl FnMut(&Frame) -> bool,
    ) -> Vec<(Frame, bool)> {
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
                    if decodes(&p) || !decodes(&a) {
                        (p, false)
                    } else {
                        (a, true)
                    }
                }
                (Some(p), None) => (p, false),
                (None, Some(a)) => (a, true),
                (None, None) => continue,
            };
            let (mut frame, alternate) = chosen;
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
            out.push((frame, alternate));
        }
        out
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.released = None;
    }
}

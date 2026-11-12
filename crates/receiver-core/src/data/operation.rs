//! Common-wave 0x22 / C9 line tables, compared with the reference decoder.
//! Accept only the observed 0x87 seven-row layout. Unknown bytes remain exported.
//! Prefix bytes have no timestamp interpretation until their meaning is established.
use super::{add, hex, lha, message, packet::Packet, text, unknown, Message, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationRow {
    pub line: String,
    pub delayed: Option<u8>,
    pub on_line: Option<u8>,
    pub delay_classes: [Option<u8>; 3],
    pub max_delay_min: Option<u8>,
    pub train_no: Option<String>,
    pub raw_hex: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationSummary {
    pub rows: Vec<OperationRow>,
    pub prefix_hex: String,
    pub expanded_hex: String,
    // Observed relationship only, not an established protocol checksum rule.
    pub prefix_sum_matches: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OperationReception {
    pub first_received_at: String,
    pub last_received_at: String,
    pub receptions: u64,
    pub summary: OperationSummary,
}
impl OperationReception {
    pub fn update(current: &mut Option<Self>, message: &Message, received_at: String) {
        let Some(summary) = &message.operation_summary else {
            return;
        };
        if let Some(previous) = current {
            // Compare opaque content only; completed tables arrive in decoder order.
            if previous.summary == *summary {
                previous.last_received_at = received_at;
                previous.receptions = previous.receptions.saturating_add(1);
                return;
            }
        }
        *current = Some(Self {
            first_received_at: received_at.clone(),
            last_received_at: received_at,
            receptions: 1,
            summary: summary.clone(),
        });
    }
}

#[derive(Default)]
pub(super) struct Assembler {
    key: Vec<u8>,
    body: Vec<u8>,
    part: u8,
    first: u64,
    last: u64,
}
impl Assembler {
    pub fn accept(&mut self, p: Packet) -> Result<Option<Message>> {
        let b = &p.bytes;
        if b.len() < 16 || b[2] != 5 || b[10..12] != [1, 1] || b[13..16] != [0xc9, 1, 0x21] {
            *self = Self::default();
            return Ok(Some(unknown(p)));
        }
        let (part, total) = (b[12] >> 4, b[12] & 15);
        if part == 0 || part > total {
            *self = Self::default();
            return Err("Invalid operation table part");
        }
        let mut key = b[2..12].to_vec();
        key.extend_from_slice(&[total, b[13], b[14], b[15]]);
        if part == 1 {
            self.body.clear();
            self.key = key;
            self.first = p.first;
        } else if self.part + 1 != part
            || self.key != key
            || p.first.checked_sub(self.last) != Some(384)
        {
            *self = Self::default();
            return Ok(None);
        }
        self.body.extend_from_slice(&b[16..]);
        self.part = part;
        self.last = p.last;
        if self.body.len() > 15 * 241 {
            *self = Self::default();
            return Err("Operation table size limit");
        }
        if part != total {
            return Ok(None);
        }
        let first = self.first;
        let body = std::mem::take(&mut self.body);
        *self = Self::default();
        let summary = decode(&body)?;
        let mut m = message("OperationSummary", hex(&b[8..10]), &p);
        m.first_bit = first;
        m.raw_hex = hex(&body);
        // Leave timestamp unset. Reception time is recorded separately, and the
        // uninterpreted prefix remains available in raw_hex and prefix_hex.
        add(&mut m.fields, "LineCount", summary.rows.len());
        m.operation_summary = Some(summary);
        Ok(Some(m))
    }
}

fn value(byte: u8) -> Option<u8> {
    (byte & 0x80 != 0).then_some(byte & 0x7f)
}
fn train(bytes: &[u8]) -> Option<String> {
    let digits = hex(&bytes[..2]);
    let digits = digits.trim_start_matches('F');
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || !(1..=26).contains(&bytes[2])
    {
        return None;
    }
    Some(format!(
        "{}{}",
        digits.parse::<u16>().ok()?,
        (b'A' + bytes[2] - 1) as char
    ))
}
fn decode(body: &[u8]) -> Result<OperationSummary> {
    if body.len() < 11 {
        return Err("Truncated operation table prefix");
    }
    // The prefix is not the service-information destination/checksum header.
    // Do not search arbitrary payload bytes for a plausible archive signature.
    let expanded = lha::expand(&body[11..])?;
    if expanded.len() != 1 + 7 * 21 || expanded[0] != 0x87 {
        return Err("Unsupported operation table layout");
    }
    let mut rows = Vec::with_capacity(7);
    for row in expanded[1..].as_chunks::<21>().0 {
        if row[..2] != [7, 0] {
            return Err("Unsupported operation row");
        }
        let mut line = String::new();
        for pair in row[2..10].as_chunks::<2>().0 {
            if !pair.iter().all(|b| (0x21..=0x7e).contains(b)) {
                return Err("Invalid operation line name");
            }
            line.push_str(&text::jis(pair[0], pair[1])?);
        }
        let line = line.replace('\u{3000}', "");
        if line.is_empty() {
            return Err("Empty operation line name");
        }
        rows.push(OperationRow {
            line,
            max_delay_min: value(row[11]),
            train_no: train(&row[13..16]),
            delayed: value(row[16]),
            on_line: value(row[17]),
            delay_classes: [value(row[18]), value(row[19]), value(row[20])],
            raw_hex: hex(row),
        });
    }
    Ok(OperationSummary {
        rows,
        prefix_hex: hex(&body[..11]),
        expanded_hex: hex(&expanded),
        prefix_sum_matches: super::sum(&expanded) == super::be(&body[9..11]),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        coding::{crc16, Galois},
        data::{Decoder, Format},
        frame::Frame,
        pipeline::{ChannelConfig, Pipeline},
    };

    fn table() -> Vec<u8> {
        let mut bytes = vec![0x87];
        for i in 0..7u8 {
            // Entirely synthetic names, counts and train numbers.
            bytes.extend([7, 0]);
            bytes.extend([0x3b, 0x6e, 0x38, 0x33, 0x23, 0x41 + i, 0x21, 0x21]); // 試験Ａ…Ｇ
            bytes.extend([
                0x80,
                0x80 | (7 + i),
                0,
                0xf0,
                0x12,
                3,
                0x83,
                0xa8,
                0x82,
                0x81,
                0x80,
            ]);
        }
        bytes
    }
    fn archive(data: &[u8]) -> Vec<u8> {
        // A valid LH5 stream with one constant-literal block per byte.
        // No radio capture or copied compressed payload is used.
        let mut bits = Vec::new();
        for &byte in data {
            for (v, n) in [
                (1u16, 16),
                (0, 5),
                (0, 5),
                (0, 9),
                (byte as u16, 9),
                (0, 4),
                (0, 4),
            ] {
                for bit in (0..n).rev() {
                    bits.push(((v >> bit) & 1) as u8);
                }
            }
        }
        let mut packed = vec![0; bits.len().div_ceil(8)];
        for (i, bit) in bits.iter().enumerate() {
            packed[i / 8] |= bit << (7 - i % 8);
        }
        let mut a = vec![0u8; 28];
        a[0] = 26;
        a[2..7].copy_from_slice(b"-lh5-");
        a[7..11].copy_from_slice(&(packed.len() as u32).to_le_bytes());
        a[11..15].copy_from_slice(&(data.len() as u32).to_le_bytes());
        a[20] = 1;
        a[21] = 1;
        a[22] = b't';
        let mut crc = 0u16;
        for byte in data {
            crc ^= *byte as u16;
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 != 0 { 0xa001 } else { 0 };
            }
        }
        a[23..25].copy_from_slice(&crc.to_le_bytes());
        a[1] = a[2..].iter().fold(0u8, |s, b| s.wrapping_add(*b));
        a.extend(packed);
        a
    }
    fn body(data: &[u8]) -> Vec<u8> {
        let mut b = vec![1, 0, 1];
        b.extend([0x19, 0x02, 0xaa, 0x7f, 0, 0xff]); // Opaque synthetic prefix.
        b.extend(super::super::sum(data).to_be_bytes());
        b.extend(archive(data));
        b
    }
    fn packets(body: &[u8], serial: u16) -> Vec<Packet> {
        let total = body.len().div_ceil(240) as u8;
        let mut offset = 0;
        body.chunks(240)
            .enumerate()
            .map(|(i, part)| {
                let mut bytes = vec![0x22, (14 + part.len()) as u8, 5, 0, 0, 0, 0x10, 2];
                bytes.extend(serial.to_be_bytes());
                bytes.extend([1, 1, ((i as u8 + 1) << 4) | total, 0xc9, 1, 0x21]);
                bytes.extend(part);
                let first = offset;
                offset += bytes.len().div_ceil(27) as u64 * 384;
                Packet {
                    bytes,
                    first,
                    last: offset - 384,
                }
            })
            .collect()
    }
    fn frames(packets: &[Packet]) -> Vec<Frame> {
        let gf = Galois::get();
        let mut generator = vec![1u8];
        for root in 0..12 {
            let mut next = vec![0; generator.len() + 1];
            for (i, c) in generator.iter().enumerate() {
                next[i] ^= c;
                next[i + 1] ^= gf.mul(*c, gf.powers[root]);
            }
            generator = next;
        }
        let mut frames = Vec::new();
        for p in packets {
            let count = p.bytes.len().div_ceil(27);
            for (i, part) in p.bytes.chunks(27).enumerate() {
                let mut word = [0u8; 42];
                word[0] = if count == 1 {
                    0xc0 | part.len() as u8
                } else if i == 0 {
                    0x80 | (count - 1) as u8
                } else if i + 1 == count {
                    0x40 | part.len() as u8
                } else {
                    (count - i - 1) as u8
                };
                word[1..1 + part.len()].copy_from_slice(part);
                let crc = crc16(&word[..28]);
                word[28..30].copy_from_slice(&crc.to_be_bytes());
                let info = word;
                for j in 0..30 {
                    let c = word[j];
                    for k in 0..13 {
                        word[j + k] ^= gf.mul(c, generator[k]);
                    }
                }
                word[..30].copy_from_slice(&info[..30]);
                let mut received = [0u8; 48];
                for (i, marker) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
                    received[16 * i..16 * i + 2].copy_from_slice(&marker.to_be_bytes());
                }
                let mut pn = [1u8; 336];
                for i in 0..336 {
                    if i >= 9 {
                        pn[i] = pn[i - 9] ^ pn[i - 5];
                    }
                    let pos = 128 * (i / 112) + 16 + i % 112;
                    received[pos / 8] |= (((word[i / 8] >> (i % 8)) & 1) ^ pn[i]) << (7 - pos % 8);
                }
                assert_eq!(super::super::decode_frame(&received).unwrap().0, word[..28]);
                frames.push(Frame {
                    bit_offset: p.first + i as u64 * 384,
                    sync_matched: true,
                    marker_errors: 0,
                    received,
                    confidence: Vec::new(),
                });
            }
        }
        frames
    }
    pub(crate) fn decoded() -> Message {
        let mut a = Assembler::default();
        packets(&body(&table()), 0x1234)
            .into_iter()
            .filter_map(|p| a.accept(p).unwrap())
            .next()
            .unwrap()
    }
    #[test]
    fn synthetic_frames_reach_structured_json_and_monitor_without_recording() {
        let config = ChannelConfig {
            id: "test-common".into(),
            label: "Synthetic common".into(),
            frequency_hz: 352625000.0,
            format: Format::Common,
        };
        let mut pipe = Pipeline::new(config, 240000.0, 352625000.0).unwrap();
        let input = frames(&packets(&body(&table()), 0x1234));
        let mut exported = Vec::new();
        for frame in &input {
            pipe.frame_observed(frame, &mut |event| exported.extend(event.messages))
                .unwrap();
        }
        assert_eq!(exported.len(), 1);
        let message = &exported[0];
        assert_eq!(message.kind, "OperationSummary");
        assert_eq!(message.timestamp, None);
        let summary = message.operation_summary.as_ref().unwrap();
        assert_eq!(summary.rows.len(), 7);
        assert_eq!(summary.rows[0].line, "試験Ａ");
        assert_eq!(summary.rows[0].on_line, Some(40));
        assert_eq!(summary.rows[0].train_no.as_deref(), Some("12C"));
        assert!(summary.prefix_sum_matches);
        let json = serde_json::to_value(message).unwrap();
        assert!(json["timestamp"].is_null());
        assert_eq!(
            json["operation_summary"]["rows"][0]["delay_classes"],
            serde_json::json!([2, 1, 0])
        );
        assert_eq!(pipe.snapshot().operation_monitor.unwrap().summary, *summary);
        assert!(serde_json::to_value(pipe.snapshot().operation_monitor)
            .unwrap()
            .get("message_time")
            .is_none());
        // IQ loss resets in-progress assembly/history, not the completed table.
        pipe.discontinuity().unwrap();
        assert!(pipe.snapshot().messages.is_empty());
        assert!(pipe.snapshot().operation_monitor.is_some());
    }
    #[test]
    fn missing_reordered_mixed_or_corrupted_parts_cannot_publish_a_table() {
        let p = packets(&body(&table()), 0x1234);
        for indices in [vec![1, 2, 3, 4], vec![0, 2, 3, 4], vec![0, 2, 1, 3, 4]] {
            let mut a = Assembler::default();
            for i in indices {
                assert!(a.accept(p[i].clone()).unwrap().is_none());
            }
        }
        let mut mixed = p.clone();
        mixed[1].bytes[9] ^= 1;
        let mut a = Assembler::default();
        for part in mixed {
            assert!(a.accept(part).unwrap().is_none());
        }
        let mut input = frames(&p);
        input[2].received = [0; 48];
        let mut decoder = Decoder::new(Format::Common);
        assert!(input
            .iter()
            .flat_map(|f| decoder.decode(f))
            .next()
            .is_none());
        assert!(decoder.stats.rejected_frames > 0);
        // A clean new first part recovers after corruption.
        assert_eq!(frames(&p).iter().flat_map(|f| decoder.decode(f)).count(), 1);
    }
    #[test]
    fn invalid_archives_and_layouts_are_rejected_and_unknown_is_not_zero() {
        let original = body(&table());
        for n in 0..original.len() {
            assert!(decode(&original[..n]).is_err());
        }
        let mut damaged = original.clone();
        damaged[12] ^= 1;
        assert!(decode(&damaged).is_err()); // archive header checksum
        let mut damaged = original.clone();
        damaged[34] ^= 1;
        // Fix header checksum to isolate the content CRC check.
        damaged[12] = damaged[13..39].iter().fold(0u8, |s, b| s.wrapping_add(*b));
        assert!(decode(&damaged).is_err());
        let mut rows = table();
        rows[0] = 0x88;
        assert!(decode(&body(&rows)).is_err());
        rows = table();
        rows[1] = 8;
        assert!(decode(&body(&rows)).is_err());
        rows = table();
        rows[17] = 0;
        rows[21] = 0x80;
        rows[14] = 0xff;
        rows[15] = 0xff;
        let decoded = decode(&body(&rows)).unwrap();
        assert_eq!(decoded.rows[0].delayed, None);
        assert_eq!(decoded.rows[0].delay_classes[2], Some(0));
        assert_eq!(decoded.rows[0].train_no, None);
        let mut different_prefix = original;
        different_prefix[9] ^= 1;
        assert!(!decode(&different_prefix).unwrap().prefix_sum_matches);
    }
    #[test]
    fn opaque_prefix_is_preserved_without_date_validation_or_timestamp_output() {
        let original = body(&table());
        for prefix in [[0; 6], [0xff; 6], [153, 131, 142, 137, 154, 181]] {
            let mut input = original.clone();
            input[3..9].copy_from_slice(&prefix);
            let mut decoder = Decoder::new(Format::Common);
            let messages: Vec<_> = frames(&packets(&input, 0x1234))
                .iter()
                .flat_map(|f| decoder.decode(f))
                .collect();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].timestamp, None);
            assert_eq!(messages[0].raw_hex, hex(&input));
            let summary = messages[0].operation_summary.as_ref().unwrap();
            assert_eq!(summary.prefix_hex, hex(&input[..11]));
            assert_eq!(summary.rows, decode(&original).unwrap().rows);
            assert_eq!(decoder.stats.rejected_messages, 0);
        }
    }
    #[test]
    fn retransmissions_count_identical_content_and_replace_changed_tables_in_receive_order() {
        let mut state = None;
        let mut message = decoded();
        OperationReception::update(&mut state, &message, "first".into());
        message.identity = "4567".into();
        OperationReception::update(&mut state, &message, "second".into());
        let received = state.as_ref().unwrap();
        assert_eq!(received.first_received_at, "first");
        assert_eq!(received.last_received_at, "second");
        assert_eq!(received.receptions, 2);
        message.timestamp = Some("2024-12-31T23:59:59".into());
        OperationReception::update(&mut state, &message, "third".into());
        message.timestamp = None;
        OperationReception::update(&mut state, &message, "fourth".into());
        assert_eq!(state.as_ref().unwrap().last_received_at, "fourth");
        assert_eq!(state.as_ref().unwrap().receptions, 4);
        let mut rows = table();
        rows[17] = 0x80;
        message.operation_summary = Some(decode(&body(&rows)).unwrap());
        OperationReception::update(&mut state, &message, "fifth".into());
        assert_eq!(state.as_ref().unwrap().receptions, 1);
        assert_eq!(state.as_ref().unwrap().summary.rows[0].delayed, Some(0));
        assert_eq!(state.as_ref().unwrap().first_received_at, "fifth");
        // Different unknown prefix bytes also replace the stored original bytes.
        let mut changed = body(&rows);
        changed[3] = 0;
        message.operation_summary = Some(decode(&changed).unwrap());
        OperationReception::update(&mut state, &message, "sixth".into());
        assert_eq!(state.as_ref().unwrap().receptions, 1);
        assert_eq!(state.as_ref().unwrap().first_received_at, "sixth");
        assert_eq!(state.unwrap().summary.prefix_hex, hex(&changed[..11]));
    }
    #[test]
    fn known_service_information_still_decodes_and_old_json_remains_readable() {
        // Plain, uncompressed synthetic service message with seven empty fields.
        let mut content = vec![0x25, 0x03, 0x14, 0x09, 0x26];
        content.extend([0; 7]);
        let mut body = vec![1, 0, 1, 0, 1, 1, 0x99, 0x99];
        let sum = super::super::sum(&body).wrapping_add(super::super::sum(&content));
        body.extend(sum.to_be_bytes());
        body.extend(content);
        let mut p = packets(&body, 1);
        p[0].bytes[0] = 0x21;
        p[0].bytes[13] = 0x46;
        let mut decoder = Decoder::new(Format::Common);
        let messages: Vec<_> = frames(&p).iter().flat_map(|f| decoder.decode(f)).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, "ServiceInformation");
        assert_eq!(
            messages[0].timestamp.as_deref(),
            Some("2025-03-14T09:26:00")
        );
        let json = serde_json::to_value(&messages[0]).unwrap();
        assert!(json.get("operation_summary").is_none());
        assert!(serde_json::from_value::<Message>(json)
            .unwrap()
            .operation_summary
            .is_none());
    }
}

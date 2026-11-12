//! Verified data frames and bounded application messages. Unknown codes stay raw.
mod lha;
mod operation;
mod packet;
mod text;
use crate::{
    coding::{crc16, descramble, Galois},
    frame::Frame,
};
#[cfg(test)]
pub(crate) use operation::tests::decoded as synthetic_operation;
pub use operation::{OperationReception, OperationRow, OperationSummary};
use packet::{Assembler, Packet};
use serde::{Deserialize, Serialize};
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Common,
    Slotted,
}
impl Format {
    pub fn for_frequency(hz: f64) -> Self {
        if (hz - 352_625_000.0).abs() < 3125.0 {
            Self::Common
        } else {
            Self::Slotted
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Field {
    pub field: String,
    pub value: String,
    pub english: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub kind: String,
    pub identity: String,
    pub timestamp: Option<String>,
    pub fields: Vec<Field>,
    pub raw_hex: String,
    pub first_bit: u64,
    pub last_bit: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_summary: Option<OperationSummary>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub valid_frames: u64,
    pub corrected_frames: u64,
    pub rejected_frames: u64,
    pub rejected_messages: u64,
}
pub struct Decoder {
    format: Format,
    packets: Assembler,
    body: Vec<u8>,
    key: Option<String>,
    part: u8,
    first: u64,
    last: u64,
    operation: operation::Assembler,
    pub stats: Stats,
}
pub fn decode_frame(envelope: &[u8; 48]) -> Option<([u8; 28], usize)> {
    let mut word = descramble(envelope);
    let corrections = Galois::get().correct(&mut word, 12)?;
    if crc16(&word[..28]) != u16::from_be_bytes([word[28], word[29]]) {
        return None;
    }
    Some((word[..28].try_into().unwrap(), corrections))
}
/// Most bytes erased in one soft retry. Twelve roots allow twelve erasures;
/// stopping at eight keeps four roots of detection on top of CRC-16.
const MAX_ERASURES: usize = 8;

/// [`decode_frame`], then, if that fails and per-bit reliabilities are
/// available, errors-and-erasures retries with the least reliable bytes erased.
pub fn decode_frame_soft(envelope: &[u8; 48], confidence: &[u8]) -> Option<([u8; 28], usize)> {
    if let Some(result) = decode_frame(envelope) {
        return Some(result);
    }
    if confidence.len() != 384 {
        return None;
    }
    let word = descramble(envelope);
    let byte_reliability = |byte: usize| {
        (0..8)
            .map(|k| {
                let i = 8 * byte + k;
                confidence[128 * (i / 112) + 16 + i % 112]
            })
            .min()
            .unwrap_or(0)
    };
    let mut order: Vec<usize> = (0..42).collect();
    order.sort_by_key(|&b| byte_reliability(b));
    let gf = Galois::get();
    (2..=MAX_ERASURES).step_by(2).find_map(|erased| {
        let mut repaired = word;
        let mut positions = order[..erased].to_vec();
        positions.sort_unstable();
        let corrections = gf.correct_with_erasures(&mut repaired, 12, &positions)?;
        (crc16(&repaired[..28]) == u16::from_be_bytes([repaired[28], repaired[29]]))
            .then(|| (repaired[..28].try_into().unwrap(), corrections))
    })
}
impl Decoder {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            packets: Assembler::new(format),
            body: Vec::new(),
            key: None,
            part: 0,
            first: 0,
            last: 0,
            operation: operation::Assembler::default(),
            stats: Stats::default(),
        }
    }
    pub fn reset_assembly(&mut self) {
        self.packets.reset();
        self.key = None;
        self.part = 0;
        self.body.clear();
        self.operation = operation::Assembler::default();
    }
    pub fn decode(&mut self, frame: &Frame) -> Vec<Message> {
        let Some((info, corrected)) = decode_frame_soft(&frame.received, &frame.confidence) else {
            self.stats.rejected_frames += 1;
            self.reset_assembly();
            return Vec::new();
        };
        self.stats.valid_frames += 1;
        self.stats.corrected_frames += u64::from(corrected > 0);
        if self.format == Format::Slotted {
            if matches!(info[0] >> 4, 0 | 8) && info[1] == 0x20 {
                return channels(&info, frame.bit_offset);
            }
            if info[0] >> 4 == 10 && info[8] == 0x20 && info[9] == 3 {
                return vec![call(&info, frame.bit_offset)];
            }
        }
        let Some(packet) = self.packets.accept(&info, frame.bit_offset) else {
            return Vec::new();
        };
        let result = if self.format == Format::Common {
            self.common(packet)
        } else {
            traffic(packet).map(Some)
        };
        match result {
            Ok(Some(m)) => vec![m],
            Ok(None) => Vec::new(),
            Err(_) => {
                self.stats.rejected_messages += 1;
                self.reset_assembly();
                Vec::new()
            }
        }
    }
    fn common(&mut self, p: Packet) -> Result<Option<Message>> {
        if p.bytes.first() == Some(&0x22) {
            return self.operation.accept(p);
        }
        let b = &p.bytes;
        if b.len() < 16 || b[0] != 0x21 {
            return Ok(Some(unknown(p)));
        }
        if b[2] == 4 {
            if b.len() < 21
                || !(b.len() - 21).is_multiple_of(2)
                || usize::from(b[18]) != (b.len() - 21) / 2
                || sum(&b[13..b.len() - 2]) != be(&b[b.len() - 2..])
            {
                return Err("Invalid message directory");
            }
            let mut m = message("MessageDirectory", "directory", &p);
            // Five BCD components (YY MM DD hh mm), followed by a binary
            // message-ID count. The count is not a seconds component.
            m.timestamp = Some(stamp(&b[13..18])?);
            add(&mut m.fields, "MessageIds", pairs(&b[19..b.len() - 2]));
            return Ok(Some(m));
        }
        if b[2] != 5
            || b[10] != 1
            || b[11] != 1
            || !matches!(b[13], 0x46 | 0xc6)
            || b[14] != 1
            || b[15] != 0x21
        {
            return Ok(Some(unknown(p)));
        }
        let part = b[12] >> 4;
        let total = b[12] & 15;
        let key = format!("{}{}{}", hex(&b[2..12]), total, hex(&b[13..16]));
        if part < 1 || part > total {
            return Err("Invalid service part");
        }
        if part == 1 {
            self.body.clear();
            self.key = Some(key.clone());
            self.part = 0;
            self.first = p.first;
        } else if self.key.as_ref() != Some(&key)
            || part != self.part + 1
            || p.first.checked_sub(self.last) != Some(384)
        {
            self.key = None;
            self.body.clear();
            return Ok(None);
        }
        self.body.extend_from_slice(&b[16..]);
        self.part = part;
        self.last = p.last;
        if self.body.len() > 15 * 241 {
            return Err("Service size limit");
        }
        if part != total {
            return Ok(None);
        }
        let body = std::mem::take(&mut self.body);
        self.key = None;
        if body.len() < 10 || body[0] != 1 || body[5] == 0 {
            return Err("Invalid service body");
        }
        let terminal = 6 + 2 * (body[5] as usize - 1);
        if terminal + 4 > body.len() || body[terminal..terminal + 2] != [0x99, 0x99] {
            return Err("Invalid destinations");
        }
        let content = if b[13] == 0xc6 {
            lha::expand(&body[terminal + 4..])?
        } else {
            body[terminal + 4..].to_vec()
        };
        if sum(&body[..terminal + 2]).wrapping_add(sum(&content)) != be(&body[terminal + 2..]) {
            return Err("Service checksum mismatch");
        }
        let date = stamp(content.get(..5).ok_or("Missing service date")?)?;
        let mut fields = text::service(&content)?;
        add(&mut fields, "RelatedMessage", hex(&body[3..5]));
        add(&mut fields, "DestinationCodes", pairs(&body[6..terminal]));
        Ok(Some(Message {
            kind: "ServiceInformation".into(),
            identity: hex(&body[1..3]),
            timestamp: Some(date),
            fields,
            raw_hex: hex(&content),
            first_bit: self.first,
            last_bit: p.last,
            operation_summary: None,
        }))
    }
}
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02X}")).collect()
}
fn pairs(b: &[u8]) -> String {
    b.chunks(2).map(hex).collect::<Vec<_>>().join(" ")
}
fn be(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn sum(b: &[u8]) -> u16 {
    b.iter().fold(0u16, |s, b| s.wrapping_add(*b as u16))
}
fn bcd(b: &[u8]) -> Option<u32> {
    let mut n = 0;
    for v in b {
        if v >> 4 > 9 || v & 15 > 9 {
            return None;
        }
        n = n * 100 + (v >> 4) as u32 * 10 + (v & 15) as u32;
    }
    Some(n)
}
fn train(b: &[u8]) -> Option<String> {
    if !(1..=26).contains(&b[2]) {
        None
    } else {
        bcd(&b[..2]).map(|n| format!("{n}{}", (64 + b[2]) as char))
    }
}
fn stamp(b: &[u8]) -> Result<String> {
    if !matches!(b.len(), 5 | 6) {
        return Err("Invalid date length");
    }
    let mut v = [0; 6];
    for (i, byte) in b.iter().enumerate() {
        v[i] = bcd(&[*byte]).ok_or("Invalid BCD date")?;
    }
    let date = chrono::NaiveDate::from_ymd_opt(2000 + v[0] as i32, v[1], v[2])
        .ok_or("Invalid message date")?;
    let datetime = date
        .and_hms_opt(v[3], v[4], v[5])
        .ok_or("Invalid message time")?;
    Ok(datetime.format("%Y-%m-%dT%H:%M:%S").to_string())
}
fn add(f: &mut Vec<Field>, name: &str, value: impl ToString) {
    f.push(Field {
        field: name.into(),
        value: value.to_string(),
        english: None,
    });
}
fn optional(f: &mut Vec<Field>, name: &str, v: Option<impl ToString>) {
    if let Some(v) = v {
        add(f, name, v);
    }
}
fn identity(f: &mut Vec<Field>, b: &[u8]) {
    add(f, "ClassCode", hex(&b[..2]));
    add(f, "Formation", hex(&b[2..3]));
}
fn message(kind: &str, id: impl ToString, p: &Packet) -> Message {
    Message {
        kind: kind.into(),
        identity: id.to_string(),
        timestamp: None,
        fields: Vec::new(),
        raw_hex: hex(&p.bytes),
        first_bit: p.first,
        last_bit: p.last,
        operation_summary: None,
    }
}
fn unknown(p: Packet) -> Message {
    message("Unsupported", format!("{:02X}", p.bytes[0]), &p)
}
fn traffic(p: Packet) -> Result<Message> {
    let b = &p.bytes;
    let mut f = Vec::new();
    let (kind, id, date) = if b.len() == 18 && b[..3] == [0x51, 0x10, 0] {
        identity(&mut f, &b[3..6]);
        add(&mut f, "UnassignedBytes", hex(&b[6..]));
        ("Presence", hex(&b[3..6]), None)
    } else if b.len() == 31 && b[0] == 5 {
        let phase = b[4] & 127;
        identity(&mut f, &b[9..12]);
        for (name, idx) in [
            ("Subtype", 2),
            ("CabFlags", 3),
            ("VehicleKind", 12),
            ("LineCode", 17),
        ] {
            add(&mut f, name, hex(&b[idx..idx + 1]));
        }
        add(&mut f, "DispatchNotice", u8::from(b[4] & 128 != 0));
        add(&mut f, "NumberPhase", phase);
        if phase != 3 {
            optional(&mut f, "TrainNumber", train(&b[6..9]));
        }
        if b[5] & 0xc0 == 0x80 {
            add(&mut f, "NumberPrefix", b[5] & 63);
        }
        if (0xf0..=0xf9).contains(&b[13]) {
            optional(
                &mut f,
                "VehicleSeries",
                bcd(&b[14..15]).map(|n| format!("{}{n:02}", b[13] & 15)),
            );
        }
        optional(&mut f, "VehicleNumber", bcd(&b[15..17]));
        add(&mut f, "StationCode", be(&b[18..]));
        optional(&mut f, "RadioNumber", bcd(&b[22..24]));
        add(
            &mut f,
            "UnassignedBytes",
            format!("{} / {:02X}", hex(&b[20..22]), b[24]),
        );
        ("Registration", hex(&b[9..12]), Some(stamp(&b[25..31])?))
    } else if b.len() >= 23 && b[0] == 0x15 && b[6] == 0x10 && b[7] == 4 {
        if sum(&b[15..]) != be(&b[13..]) {
            return Err("Transfer checksum mismatch");
        }
        identity(&mut f, &b[3..6]);
        add(&mut f, "ToAll", u8::from(b[2] == 4));
        add(&mut f, "Serial", be(&b[8..]));
        add(&mut f, "RecordKind", hex(&b[10..11]));
        add(&mut f, "DataKind", hex(&b[21..22]));
        add(
            &mut f,
            "UnassignedBytes",
            format!("{} / {}", hex(&b[11..13]), hex(&b[22..])),
        );
        ("Transfer", hex(&b[8..10]), Some(stamp(&b[15..21])?))
    } else if matches!(b[0], 1 | 3)
        && b.len() >= 126
        && !(b[10] == 1 && b[11] == 1)
        && matches!(b[10], 0x10 | 0x12)
    {
        if sum(&b[34..b.len() - 2]) != be(&b[b.len() - 2..]) {
            return Err("Notice checksum mismatch");
        }
        if b[2] == 0 {
            identity(&mut f, &b[3..6]);
        }
        for (name, start, end) in [
            ("TargetMode", 2, 3),
            ("Layout", 10, 11),
            ("Category", 14, 18),
            ("NoticeNumber", 18, 20),
            ("IssuedAtBytes", 41, 47),
            ("FromStationCode", 105, 108),
            ("ToStationCode", 116, 119),
        ] {
            add(&mut f, name, hex(&b[start..end]));
        }
        optional(&mut f, "TrainNumber", train(&b[29..32]));
        add(&mut f, "From", text::notice(&b[97..105], true));
        add(&mut f, "To", text::notice(&b[108..116], true));
        add(
            &mut f,
            "NoticeText",
            text::notice(&b[124..b.len() - 2], false),
        );
        add(&mut f, "TextBytes", hex(&b[124..b.len() - 2]));
        ("Notice", hex(&b[8..10]), None)
    } else {
        return Ok(unknown(p));
    };
    let mut m = message(kind, id, &p);
    m.timestamp = date;
    m.fields = f;
    Ok(m)
}
fn call(info: &[u8; 28], offset: u64) -> Message {
    let mut f = Vec::new();
    let id = hex(&info[1..3]);
    add(&mut f, "CallAddress", &id);
    add(&mut f, "NumberState", hex(&info[3..4]));
    optional(&mut f, "TrainNumber", train(&info[4..7]));
    add(&mut f, "CallCabCode", hex(&info[7..8]));
    optional(&mut f, "RadioNumber", bcd(&info[10..12]));
    add(&mut f, "CallResultCode", hex(&info[12..14]));
    if info[12] == 0xff {
        add(&mut f, "CallSetupStep", info[13]);
    }
    Message {
        kind: "CallSetup".into(),
        identity: id,
        timestamp: None,
        fields: f,
        raw_hex: hex(info),
        first_bit: offset,
        last_bit: offset,
        operation_summary: None,
    }
}
fn channels(info: &[u8; 28], offset: u64) -> Vec<Message> {
    (0..2)
        .map(|i| {
            let r = &info[4 + i * 11..15 + i * 11];
            let mut f = Vec::new();
            let ch = bcd(&r[..1]);
            optional(&mut f, "Channel", ch);
            optional(&mut f, "Frequency", ch.map(|n| 352531250 + 6250 * n));
            add(&mut f, "ChannelStatus", hex(&r[1..2]));
            add(&mut f, "Serial", r[2]);
            add(&mut f, "NumberState", hex(&r[3..4]));
            if r[3] != 0 {
                optional(&mut f, "TrainNumber", train(&r[4..7]));
            }
            add(&mut f, "CabRole", hex(&r[7..8]));
            if r[10] == 0x80 {
                add(
                    &mut f,
                    "RadioNumber",
                    ((r[8] as u16 & 31) << 8) | r[9] as u16,
                );
            }
            add(&mut f, "ControlIdentifier", hex(&info[2..4]));
            Message {
                kind: "ChannelActivity".into(),
                identity: i.to_string(),
                timestamp: None,
                fields: f,
                raw_hex: hex(r),
                first_bit: offset,
                last_bit: offset,
                operation_summary: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(count: u8) -> Packet {
        let mut bytes = vec![0; 13];
        bytes[0] = 0x21;
        bytes[2] = 4;
        // Synthetic minute-resolution date and IDs; no captured data.
        bytes.extend([0x25, 0x03, 0x14, 0x09, 0x26, count]);
        for id in 0..u16::from(count) {
            bytes.extend((0xa000 + id).to_be_bytes());
        }
        bytes.extend(sum(&bytes[13..]).to_be_bytes());
        bytes[1] = (bytes.len() - 2) as u8;
        Packet {
            bytes,
            first: 384,
            last: 768,
        }
    }

    #[test]
    fn directory_count_is_not_a_seconds_component() {
        let mut decoder = Decoder::new(Format::Common);
        // Decimal-looking counts previously invented seconds; 0x0D/0x0F
        // previously rejected otherwise valid directories as invalid BCD.
        for count in [0, 1, 9, 13, 15, 16, 118] {
            let packet = directory(count);
            let raw = hex(&packet.bytes);
            let m = decoder.common(packet).unwrap().unwrap();
            assert_eq!(m.kind, "MessageDirectory");
            assert_eq!(m.timestamp.as_deref(), Some("2025-03-14T09:26:00"));
            assert_eq!(m.raw_hex, raw);
            let ids = m.fields.iter().find(|f| f.field == "MessageIds").unwrap();
            assert_eq!(ids.value.split_whitespace().count(), usize::from(count));
            if count != 0 {
                assert_eq!(ids.value.split_whitespace().next(), Some("A000"));
            }
        }
    }

    #[test]
    fn directory_rejects_inconsistent_counts_invalid_dates_and_corruption() {
        let mut decoder = Decoder::new(Format::Common);
        for (offset, value) in [(18, 12), (14, 0x13), (17, 0x6a)] {
            let mut packet = directory(13);
            packet.bytes[offset] = value;
            let end = packet.bytes.len() - 2;
            let checksum = sum(&packet.bytes[13..end]).to_be_bytes();
            packet.bytes[end..].copy_from_slice(&checksum);
            assert!(decoder.common(packet).is_err());
        }
        let mut packet = directory(13);
        packet.bytes[19] ^= 1;
        assert!(decoder.common(packet).is_err());
    }
}

#[cfg(test)]
mod soft_tests {
    use super::*;
    use crate::coding::scramble;

    #[test]
    fn unreliable_bytes_are_erased_beyond_the_errors_only_limit() {
        let gf = Galois::get();
        let mut word = [0u8; 42];
        for (i, b) in word[..28].iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(29).wrapping_add(7);
        }
        let crc = crc16(&word[..28]);
        word[28..30].copy_from_slice(&crc.to_be_bytes());
        gf.encode(&mut word, 12);
        let mut corrupted = word;
        let mut confidence = vec![180u8; 384];
        // Eight byte errors: beyond six-error correction, within eight erasures.
        for byte in [1, 6, 11, 17, 23, 30, 35, 40] {
            corrupted[byte] ^= 0xa5;
            for k in 0..8 {
                let i = 8 * byte + k;
                confidence[128 * (i / 112) + 16 + i % 112] = 2;
            }
        }
        let envelope = scramble(&corrupted);
        assert!(decode_frame(&envelope).is_none());
        let (info, corrections) = decode_frame_soft(&envelope, &confidence).expect("erasures");
        assert_eq!(info, word[..28]);
        assert_eq!(corrections, 8);
    }
}

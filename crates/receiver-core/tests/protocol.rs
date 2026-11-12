use receiver_core::{
    data::Format,
    frame::{Frame, Merger, Synchronizer},
    pipeline::{presets, validate_channels},
};

#[test]
fn synchronization_requires_three_envelopes_and_drops_after_three_failures() {
    let mut envelope = [0u8; 48];
    for (i, m) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
        envelope[16 * i..16 * i + 2].copy_from_slice(&m.to_be_bytes());
    }
    let mut s = Synchronizer::default();
    assert!(s.packed(&envelope).is_empty());
    assert!(s.packed(&envelope).is_empty());
    assert_eq!(s.packed(&envelope).len(), 3);
    assert!(s.locked);
    // A frame whose markers are out of place waits one byte (four symbols)
    // to check for a one-symbol clock slip before it is emitted.
    assert!(s.packed(&[0; 48]).is_empty());
    for _ in 0..2 {
        assert_eq!(s.packed(&[0; 48]).len(), 1);
    }
    assert_eq!(s.packed(&[0]).len(), 1);
    assert!(!s.locked);
}

#[test]
fn a_one_symbol_slip_keeps_lock_and_later_frames() {
    let mut envelope = [0u8; 48];
    for (i, m) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
        envelope[16 * i..16 * i + 2].copy_from_slice(&m.to_be_bytes());
    }
    for (i, byte) in envelope.iter_mut().enumerate() {
        if i % 16 >= 2 {
            *byte = (i as u8).wrapping_mul(73) ^ 0x96;
        }
    }
    let dibits = |bytes: &[u8]| -> Vec<u8> {
        bytes
            .iter()
            .flat_map(|b| [6, 4, 2, 0].map(|s| (b >> s) & 3))
            .collect()
    };
    for slip in [Slip::Inserted, Slip::Dropped] {
        let mut stream = Vec::new();
        for _ in 0..4 {
            stream.extend(dibits(&envelope));
        }
        // A slip in the middle of the fifth frame's second segment.
        let mut fifth = dibits(&envelope);
        match slip {
            Slip::Inserted => fifth.insert(100, 1),
            Slip::Dropped => {
                fifth.remove(100);
            }
        }
        stream.extend(fifth);
        for _ in 0..4 {
            stream.extend(dibits(&envelope));
        }
        let mut s = Synchronizer::default();
        let mut frames = Vec::new();
        for d in stream {
            s.push(d, &mut frames);
        }
        assert!(s.locked, "{slip:?}");
        assert_eq!(s.slips, 1, "{slip:?}");
        // Every frame after the slipped one (frames 5..9) is intact and stays
        // on the 384-bit grid; the slipped frame keeps its third segment.
        let after: Vec<_> = frames.iter().filter(|f| f.bit_offset >= 5 * 384).collect();
        assert_eq!(after.len(), 4, "{slip:?}");
        let slipped = frames.iter().find(|f| f.bit_offset == 4 * 384).unwrap();
        assert!(slipped.sync_matched, "{slip:?}");
        assert_eq!(slipped.received[32..], envelope[32..], "{slip:?}");
        for f in after {
            assert_eq!(f.received, envelope, "{slip:?}");
            assert_eq!(f.bit_offset % 384, 0, "{slip:?}");
        }
    }
}
#[derive(Debug, Clone, Copy)]
enum Slip {
    Inserted,
    Dropped,
}
#[test]
fn chart_has_22_channels_and_checks_actual_capture_bandwidth() {
    let c = presets("east");
    assert_eq!(c.len(), 22);
    assert_eq!(c[0].frequency_hz, 352537500.0);
    assert_eq!(c[21].frequency_hz, 352668750.0);
    assert!(validate_channels(&c, 240000.0, 352603125.0).is_ok());
    assert!(validate_channels(&c, 96000.0, 352603125.0).is_err());
    assert_eq!(presets("west")[1].format, Format::Common);
    assert!(presets("central")
        .iter()
        .all(|c| c.format == Format::Slotted));
}

fn frame(offset: u64, tag: u8) -> Frame {
    Frame {
        bit_offset: offset,
        sync_matched: true,
        marker_errors: 0,
        received: [tag; 48],
        confidence: Vec::new(),
    }
}

#[test]
fn merger_prefers_the_primary_frame_and_falls_back_to_the_alternate() {
    let mut m = Merger::default();
    // Position 0: both decode -> primary. Position 384: only the alternate
    // decodes -> alternate, on the primary grid although it started 2 bits late.
    m.push(0, frame(0, 1));
    m.push(1, frame(0, 2));
    m.push(0, frame(384, 0));
    m.push(1, frame(386, 2));
    let out = m.release([Some(1000), Some(1000)], false, |f| f.received[0] != 0);
    let picked: Vec<_> = out
        .iter()
        .map(|(f, alt)| (f.bit_offset, f.received[0], *alt))
        .collect();
    assert_eq!(picked, vec![(0, 1, false), (384, 2, true)]);
}

#[test]
fn merger_waits_for_a_stream_that_may_still_deliver_a_position() {
    let mut m = Merger::default();
    m.push(0, frame(768, 0));
    // The alternate synchronizer has not consumed past 768 yet.
    assert!(m
        .release([Some(1152), Some(700)], false, |_| true)
        .is_empty());
    m.push(1, frame(860, 5));
    let out = m.release([Some(1152), Some(1244)], false, |f| f.received[0] == 5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.received[0], 5);
    assert!(out[0].1);
    // Frames older than the last released position are dropped.
    m.push(0, frame(400, 9));
    assert!(m.release([None, None], true, |_| true).is_empty());
}

//! Stream contract tests using only payloads generated in memory.
use jr_conventional_voice::receiver::Voice;

type Event = (u32, Option<[u8; 20]>);

fn payload(seed: u8) -> [u8; 20] {
    let mut bytes = std::array::from_fn(|i| {
        seed.wrapping_mul(29)
            .wrapping_add((i as u8).wrapping_mul(17))
            ^ 0x5a
    });
    bytes[19] &= 0x3f;
    bytes
}

fn drain_indices(voice: &mut Voice, output: &mut Vec<u32>) {
    while let Some(frame) = voice.read() {
        output.push(frame.index);
    }
}

fn decode_indices(events: &[Event]) -> Vec<u32> {
    let mut voice = Voice::default();
    let mut indices = Vec::new();
    for &(index, payload) in events {
        voice.push(index, payload).unwrap();
        drain_indices(&mut voice, &mut indices);
    }
    voice.flush().unwrap();
    drain_indices(&mut voice, &mut indices);
    voice.flush().unwrap();
    assert!(
        voice.read().is_none(),
        "a second flush must not replay the tail"
    );
    indices
}

#[test]
fn first_frame_waits_for_four_future_frames_and_flush_keeps_tail() {
    let mut voice = Voice::default();
    for i in 0..5 {
        voice.push(i, Some(payload(i as u8))).unwrap();
        if i < 4 {
            assert!(voice.read().is_none(), "160 ms synthesis lookahead");
        }
    }
    assert_eq!(voice.read().unwrap().index, 0);
    assert!(voice.read().is_none());
    voice.flush().unwrap();
    let mut indices = Vec::new();
    drain_indices(&mut voice, &mut indices);
    assert_eq!(indices, [1, 2, 3, 4]);
}

#[test]
fn short_calls_emit_every_accepted_frame_at_eof() {
    for length in 1..=4 {
        let events: Vec<_> = (0..length)
            .map(|i| (42 + i, Some(payload(i as u8))))
            .collect();
        assert_eq!(
            decode_indices(&events),
            (42..42 + length).collect::<Vec<_>>()
        );
    }
}

#[test]
fn missing_slots_and_gaps_preserve_only_accepted_indices() {
    let events = [
        (0, Some(payload(0))),
        (1, Some(payload(1))),
        (2, None),
        (5, None),
        (6, Some(payload(2))),
        (7, Some(payload(3))),
        (8, None),
        (9, None),
        (10, Some(payload(4))),
        (11, None),
    ];
    assert_eq!(decode_indices(&events), [0, 1, 6, 7, 10]);
}

#[test]
fn frequent_two_frame_runs_drain_without_losing_or_duplicating_frames() {
    let indices: Vec<_> = (0..8).flat_map(|i| [5 * i, 5 * i + 1]).collect();
    let events: Vec<_> = indices
        .iter()
        .map(|&i| (i, Some(payload(i as u8))))
        .collect();
    assert_eq!(decode_indices(&events), indices);
}

#[test]
fn maximum_u32_index_survives_gap_and_flush() {
    let events = [
        (u32::MAX - 4, Some(payload(0))),
        (u32::MAX - 2, None),
        (u32::MAX - 1, Some(payload(1))),
        (u32::MAX, Some(payload(2))),
    ];
    assert_eq!(
        decode_indices(&events),
        [u32::MAX - 4, u32::MAX - 1, u32::MAX]
    );
}

#[test]
fn missing_only_stream_has_no_output() {
    let events: Vec<_> = (0..8).map(|i| (i, None)).collect();
    assert!(decode_indices(&events).is_empty());
}

//! Deterministic payloads exercise the full streaming contract without recordings.
use jr_conventional_voice::receiver::Voice;

#[test]
fn owned_voice_fits_an_ordinary_worker_stack() {
    assert!(std::mem::size_of::<Voice>() <= 2 * std::mem::size_of::<usize>());
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let events: Vec<_> = (0..9).map(|i| (i, Some(payload(i)))).collect();
            assert_eq!(decode(&events).len(), 9);
        })
        .unwrap()
        .join()
        .unwrap();
}

fn payload(index: u32) -> [u8; 20] {
    let mut state = index as u64 + 1;
    let mut result = std::array::from_fn(|_| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (state >> 32) as u8
    });
    result[19] &= 63;
    result
}

fn decode(events: &[(u32, Option<[u8; 20]>)]) -> Vec<(u32, [i16; 320])> {
    let mut voice = Voice::default();
    let mut output = Vec::new();
    for &(index, bytes) in events {
        voice.push(index, bytes).unwrap();
        let before = output.len();
        while let Some(frame) = voice.read() {
            output.push((frame.index, frame.pcm));
        }
        assert!(
            output.len() - before <= 1,
            "one output per receive operation"
        );
    }
    voice.flush().unwrap();
    while let Some(frame) = voice.read() {
        output.push((frame.index, frame.pcm));
    }
    voice.flush().unwrap();
    assert!(voice.read().is_none());
    output
}

#[test]
fn repeated_gaps_do_not_exhaust_the_input_queue() {
    for missing_events in [false, true] {
        let mut events = Vec::new();
        let mut expected = Vec::new();
        let mut index = 0;
        for run in 0..24 {
            for _ in 0..1 + (run * 7) % 19 {
                events.push((index, Some(payload(index))));
                expected.push(index);
                index += 1;
            }
            if missing_events {
                events.push((index, None));
            }
            index += 1;
        }
        let actual = decode(&events);
        assert_eq!(actual.iter().map(|r| r.0).collect::<Vec<_>>(), expected);
        assert_eq!(actual, decode(&events), "replay resets all synthesis state");
    }
}

#[test]
fn seventh_future_frame_is_the_latest_possible_dependency() {
    let events: Vec<_> = (0..32).map(|i| (i, Some(payload(i)))).collect();
    let baseline = decode(&events);
    let mut changed = events.clone();
    changed[24].1 = Some(payload(1337));
    let actual = decode(&changed);
    assert_eq!(baseline[..17], actual[..17]);
    assert!(baseline[17..]
        .iter()
        .zip(&actual[17..])
        .any(|(a, b)| a != b));
}

#[test]
fn states_from_an_earlier_call_do_not_leak_past_a_gap() {
    let first: Vec<_> = (0..16).map(|i| (i, Some(payload(i)))).collect();
    let second: Vec<_> = (25..45).map(|i| (i, Some(payload(i)))).collect();
    let mut a = first.clone();
    a.extend(&second);
    let mut b = first;
    for (i, p) in &mut b {
        *p = Some(payload(*i + 1000));
    }
    b.extend(&second);
    let a = decode(&a);
    let b = decode(&b);
    assert_eq!(a[16..], b[16..]);
}

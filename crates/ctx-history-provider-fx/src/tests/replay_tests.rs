use std::{
    fs,
    io::{self, BufRead, Cursor, Read},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use crate::{
    decode_authority, decode_replay_checkpoint, decode_watermark, encode_replay_checkpoint,
    replay_committed, replay_suffix, BoundaryIntent, ColdReplayDisposition, FxDigest,
    FxProviderError, PendingIntent, ReplayLimits, SuffixDisposition, TempFileScratch,
};

use super::support::{
    assistant_turn, assistant_turn_with_work, authority, canonical_state, cold, frame,
    history_payload, history_payload_with_work, id, public_fx_fixture, started, watermark, SESSION,
};

#[test]
fn cold_replays_public_v006_tool_free_and_read_file_fixtures() {
    for (session, generation) in [
        (
            "v0.0.6/native-v3-tool-free/.fx/sessions/1700000000001-1700000000000000001-0000000000000001",
            "b82f00a357b44301d54300d2856e934b",
        ),
        (
            "v0.0.6/native-v3-read-file/.fx/sessions/1700000000002-1700000000000000002-0000000000000002",
            "9e802c6cbfa70c6fe90430a1b335398e",
        ),
    ] {
        let session = public_fx_fixture(session);
        let authority = decode_authority(&fs::read(session.join("authority.json")).unwrap(), ReplayLimits::default()).unwrap();
        let watermark = decode_watermark(
            &fs::read(session.join(format!("commit.{generation}.json"))).unwrap(),
            ReplayLimits::default(),
        )
        .unwrap();
        let log = fs::read(session.join("events.jsonl")).unwrap();
        let replay = replay_committed(
            &authority,
            &watermark,
            &mut Cursor::new(log),
            BoundaryIntent::Stable,
            &TempFileScratch,
            ReplayLimits::default(),
        )
        .unwrap();
        assert!(matches!(replay, ColdReplayDisposition::Canonical(_)));
    }
}

fn run_cold(
    log: &[u8],
    commit: &crate::FxWatermark,
    boundary: BoundaryIntent,
    limits: ReplayLimits,
) -> crate::FxProviderResult<ColdReplayDisposition> {
    replay_committed(
        &authority(),
        commit,
        &mut Cursor::new(log),
        boundary,
        &TempFileScratch,
        limits,
    )
}

#[test]
fn cold_replays_authentic_ordinary_v006_frames() {
    let mut log = started(1, id(1));
    log.extend(frame(
        id(0x11),
        2,
        id(2),
        2,
        "history_turn_committed",
        history_payload(assistant_turn(
            "complete user text",
            "complete assistant text",
        )),
    ));
    let replay = cold(&log, &watermark(&log, 2, id(2)));
    assert_eq!(replay.state.history.len(), 1);
    assert_eq!(replay.checkpoint.next_seq, 3);
    assert_eq!(replay.checkpoint.absolute_turn_slots, 1);
    assert_eq!(replay.checkpoint.current_workspace_root, "/workspace/root");
}

#[test]
fn append_returns_only_new_turns_and_compact_checkpoint() {
    let prefix = started(1, id(1));
    let replay = cold(&prefix, &watermark(&prefix, 1, id(1)));
    let suffix = frame(
        id(0x11),
        2,
        id(2),
        2,
        "history_turn_committed",
        history_payload(assistant_turn("next", "reply")),
    );
    let mut complete = prefix.clone();
    complete.extend_from_slice(&suffix);
    let disposition = replay_suffix(
        &authority(),
        &replay.checkpoint,
        &watermark(&complete, 2, id(2)),
        &mut Cursor::new(&suffix),
        BoundaryIntent::Stable,
        ReplayLimits::default(),
    )
    .expect("append succeeds");
    let SuffixDisposition::AppendNewTurns(append) = disposition else {
        panic!("expected ordinary append");
    };
    assert_eq!(append.new_turns.len(), 1);
    assert_eq!(append.new_turns[0].absolute_ordinal, 0);
    assert_eq!(append.checkpoint.absolute_turn_slots, 1);
    assert_eq!(
        append.checkpoint.through_event_log_bytes,
        complete.len() as u64
    );
}

#[test]
fn checkpoint_serialization_is_deterministic_bounded_and_strict() {
    let prefix = started(1, id(1));
    let checkpoint = cold(&prefix, &watermark(&prefix, 1, id(1))).checkpoint;
    let first = encode_replay_checkpoint(&checkpoint).expect("checkpoint encodes");
    let second = encode_replay_checkpoint(&checkpoint).expect("checkpoint encodes again");
    assert_eq!(first, second);
    assert_eq!(
        decode_replay_checkpoint(&first, ReplayLimits::default()).expect("checkpoint decodes"),
        checkpoint
    );
    let mut unknown: Value = serde_json::from_slice(&first).expect("test JSON");
    unknown
        .as_object_mut()
        .expect("checkpoint object")
        .insert("unknown".to_owned(), json!(true));
    assert!(decode_replay_checkpoint(
        &serde_json::to_vec(&unknown).expect("test JSON encodes"),
        ReplayLimits::default()
    )
    .is_err());
}

#[test]
fn suffix_replacement_or_summary_requests_canonical_retry() {
    let prefix = started(1, id(1));
    let checkpoint = cold(&prefix, &watermark(&prefix, 1, id(1))).checkpoint;
    let cases = [
        (
            "state_replacement_started",
            json!({
                "replacement_id": id(9),
                "reason": "compaction",
                "encoded_bytes": 1,
                "sha256": FxDigest([0; 32]),
                "chunk_count": 1
            }),
        ),
        (
            "history_turn_committed",
            history_payload(json!({
                "kind": "compacted_summary",
                "summary": "summary",
                "removed_turn_count": 2,
                "compaction_count": 1
            })),
        ),
    ];
    for (kind, payload) in cases {
        let suffix = frame(id(0x11), 2, id(2), 2, kind, payload);
        let mut complete = prefix.clone();
        complete.extend_from_slice(&suffix);
        let disposition = replay_suffix(
            &authority(),
            &checkpoint,
            &watermark(&complete, 2, id(2)),
            &mut Cursor::new(&suffix),
            BoundaryIntent::Stable,
            ReplayLimits::default(),
        )
        .expect("suffix classifies");
        assert!(matches!(
            disposition,
            SuffixDisposition::ReplaceCanonicalState
        ));
    }
}

#[test]
fn work_id_is_copied_and_conflicts_are_rejected() {
    let prefix = started(1, id(1));
    let checkpoint = cold(&prefix, &watermark(&prefix, 1, id(1))).checkpoint;
    let copied = frame(
        id(0x11),
        2,
        id(2),
        2,
        "history_turn_committed",
        history_payload_with_work(assistant_turn("u", "a"), "work-λ"),
    );
    let mut complete = prefix.clone();
    complete.extend_from_slice(&copied);
    let SuffixDisposition::AppendNewTurns(append) = replay_suffix(
        &authority(),
        &checkpoint,
        &watermark(&complete, 2, id(2)),
        &mut Cursor::new(&copied),
        BoundaryIntent::Stable,
        ReplayLimits::default(),
    )
    .expect("work association succeeds") else {
        panic!("expected append");
    };
    assert_eq!(
        append.new_turns[0]
            .turn
            .structured_value()
            .expect("turn JSON")["user"]["work_id"],
        "work-λ"
    );

    for payload in [
        history_payload(assistant_turn_with_work("u", "a", "turn-only")),
        history_payload_with_work(
            assistant_turn_with_work("u", "a", "turn-work"),
            "event-work",
        ),
        history_payload_with_work(assistant_turn("u", "a"), &"x".repeat(129)),
    ] {
        let suffix = frame(id(0x11), 2, id(2), 2, "history_turn_committed", payload);
        let mut full = prefix.clone();
        full.extend_from_slice(&suffix);
        assert!(replay_suffix(
            &authority(),
            &checkpoint,
            &watermark(&full, 2, id(2)),
            &mut Cursor::new(&suffix),
            BoundaryIntent::Stable,
            ReplayLimits::default(),
        )
        .is_err());
    }
}

#[test]
fn sequence_generation_watermark_and_unknown_events_are_fatal() {
    let first = started(1, id(1));
    let bad_seq = frame(
        id(0x11),
        3,
        id(2),
        2,
        "history_turn_committed",
        history_payload(assistant_turn("u", "a")),
    );
    let mut log = first.clone();
    log.extend_from_slice(&bad_seq);
    assert!(matches!(
        run_cold(
            &log,
            &watermark(&log, 3, id(2)),
            BoundaryIntent::Stable,
            ReplayLimits::default()
        ),
        Err(FxProviderError::NonContiguousSequence { .. })
    ));

    let changed = frame(
        id(0x22),
        2,
        id(2),
        2,
        "history_turn_committed",
        history_payload(assistant_turn("u", "a")),
    );
    let mut log = first.clone();
    log.extend_from_slice(&changed);
    assert!(matches!(
        run_cold(
            &log,
            &watermark(&log, 2, id(2)),
            BoundaryIntent::Stable,
            ReplayLimits::default()
        ),
        Err(FxProviderError::GenerationChanged)
    ));

    let unknown = frame(id(0x11), 2, id(2), 2, "future_state_change", json!({}));
    let mut log = first.clone();
    log.extend_from_slice(&unknown);
    assert!(matches!(
        run_cold(
            &log,
            &watermark(&log, 2, id(2)),
            BoundaryIntent::Stable,
            ReplayLimits::default()
        ),
        Err(FxProviderError::UnknownEventKind(_))
    ));

    let mut wrong = watermark(&first, 1, id(7));
    assert!(matches!(
        run_cold(
            &first,
            &wrong,
            BoundaryIntent::Stable,
            ReplayLimits::default()
        ),
        Err(FxProviderError::WatermarkMismatch)
    ));
    wrong.through_event_id = id(1);
    wrong.through_event_log_bytes += 1;
    assert!(matches!(
        run_cold(
            &first,
            &wrong,
            BoundaryIntent::Stable,
            ReplayLimits::default()
        ),
        Err(FxProviderError::WatermarkMismatch)
    ));
}

#[test]
fn provisional_tail_is_ignored_and_terminal_pending_is_temporary() {
    let committed = started(1, id(1));
    let mut physical = committed.clone();
    physical.extend_from_slice(b"provisional-not-json");
    assert!(matches!(
        run_cold(
            &physical,
            &watermark(&committed, 1, id(1)),
            BoundaryIntent::ProvisionalTail {
                bytes_after_watermark: (physical.len() - committed.len()) as u64
            },
            ReplayLimits::default()
        )
        .expect("committed prefix remains readable"),
        ColdReplayDisposition::Canonical(_)
    ));
    assert!(matches!(
        run_cold(
            b"malformed",
            &watermark(&committed, 1, id(1)),
            BoundaryIntent::TerminalPending(PendingIntent::AuthorityTransition),
            ReplayLimits::default()
        )
        .expect("pending is availability, not quarantine"),
        ColdReplayDisposition::UnsafePending(PendingIntent::AuthorityTransition)
    ));
}

fn replacement_log(
    mut state_bytes: Vec<u8>,
    mutate: impl FnOnce(&mut String, &mut String, &mut String, &mut u64),
) -> (Vec<u8>, crate::FxWatermark) {
    while state_bytes.len() % 3 != 1 {
        state_bytes.push(b' ');
    }
    let aggregate = hex_digest(&state_bytes);
    let mut start_digest = aggregate.clone();
    let mut chunk_digest = aggregate.clone();
    let commit_digest = aggregate;
    let mut raw_bytes = state_bytes.len() as u64;
    let mut encoded = STANDARD.encode(&state_bytes);
    mutate(
        &mut encoded,
        &mut start_digest,
        &mut chunk_digest,
        &mut raw_bytes,
    );
    let mut log = started(1, id(1));
    log.extend(frame(
        id(0x11),
        2,
        id(2),
        2,
        "state_replacement_started",
        json!({
            "replacement_id": id(9),
            "reason": "compaction",
            "encoded_bytes": state_bytes.len(),
            "sha256": start_digest,
            "chunk_count": 1
        }),
    ));
    log.extend(frame(
        id(0x11),
        3,
        id(3),
        2,
        "state_replacement_chunk",
        json!({
            "replacement_id": id(9),
            "chunk_index": 0,
            "raw_bytes": raw_bytes,
            "chunk_sha256": chunk_digest,
            "base64": encoded
        }),
    ));
    log.extend(frame(
        id(0x11),
        4,
        id(4),
        2,
        "state_replacement_committed",
        json!({
            "replacement_id": id(9),
            "encoded_bytes": state_bytes.len(),
            "sha256": commit_digest,
            "chunk_count": 1
        }),
    ));
    let commit = watermark(&log, 4, id(4));
    (log, commit)
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn replacement_spool_validates_complete_transaction_and_hashes() {
    let state = serde_json::to_vec(&canonical_state(
        vec![assistant_turn("after compaction", "retained")],
        2,
    ))
    .expect("state encodes");
    let (log, commit) = replacement_log(state.clone(), |_, _, _, _| {});
    let replay = run_cold(
        &log,
        &commit,
        BoundaryIntent::Stable,
        ReplayLimits::default(),
    )
    .expect("replacement succeeds");
    let ColdReplayDisposition::Canonical(replay) = replay else {
        panic!("expected canonical state");
    };
    assert_eq!(replay.state.history.len(), 1);

    let cases = [0_u8, 1, 2];
    for case in cases {
        let (bad, commit) = replacement_log(state.clone(), |_, start, chunk, raw| match case {
            0 => *chunk = "00".repeat(32),
            1 => *start = "11".repeat(32),
            2 => *raw = (*raw).saturating_sub(1),
            _ => unreachable!(),
        });
        assert!(matches!(
            run_cold(
                &bad,
                &commit,
                BoundaryIntent::Stable,
                ReplayLimits::default()
            ),
            Err(FxProviderError::InvalidReplacement(_))
        ));
    }
}

#[test]
fn replacement_rejects_noncanonical_base64_padding_bits() {
    let state = serde_json::to_vec(&canonical_state(vec![], 2)).expect("state encodes");
    let (log, commit) = replacement_log(state, |encoded, _, _, _| {
        assert!(encoded.ends_with("=="));
        let index = encoded.len() - 3;
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let current = alphabet
            .iter()
            .position(|byte| *byte == encoded.as_bytes()[index])
            .expect("base64 alphabet");
        encoded.replace_range(
            index..=index,
            &char::from(alphabet[current ^ 1]).to_string(),
        );
    });
    assert!(matches!(
        run_cold(
            &log,
            &commit,
            BoundaryIntent::Stable,
            ReplayLimits::default()
        ),
        Err(FxProviderError::InvalidReplacement(_))
    ));
}

#[test]
fn replacement_enforces_decoded_scratch_and_nested_state_aggregate_limits() {
    let image_state = canonical_state(
        vec![json!({
            "kind": "assistant",
            "user": {
                "text": "u",
                "images": [
                    {"id": 1, "path": "/a", "media_type": "image/png"},
                    {"id": 2, "path": "/b", "media_type": "image/png"}
                ]
            },
            "assistant": "a",
            "execution": {"schema_version": 1, "tool_steps": [], "files": []}
        })],
        2,
    );
    let state = serde_json::to_vec(&image_state).expect("replacement state encodes");
    let (log, commit) = replacement_log(state.clone(), |_, _, _, _| {});

    for limits in [
        ReplayLimits {
            max_replacement_decoded_bytes: state.len() as u64 - 1,
            ..ReplayLimits::default()
        },
        ReplayLimits {
            max_scratch_bytes: state.len() as u64 - 1,
            ..ReplayLimits::default()
        },
        ReplayLimits {
            max_images: 1,
            ..ReplayLimits::default()
        },
    ] {
        assert!(run_cold(&log, &commit, BoundaryIntent::Stable, limits).is_err());
    }
}

struct CountingReader {
    inner: Cursor<Vec<u8>>,
    consumed: usize,
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.consumed += read;
        Ok(read)
    }
}

impl BufRead for CountingReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.consumed += amount;
        self.inner.consume(amount);
    }
}

#[test]
fn large_prefix_append_work_is_independent_of_prior_turn_count() {
    let prefix = started(1, id(1));
    let mut checkpoint = cold(&prefix, &watermark(&prefix, 1, id(1))).checkpoint;
    checkpoint.absolute_turn_slots = 1_000_000_000;
    checkpoint.next_seq = 900_000;
    checkpoint.through_event_log_bytes = ReplayLimits::default().max_committed_bytes / 2;
    let suffix = frame(
        id(0x11),
        checkpoint.next_seq,
        id(8),
        2,
        "history_turn_committed",
        history_payload(assistant_turn("suffix only", "constant work")),
    );
    let commit = crate::FxWatermark {
        schema_version: 1,
        session_id: SESSION.to_owned(),
        log_generation: id(0x11),
        through_seq: checkpoint.next_seq,
        through_event_id: id(8),
        through_event_log_bytes: checkpoint.through_event_log_bytes + suffix.len() as u64,
    };
    let mut reader = CountingReader {
        inner: Cursor::new(suffix.clone()),
        consumed: 0,
    };
    let SuffixDisposition::AppendNewTurns(append) = replay_suffix(
        &authority(),
        &checkpoint,
        &commit,
        &mut reader,
        BoundaryIntent::Stable,
        ReplayLimits::default(),
    )
    .expect("large-prefix continuation succeeds") else {
        panic!("expected append");
    };
    assert_eq!(reader.consumed, suffix.len());
    assert_eq!(append.new_turns.len(), 1);
    assert_eq!(append.new_turns[0].absolute_ordinal, 1_000_000_000);
}

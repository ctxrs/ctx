use super::{
    project_cursor_jsonl_record, project_cursor_jsonl_record_with_rejection,
    CursorJsonlRecordOutcome, CursorRejectionKind, MAX_CURSOR_CONTENT_BLOCKS,
};
use crate::cursor::projection::CursorTimestampState;

fn cursor_record_outcome(record: &[u8]) -> CursorJsonlRecordOutcome {
    project_cursor_jsonl_record_with_rejection(record, 0, 0, 0, record.len() as u64)
        .expect("Cursor record handling must remain record local")
}

fn assert_unsupported_shape(label: &str, record: &[u8]) {
    assert!(
        matches!(
            cursor_record_outcome(record),
            CursorJsonlRecordOutcome::Rejected(CursorRejectionKind::UnsupportedShape, _)
        ),
        "Cursor record was not rejected as an unsupported shape: {label}"
    );
}

#[test]
fn cursor_distinguishes_malformed_json_from_unsupported_shapes() {
    for (record, expected) in [
        (b"{".as_slice(), CursorRejectionKind::MalformedJson),
        (b"[]".as_slice(), CursorRejectionKind::UnsupportedShape),
    ] {
        let outcome = cursor_record_outcome(record);
        assert!(matches!(
            outcome,
            CursorJsonlRecordOutcome::Rejected(kind, _) if kind == expected
        ));
    }
}

fn assert_message_retained(label: &str, role: &str, record: &[u8]) {
    let events = project_cursor_jsonl_record(record, 0, 0, 0, record.len() as u64)
        .unwrap()
        .unwrap_or_else(|| panic!("Cursor message was rejected: {label}"));
    assert_eq!(events.len(), 1, "unexpected Cursor event count: {label}");
    assert_eq!(
        events[0].role.as_str(),
        role,
        "top-level Cursor role was not authoritative: {label}"
    );
}

fn assert_no_events(label: &str, record: &[u8]) {
    let event_count = project_cursor_jsonl_record(record, 0, 0, 0, record.len() as u64)
        .unwrap()
        .map_or(0, |events| events.len());
    assert_eq!(
        event_count, 0,
        "invalid Cursor record emitted events: {label}"
    );
}

#[test]
fn cursor_messages_without_a_nested_message_role_are_retained() {
    // Cursor agent transcripts carry the role only at the top level; the
    // message object holds content alone.
    for role in ["user", "assistant"] {
        let record = format!(
            r#"{{"role":"{role}","message":{{"content":[{{"type":"text","text":"retained"}}]}}}}"#
        );
        assert_message_retained(role, role, record.as_bytes());
    }
}

#[test]
fn cursor_messages_with_matching_legacy_nested_roles_are_retained() {
    for role in ["user", "assistant"] {
        let record = format!(
            r#"{{"role":"{role}","message":{{"role":"{role}","content":[{{"type":"text","text":"retained"}}]}}}}"#
        );
        assert_message_retained(role, role, record.as_bytes());
    }
}

#[test]
fn cursor_malformed_nested_roles_are_record_local_unsupported_shapes() {
    for (label, record) in [
        (
            "disagreeing nested role",
            br#"{"role":"user","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "explicit null nested role",
            br#"{"role":"user","message":{"role":null,"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "non-string nested role",
            br#"{"role":"assistant","message":{"role":{"value":"assistant"},"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
    ] {
        assert_unsupported_shape(label, record);
    }
}

#[test]
fn cursor_messages_require_a_valid_top_level_role() {
    for (label, record) in [
        (
            "missing top-level role",
            br#"{"message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "null top-level role",
            br#"{"role":null,"message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#
                .as_slice(),
        ),
        (
            "unsupported top-level role",
            br#"{"role":"system","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#
                .as_slice(),
        ),
    ] {
        assert_no_events(label, record);
    }
}

#[test]
fn cursor_malformed_or_ambiguous_content_is_rejected() {
    for (label, record) in [
        (
            "null message content",
            br#"{"role":"user","message":{"content":null}}"#.as_slice(),
        ),
        (
            "malformed content block",
            br#"{"role":"assistant","message":{"content":[false]}}"#.as_slice(),
        ),
        (
            "top-level and nested content",
            br#"{"role":"user","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]},"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}"#.as_slice(),
        ),
    ] {
        assert_no_events(label, record);
    }
}

fn cursor_record_with_content_blocks(block_count: usize) -> Vec<u8> {
    let mut record = br#"{"role":"assistant","message":{"content":["#.to_vec();
    for index in 0..block_count {
        if index != 0 {
            record.push(b',');
        }
        record.extend_from_slice(br#"{"type":"text","text":""}"#);
    }
    record.extend_from_slice(br#"]}}"#);
    record
}

#[test]
fn cursor_content_block_bound_is_record_local() {
    let at_bound = cursor_record_with_content_blocks(MAX_CURSOR_CONTENT_BLOCKS);
    let events = match cursor_record_outcome(&at_bound) {
        CursorJsonlRecordOutcome::Events(events) => events,
        CursorJsonlRecordOutcome::Ignored | CursorJsonlRecordOutcome::Rejected(_, _) => {
            panic!("Cursor rejected {MAX_CURSOR_CONTENT_BLOCKS} content blocks")
        }
    };
    assert_eq!(events.len(), MAX_CURSOR_CONTENT_BLOCKS);
    assert_eq!(
        events.last().map(|event| event.native_order.part_ordinal),
        Some(u32::from(u16::MAX))
    );

    let over_bound = cursor_record_with_content_blocks(MAX_CURSOR_CONTENT_BLOCKS + 1);
    assert_unsupported_shape("65,537 content blocks", &over_bound);
}

#[test]
fn cursor_conflicting_duplicate_critical_selectors_are_rejected_before_retention() {
    let baseline = br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"baseline"}]}}"#;
    assert!(
        project_cursor_jsonl_record(baseline, 0, 0, 0, baseline.len() as u64)
            .unwrap()
            .is_some()
    );

    for (label, record) in [
        (
            "message",
            br#"{"type":"message","role":"assistant","message":{"role":"user","content":[{"type":"text","text":"first"}]},"message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "role",
            br#"{"type":"message","role":"user","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "type",
            br#"{"type":"summary","type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "event",
            br#"{"event":"turn_ended","event":"summary","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "status",
            br#"{"event":"summary","status":"running","status":"completed","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "message.role",
            br#"{"type":"message","role":"assistant","message":{"role":"user","role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "content block type",
            br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"tool_result","type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
    ] {
        assert_unsupported_shape(label, record);
    }
}

#[test]
fn cursor_embedded_turn_timestamp_applies_to_following_messages() {
    let user = br#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC-7)</timestamp>\n<user_query>question</user_query>"}]}}"#;
    let assistant =
        br#"{"role":"assistant","message":{"content":[{"type":"text","text":"answer"}]}}"#;
    let mut timestamps = CursorTimestampState::default();
    let mut user_events = project_cursor_jsonl_record(user, 0, 0, 0, user.len() as u64)
        .unwrap()
        .unwrap();
    timestamps.apply(&mut user_events).unwrap();
    let mut assistant_events =
        project_cursor_jsonl_record(assistant, 1, 1, 0, assistant.len() as u64)
            .unwrap()
            .unwrap();
    timestamps.apply(&mut assistant_events).unwrap();

    let expected = "2026-07-30T18:38:00+00:00";
    assert_eq!(user_events[0].occurred_at.unwrap().to_rfc3339(), expected);
    assert_eq!(
        assistant_events[0].occurred_at.unwrap().to_rfc3339(),
        expected
    );
}

#[test]
fn cursor_turn_timestamp_resumes_across_an_append_boundary() {
    let user = br#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC-7)</timestamp>\n<user_query>question</user_query>"}]}}"#;
    let appended =
        br#"{"role":"assistant","message":{"content":[{"type":"text","text":"appended"}]}}"#;
    let mut certified = CursorTimestampState::default();
    let mut prefix = project_cursor_jsonl_record(user, 0, 0, 0, user.len() as u64)
        .unwrap()
        .unwrap();
    certified.apply(&mut prefix).unwrap();

    // The next capture resumes the certified prefix, so the appended tail
    // never re-reads the turn envelope that dated it.
    let mut resumed = CursorTimestampState::resume(certified.checkpoint().as_ref());
    let mut tail = project_cursor_jsonl_record(appended, 1, 1, 0, appended.len() as u64)
        .unwrap()
        .unwrap();
    resumed.apply(&mut tail).unwrap();

    assert_eq!(
        tail[0].occurred_at.unwrap().to_rfc3339(),
        "2026-07-30T18:38:00+00:00"
    );
    assert_eq!(certified.checkpoint(), resumed.checkpoint());
}

#[test]
fn cursor_records_without_a_turn_envelope_stay_undated() {
    // Transcripts predating Cursor's turn envelope carry no in-band time.
    let user = br#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>older format</user_query>"}]}}"#;
    let assistant =
        br#"{"role":"assistant","message":{"content":[{"type":"text","text":"answer"}]}}"#;
    let mut timestamps = CursorTimestampState::default();
    for record in [user.as_slice(), assistant.as_slice()] {
        let mut events = project_cursor_jsonl_record(record, 0, 0, 0, record.len() as u64)
            .unwrap()
            .unwrap();
        let identities: Vec<_> = events
            .iter()
            .map(|event| event.provider_event_hash)
            .collect();
        timestamps.apply(&mut events).unwrap();

        assert!(events.iter().all(|event| event.occurred_at.is_none()));
        // Undated records must keep the identity they had before the
        // turn timestamp existed.
        assert_eq!(
            events
                .iter()
                .map(|event| event.provider_event_hash)
                .collect::<Vec<_>>(),
            identities
        );
    }
    assert!(timestamps.checkpoint().is_none());
}

#[test]
fn cursor_identical_duplicate_critical_selectors_are_also_rejected() {
    for (label, record) in [
        (
            "message",
            br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]},"message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "role",
            br#"{"type":"message","role":"assistant","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "type",
            br#"{"type":"message","type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "event",
            br#"{"event":"summary","event":"summary","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "status",
            br#"{"event":"summary","status":"completed","status":"completed","message":{"content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "message.role",
            br#"{"type":"message","role":"assistant","message":{"role":"assistant","role":"assistant","content":[{"type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
        (
            "content block type",
            br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"text","type":"text","text":"MUST_NOT_EMIT"}]}}"#.as_slice(),
        ),
    ] {
        assert_unsupported_shape(label, record);
    }
}

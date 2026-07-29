use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{TimeZone, Utc};

use super::*;

const FIXTURE_EVENTS: &[u8] = include_bytes!(
    "../../../../../../../tests/fixtures/provider-history/junie/sessions/session-260607-100000-acme/events.jsonl"
);

fn materialized_fixture_events() -> (tempfile::TempDir, PathBuf) {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let path = temp.path().join("events.jsonl");
    fs::write(&path, FIXTURE_EVENTS).expect("materialized fixture");
    (temp, path)
}

fn initial_frontier() -> Frontier {
    let started = Utc
        .timestamp_millis_opt(1_783_339_200_000)
        .single()
        .expect("fixture timestamp");
    Frontier {
        offset: 0,
        next_ordinal: 0,
        next_event_index: 0,
        prefix_sha256: Sha256::digest([]).into(),
        state: RuntimeState {
            started_at_ms: started.timestamp_millis(),
            last_ts_ms: started.timestamp_millis(),
            ended_at_ms: None,
            title: Some("Junie fixture task".to_owned()),
            cwd: Some("/workspace/junie-fixture".to_owned()),
            saw_supported_event: false,
        },
        pending: None,
    }
}

#[test]
fn append_after_a_pending_turn_preserves_the_native_parser_certificate() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let turn_frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let terminal = parse_turn(&path, &turn_frontier).expect("terminal turn");
    let mut pending_frontier = turn_frontier;
    pending_frontier.pending = Some(PendingTurn {
        start_offset: terminal.start_offset,
        end_offset: terminal.end_offset,
        start_ordinal: terminal.start_ordinal,
        end_ordinal: terminal.end_ordinal,
        base_event_index: terminal.base_event_index,
        next_event_index: terminal.next_event_index,
        next_row: 1,
        row_count: u32::try_from(terminal.rows.len()).expect("bounded rows"),
        turn_sha256: terminal.turn_sha256,
        terminal: true,
        after_state: terminal.after_state.clone(),
        after_prefix_sha256: terminal.after_prefix_sha256,
    });
    let mut append = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append source");
    writeln!(
        append,
        "{}",
        serde_json::json!({
            "kind": "UserPromptEvent",
            "prompt": "appended after pending page",
        })
    )
    .expect("append prompt");
    drop(append);

    let replay = parse_turn(&path, &pending_frontier).expect("bounded pending replay");
    validate_pending_replay(&pending_frontier, &replay).expect("same pending turn");
    assert_eq!(replay.end_offset, terminal.end_offset);
    assert_eq!(replay.next_event_index, terminal.next_event_index);
}

#[test]
fn malformed_index_rows_keep_bounded_native_discovery_evidence() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let root = temp.path().join("sessions");
    let session_id = "session-junie-index-rejections";
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).expect("session directory");
    let mut index = "{malformed-index\n".repeat(24);
    index.push_str(&format!(
        "{}\n",
        serde_json::json!({"sessionId": session_id, "taskName": "valid sibling"})
    ));
    fs::write(root.join("index.jsonl"), index).expect("index fixture");
    fs::write(
        session_dir.join("events.jsonl"),
        b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"valid event\"}\n",
    )
    .expect("events fixture");

    let inventory = discover(Path::new(&root)).expect("native discovery");
    assert_eq!(inventory.sessions.len(), 1);
    assert_eq!(inventory.index_rejection_count, 24);
    assert_eq!(inventory.index_rejections.len(), MAX_JUNIE_FAILURES);
    assert!(inventory
        .index_rejections
        .iter()
        .all(|failure| failure.error == "Junie index row is not valid JSON"));
}

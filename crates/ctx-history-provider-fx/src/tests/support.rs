use std::{env, io::Cursor, path::PathBuf};

use ctx_history_core::{SourceAnchor, SourceKey};
use serde_json::{json, Value};

use crate::{
    BoundaryIntent, CanonicalReplay, ColdReplayDisposition, FxAuthority, FxAuthoritySource, FxId,
    FxWatermark, ReplayLimits, TempFileScratch,
};

pub(crate) const SESSION: &str = "native-session-1";

pub(crate) fn public_fx_fixture(relative: &str) -> PathBuf {
    let fixture_relative = PathBuf::from("tests/fixtures/provider-history/fx").join(relative);
    if fixture_relative.exists() {
        return fixture_relative;
    }
    if let Ok(runfiles) = env::var("TEST_SRCDIR") {
        let workspace = env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".to_owned());
        let fixture = PathBuf::from(runfiles)
            .join(workspace)
            .join(&fixture_relative);
        if fixture.exists() {
            return fixture;
        }
    }
    if let Ok(runfiles) = env::var("RUNFILES_DIR") {
        let workspace = env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".to_owned());
        let fixture = PathBuf::from(runfiles)
            .join(workspace)
            .join(&fixture_relative);
        if fixture.exists() {
            return fixture;
        }
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(fixture_relative);
    assert!(fixture.exists(), "missing public fx fixture: {fixture:?}");
    fixture
}

pub(crate) fn id(byte: u8) -> FxId {
    FxId([byte; 16])
}

pub(crate) fn authority() -> FxAuthority {
    FxAuthority {
        schema_version: 1,
        session_id: SESSION.to_owned(),
        authority_id: id(0xa0),
        storage_format: "event_log_v1".to_owned(),
        source: FxAuthoritySource::NativeCreate,
    }
}

pub(crate) fn watermark(log: &[u8], seq: u64, event_id: FxId) -> FxWatermark {
    FxWatermark {
        schema_version: 1,
        session_id: SESSION.to_owned(),
        log_generation: id(0x11),
        through_seq: seq,
        through_event_id: event_id,
        through_event_log_bytes: log.len() as u64,
    }
}

pub(crate) fn frame(
    generation: FxId,
    seq: u64,
    event_id: FxId,
    timestamp_ms: i64,
    kind: &str,
    payload: Value,
) -> Vec<u8> {
    let mut encoded = serde_json::to_vec(&json!({
        "schema_version": 1,
        "log_generation": generation,
        "seq": seq,
        "event_id": event_id,
        "timestamp_ms": timestamp_ms,
        "kind": kind,
        "payload": payload,
    }))
    .expect("test event encodes");
    encoded.push(b'\n');
    encoded
}

pub(crate) fn started(seq: u64, event_id: FxId) -> Vec<u8> {
    frame(
        id(0x11),
        seq,
        event_id,
        1,
        "session_started",
        json!({
            "id": SESSION,
            "created_at_ms": 1,
            "origin_workspace_root": "/workspace/root",
            "workspace_root": "/workspace/root",
            "conversation_language": "en",
            "preferences": {
                "model": "openai/gpt-5",
                "effort": "high",
                "fast_mode": false,
                "provider": "gateway"
            }
        }),
    )
}

pub(crate) fn assistant_turn(user: &str, assistant: &str) -> Value {
    json!({
        "kind": "assistant",
        "user": {"text": user, "images": []},
        "assistant": assistant,
        "execution": {"schema_version": 1, "tool_steps": [], "files": []}
    })
}

pub(crate) fn assistant_turn_with_work(user: &str, assistant: &str, work_id: &str) -> Value {
    json!({
        "kind": "assistant",
        "user": {"text": user, "images": [], "work_id": work_id},
        "assistant": assistant,
        "execution": {"schema_version": 1, "tool_steps": [], "files": []}
    })
}

pub(crate) fn history_payload(turn: Value) -> Value {
    json!({
        "conversation_language": "en",
        "total_input_tokens": 10,
        "total_output_tokens": 20,
        "turn": turn
    })
}

pub(crate) fn history_payload_with_work(turn: Value, work_id: &str) -> Value {
    json!({
        "conversation_language": "en",
        "total_input_tokens": 10,
        "total_output_tokens": 20,
        "turn": turn,
        "work_id": work_id
    })
}

pub(crate) fn canonical_state(history: Vec<Value>, updated_at_ms: i64) -> Value {
    json!({
        "id": SESSION,
        "origin_workspace_root": "/workspace/root",
        "workspace_root": "/workspace/root",
        "created_at_ms": 1,
        "updated_at_ms": updated_at_ms,
        "conversation_language": "en",
        "preferences": {
            "model": "openai/gpt-5",
            "effort": "high",
            "fast_mode": false,
            "provider": "gateway"
        },
        "history": history,
        "total_input_tokens": 10,
        "total_output_tokens": 20,
        "context_history_start": 0,
        "permission_state": {"schema_version": 2, "next_generation": 1, "rules": []}
    })
}

pub(crate) fn cold(log: &[u8], watermark: &FxWatermark) -> CanonicalReplay {
    let mut cursor = Cursor::new(log);
    match crate::replay_committed(
        &authority(),
        watermark,
        &mut cursor,
        BoundaryIntent::Stable,
        &TempFileScratch,
        ReplayLimits::default(),
    )
    .expect("cold replay succeeds")
    {
        ColdReplayDisposition::Canonical(replay) => *replay,
        ColdReplayDisposition::UnsafePending(_) => panic!("unexpected pending intent"),
    }
}

pub(crate) fn source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        "fx",
        "fx_session",
        "native_history",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .expect("test source derives")
}

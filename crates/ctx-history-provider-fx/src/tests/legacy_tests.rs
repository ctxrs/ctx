use std::fs;

use ctx_history_core::CoreRecord;
use serde_json::{json, Value};

use crate::{
    project_canonical_state, replay_legacy_snapshot, select_session_protocol, FxProviderError,
    LegacyDefaults, LegacySnapshotVersion, ProjectionBinding, ReplayLimits,
    SelectedSessionProtocol, MAX_LEGACY_SNAPSHOT_BYTES,
};

use super::support::{
    assistant_turn, authority, canonical_state, public_fx_fixture, source, SESSION,
};

fn defaults() -> LegacyDefaults {
    LegacyDefaults {
        source_root: "/source/lineage/root".to_owned(),
        preferences: serde_json::from_value(json!({
            "provider": "gateway",
            "model": "openai/gpt-5",
            "effort": "high",
            "fast_mode": false
        }))
        .expect("legacy defaults"),
    }
}

fn snapshot(schema_version: u64, history: Vec<Value>) -> Value {
    json!({
        "schema_version": schema_version,
        "id": SESSION,
        "created_at_ms": 1,
        "updated_at_ms": 2,
        "conversation_language": "en",
        "history_len": history.len(),
        "history": history,
        "total_input_tokens": 10,
        "total_output_tokens": 20
    })
}

fn project(state: &crate::CanonicalState) -> Vec<CoreRecord> {
    let source = source(9);
    project_canonical_state(
        ProjectionBinding {
            source: &source,
            native_session_id: SESSION,
        },
        state,
    )
    .expect("legacy projection")
}

#[test]
fn public_schema_v1_and_v2_snapshots_replay_with_stable_session_identity() {
    for (relative, version) in [
        (
            "upstream-v0.3.73-test-source/schema-v1/.fx/sessions/legacy-v1/session.json",
            LegacySnapshotVersion::V1,
        ),
        (
            "upstream-v0.3.73-test-source/schema-v2/.fx/sessions/legacy-v2/session.json",
            LegacySnapshotVersion::V2,
        ),
    ] {
        let snapshot = fs::read(public_fx_fixture(relative)).unwrap();
        let reduction = replay_legacy_snapshot(&snapshot, &defaults(), ReplayLimits::default())
            .expect("public legacy snapshot replays");
        assert_eq!(reduction.version, version);
        assert!(!reduction.state.id.is_empty());
    }
}

#[test]
fn markerless_v1_accepts_bounded_unknown_legacy_keys_but_drops_them() {
    let legacy = snapshot(
        1,
        vec![json!({
            "kind": "assistant",
            "user": {
                "text": "legacy user",
                "images": [{
                    "path": "/tmp/image.png",
                    "media_type": "image/png",
                    "legacy_image_extra": {"bounded": true}
                }],
                "legacy_user_extra": [1, 2, 3]
            },
            "assistant": "legacy assistant",
            "legacy_turn_extra": "ignored"
        })],
    );
    let mut root = legacy.as_object().expect("root").clone();
    root.insert("legacy_root_extra".to_owned(), json!({"safe": "skip"}));
    root.insert("total_web_search_requests".to_owned(), json!({"old": true}));
    let encoded = serde_json::to_vec(&Value::Object(root)).expect("legacy encodes");
    let reduction = replay_legacy_snapshot(&encoded, &defaults(), ReplayLimits::default())
        .expect("legacy v1 reduces");
    assert_eq!(reduction.version, LegacySnapshotVersion::V1);
    assert_eq!(reduction.state.history.len(), 1);
    let normalized = reduction.state.history[0]
        .structured_value()
        .expect("normalized legacy turn");
    assert!(normalized.get("legacy_turn_extra").is_none());
    assert!(normalized["user"].get("legacy_user_extra").is_none());
    assert!(normalized["user"]["images"][0]
        .get("legacy_image_extra")
        .is_none());
}

#[test]
fn markerless_v2_preserves_known_stable_image_and_background_ids() {
    let legacy = snapshot(
        2,
        vec![json!({
            "kind": "background_command",
            "user": {
                "text": "run server",
                "images": [{
                    "id": 44,
                    "path": "/tmp/image.png",
                    "media_type": "image/png"
                }]
            },
            "log_path": "/tmp/server.log",
            "expect_url": true,
            "url": "http://127.0.0.1:8080",
            "background_record_id": "11111111111111111111111111111111"
        })],
    );
    let reduction = replay_legacy_snapshot(
        &serde_json::to_vec(&legacy).expect("legacy encodes"),
        &defaults(),
        ReplayLimits::default(),
    )
    .expect("legacy v2 reduces");
    assert_eq!(reduction.version, LegacySnapshotVersion::V2);
    let normalized = reduction.state.history[0]
        .structured_value()
        .expect("normalized legacy turn");
    assert_eq!(normalized["user"]["images"][0]["id"], 44);
    assert_eq!(
        normalized["background_record_id"],
        "11111111111111111111111111111111"
    );
}

#[test]
fn markerless_schema_three_and_malformed_snapshots_are_rejected() {
    for value in [snapshot(3, vec![]), json!({"schema_version": 1})] {
        assert!(replay_legacy_snapshot(
            &serde_json::to_vec(&value).expect("test JSON"),
            &defaults(),
            ReplayLimits::default(),
        )
        .is_err());
    }
}

#[test]
fn authority_marker_prevents_any_legacy_fallback_or_inspection() {
    let marker = serde_json::to_vec(&authority()).expect("authority encodes");
    let selected = select_session_protocol(
        Some(&marker),
        Some(b"not JSON and intentionally malformed"),
        &defaults(),
        ReplayLimits::default(),
    )
    .expect("authority wins");
    assert!(matches!(selected, SelectedSessionProtocol::V3(_)));
}

#[test]
fn pre_and_post_migration_projection_use_the_same_lineage_session_identity() {
    let legacy = snapshot(
        2,
        vec![json!({
            "kind": "assistant",
            "user": {"text": "same logical user", "images": []},
            "assistant": "same logical assistant"
        })],
    );
    let before = replay_legacy_snapshot(
        &serde_json::to_vec(&legacy).expect("legacy encodes"),
        &defaults(),
        ReplayLimits::default(),
    )
    .expect("legacy reduces");
    let after: crate::CanonicalState = serde_json::from_value(canonical_state(
        vec![assistant_turn(
            "same logical user",
            "same logical assistant",
        )],
        2,
    ))
    .expect("v3 state");

    let before_records = project(&before.state);
    let after_records = project(&after);
    assert_eq!(before_records.len(), 2);
    assert_eq!(before_records.len(), after_records.len());
    for (before_record, after_record) in before_records.iter().zip(&after_records) {
        assert_eq!(before_record.session_id, after_record.session_id);
        assert_eq!(before_record.event_id, after_record.event_id);
    }
}

#[test]
fn legacy_snapshot_limit_matches_whole_record_contract_at_boundary() {
    let mut exact = serde_json::to_vec(&snapshot(1, vec![])).expect("legacy encodes");
    exact.resize(MAX_LEGACY_SNAPSHOT_BYTES as usize, b' ');
    replay_legacy_snapshot(&exact, &defaults(), ReplayLimits::default())
        .expect("exact 16 MiB legacy snapshot is admitted");

    exact.push(b' ');
    assert!(matches!(
        replay_legacy_snapshot(&exact, &defaults(), ReplayLimits::default()),
        Err(FxProviderError::LimitExceeded {
            resource: "legacy snapshot bytes",
            actual,
            maximum: MAX_LEGACY_SNAPSHOT_BYTES,
        }) if actual == MAX_LEGACY_SNAPSHOT_BYTES + 1
    ));
}

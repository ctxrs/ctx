use std::fs;
use std::path::PathBuf;

use ctx_core::models::{
    SessionSnapshot, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot,
    WorkspaceActiveSnapshotStreamMessage,
};
use serde::de::DeserializeOwned;
use serde::Serialize;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/workspace_payloads")
}

fn load_payload<T>(name: &str) -> T
where
    T: DeserializeOwned + Serialize,
{
    let path = corpus_dir().join(name);
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    });
    let value = serde_json::from_str::<T>(&raw).unwrap_or_else(|err| {
        panic!("failed to parse {}: {err}", path.display());
    });
    let encoded = serde_json::to_string(&value).expect("payload should serialize");
    serde_json::from_str(&encoded).expect("roundtripped payload should parse")
}

#[test]
fn workspace_active_snapshot_corpus_roundtrips() {
    let snapshot: WorkspaceActiveSnapshot = load_payload("workspace-active-snapshot.json");
    assert_eq!(snapshot.snapshot_rev, 42);
    assert_eq!(snapshot.active.total_count, 1);
    assert_eq!(snapshot.active.tasks.len(), 1);
    assert_eq!(
        snapshot.active.tasks[0]
            .primary_session
            .session
            .id
            .0
            .to_string(),
        "44444444-4444-4444-8444-444444444444"
    );
}

#[test]
fn workspace_active_heads_corpus_roundtrips() {
    let batch: WorkspaceActiveHeadBatch = load_payload("workspace-active-heads.json");
    assert_eq!(
        batch.workspace_id.0.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(batch.snapshot_rev, 42);
    assert_eq!(batch.heads.len(), 1);
    assert_eq!(
        batch.heads[0].session.id.0.to_string(),
        "44444444-4444-4444-8444-444444444444"
    );
}

#[test]
fn session_snapshot_corpus_roundtrips() {
    let snapshot: SessionSnapshot = load_payload("session-snapshot.json");
    assert_eq!(
        snapshot.summary.session.id.0.to_string(),
        "44444444-4444-4444-8444-444444444444"
    );
    assert!(snapshot.head.is_some());
    assert!(snapshot.state.is_some());
}

#[test]
fn workspace_stream_snapshot_corpus_roundtrips() {
    let msg: WorkspaceActiveSnapshotStreamMessage = load_payload("workspace-stream-snapshot.json");
    match msg {
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            rev,
            active_snapshot,
            active_heads,
        } => {
            assert_eq!(rev, 77);
            assert_eq!(active_snapshot.snapshot_rev, 42);
            assert_eq!(active_heads.expect("expected active heads").heads.len(), 1);
        }
        other => panic!("expected snapshot message, got {other:?}"),
    }
}

#[test]
fn workspace_stream_heads_batch_corpus_roundtrips() {
    let msg: WorkspaceActiveSnapshotStreamMessage =
        load_payload("workspace-stream-heads-batch.json");
    match msg {
        WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
            rev,
            snapshot_rev,
            deltas,
            ..
        } => {
            assert_eq!(rev, 79);
            assert_eq!(snapshot_rev, 42);
            assert_eq!(deltas.len(), 1);
            assert!(deltas[0].message.is_some());
        }
        other => panic!("expected heads batch message, got {other:?}"),
    }
}

#[test]
fn workspace_stream_session_gap_corpus_roundtrips() {
    let msg: WorkspaceActiveSnapshotStreamMessage =
        load_payload("workspace-stream-session-gap.json");
    match msg {
        WorkspaceActiveSnapshotStreamMessage::Event { rev, event, .. } => {
            assert_eq!(rev, 78);
            match *event {
                ctx_core::models::WorkspaceActiveSnapshotEvent::SessionGap {
                    workspace_id,
                    snapshot_rev,
                    session_id,
                    after_seq,
                    reason,
                    seed_follows,
                } => {
                    assert_eq!(
                        workspace_id.0.to_string(),
                        "11111111-1111-4111-8111-111111111111"
                    );
                    assert_eq!(snapshot_rev, 42);
                    assert_eq!(
                        session_id.0.to_string(),
                        "44444444-4444-4444-8444-444444444444"
                    );
                    assert_eq!(after_seq, 99);
                    assert_eq!(reason.as_deref(), Some("lagged"));
                    assert!(!seed_follows);
                }
                other => panic!("expected session_gap event, got {other:?}"),
            }
        }
        other => panic!("expected event message, got {other:?}"),
    }
}

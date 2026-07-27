use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, Artifact, ArtifactKind, ContentRef, EntityTimestamps, Event, EventRole, EventType,
    Fidelity, SessionHistoryArchive, SyncMetadata, SyncState, Visibility,
};
use uuid::Uuid;

use crate::archive::{validate_archive_artifact_record_blob, validate_archive_version};
use crate::object_store::{object_relative_path, sha256_hex};
use crate::{Store, StoreError};

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-store-archive-validation-")
        .tempdir_in(root)
        .unwrap()
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn artifact(id: Uuid, blob_hash: String, byte_size: u64) -> Artifact {
    Artifact {
        id,
        kind: ArtifactKind::Markdown,
        blob_path: object_relative_path(&blob_hash),
        blob_hash,
        byte_size,
        media_type: Some("text/markdown".into()),
        preview_text: Some("synthetic local preview blob".into()),
        timestamps: EntityTimestamps {
            created_at: fixed_time(),
            updated_at: fixed_time(),
        },
        source_id: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: serde_json::json!({}),
        },
    }
}

fn event(seq: u64, event_type: EventType, payload: serde_json::Value) -> Event {
    Event {
        id: new_id(),
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type,
        role: Some(EventRole::Tool),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload,
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: serde_json::json!({}),
        },
    }
}

fn write_blob(blob_dir: &Path, blob_hash: &str, content: &[u8]) {
    let path = blob_dir.join(&blob_hash[..2]).join(blob_hash);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn assert_artifact_error(error: StoreError, matches_expected: impl FnOnce(&StoreError) -> bool) {
    assert!(
        matches_expected(&error),
        "unexpected archive artifact validation error: {error:?}"
    );
}

#[test]
fn archive_blob_validation_fails_closed_when_blob_is_missing() {
    let temp = tempdir();
    let content = b"missing synthetic blob";
    let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);

    let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
    assert_artifact_error(
        error,
        |error| matches!(error, StoreError::ArchiveArtifactMissingContent { id } if *id == artifact.id),
    );
}

#[test]
fn archive_blob_validation_fails_closed_when_hash_differs() {
    let temp = tempdir();
    let stored_content = b"stored bytes";
    let expected_content = b"expected bytes";
    let artifact = artifact(
        new_id(),
        sha256_hex(expected_content),
        stored_content.len() as u64,
    );
    write_blob(temp.path(), &artifact.blob_hash, stored_content);

    let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
    assert_artifact_error(
        error,
        |error| matches!(error, StoreError::ArchiveArtifactHashMismatch { id } if *id == artifact.id),
    );
}

#[test]
fn archive_blob_validation_fails_closed_when_byte_size_differs() {
    let temp = tempdir();
    let content = b"size checked bytes";
    let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64 + 1);
    write_blob(temp.path(), &artifact.blob_hash, content);

    let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
    assert_artifact_error(
        error,
        |error| matches!(error, StoreError::ArchiveArtifactSizeMismatch { id } if *id == artifact.id),
    );
}

#[test]
fn archive_blob_validation_fails_closed_when_blob_path_mismatches_hash() {
    let temp = tempdir();
    let content = b"path checked bytes";
    let mut artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);
    artifact.blob_path = "objects/ff/not-the-recorded-hash".into();
    write_blob(temp.path(), &artifact.blob_hash, content);

    let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
    assert_artifact_error(
        error,
        |error| matches!(error, StoreError::ArchiveArtifactPathMismatch { id } if *id == artifact.id),
    );
}

#[test]
fn archive_blob_validation_fails_closed_when_blob_is_not_regular_file() {
    let temp = tempdir();
    let content = b"directory at blob path";
    let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);
    let path = temp
        .path()
        .join(&artifact.blob_hash[..2])
        .join(&artifact.blob_hash);
    fs::create_dir_all(&path).unwrap();

    let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
    assert_artifact_error(
        error,
        |error| matches!(error, StoreError::ArchiveArtifactNonRegularFile { id, .. } if *id == artifact.id),
    );
}

#[test]
fn archive_version_validation_rejects_future_version() {
    let archive = SessionHistoryArchive {
        schema_version: 3,
        version: 3,
        ..SessionHistoryArchive::default()
    };

    let error = validate_archive_version(&archive).unwrap_err();
    assert!(matches!(
        error,
        StoreError::UnsupportedArchiveVersion(version) if version == 3
    ));
}

#[test]
fn every_event_write_boundary_keeps_result_bodies_source_backed() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("ctx.db")).unwrap();
    let direct_canary = "DIRECT-RESULT-BODY-CANARY-79ef";
    let archive_canary = "ARCHIVE-RESULT-BODY-CANARY-cab4";
    let message_canary = "NON-RESULT-BODY-MUST-SURVIVE-3e11";
    let direct_content_ref = ContentRef::from_bytes(direct_canary.as_bytes()).unwrap();
    let direct = event(
        1,
        EventType::CommandOutput,
        serde_json::json!({
            "tool": "exec_command",
            "call_id": "call-direct",
            "exit_code": 0,
            "result_outcome": "success",
            "result_content_ref": direct_content_ref,
            "output": direct_canary,
            "output_preview": direct_canary,
        }),
    );
    store.upsert_event(&direct).unwrap();

    let archived = event(
        2,
        EventType::ToolOutput,
        serde_json::json!({
            "provider": "codex",
            "provider_session_id": "archive-session",
            "provider_event_index": 2,
            "provider_event_hash": "archive-event-hash",
            "body": {
                "tool": "shell",
                "call_id": "call-archive",
                "exit_code": 1,
                "result_outcome": "failure",
                "output": archive_canary,
                "output_preview": archive_canary,
            }
        }),
    );
    let message = event(
        3,
        EventType::Message,
        serde_json::json!({"text": message_canary}),
    );
    let archive = SessionHistoryArchive {
        schema_version: 2,
        version: 2,
        events: vec![archived.clone(), message.clone()],
        ..SessionHistoryArchive::default()
    };
    // This is the same Store transaction used by nested spool archives.
    store.import_archive(&archive, false).unwrap();

    assert_eq!(
        store.get_event(direct.id).unwrap().payload,
        serde_json::json!({
            "tool": "exec_command",
            "call_id": "call-direct",
            "exit_code": 0,
            "result_outcome": "success",
            "result_content_ref": direct_content_ref,
        })
    );
    assert_eq!(
        store.get_event(archived.id).unwrap().payload,
        serde_json::json!({
            "provider": "codex",
            "provider_session_id": "archive-session",
            "provider_event_index": 2,
            "provider_event_hash": "archive-event-hash",
            "body": {
                "tool": "shell",
                "call_id": "call-archive",
                "exit_code": 1,
                "result_outcome": "failure",
            }
        })
    );
    assert_eq!(
        store.get_event(message.id).unwrap().payload,
        message.payload
    );
    assert!(store
        .search_event_hits(direct_canary, 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits(archive_canary, 10)
        .unwrap()
        .is_empty());

    let exported = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!exported.contains(direct_canary));
    assert!(!exported.contains(archive_canary));
    assert!(exported.contains(message_canary));
}

#[test]
fn archive_export_and_import_strip_local_verified_content_locators() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("ctx.db")).unwrap();
    let mut local = event(
        1,
        EventType::Message,
        serde_json::json!({"text": "preview"}),
    );
    local.sync.metadata = serde_json::json!({
        "verified_content_locators_v1": {"local_path_capability": "/private/source"},
        "preserved": true
    });
    store.upsert_event(&local).unwrap();

    let exported = store.export_archive().unwrap();
    let exported_event = exported
        .events
        .iter()
        .find(|event| event.id == local.id)
        .unwrap();
    assert_eq!(
        exported_event.sync.metadata,
        serde_json::json!({"preserved": true})
    );
    assert!(!serde_json::to_string(&exported)
        .unwrap()
        .contains("/private/source"));

    let mut crafted = event(
        2,
        EventType::Message,
        serde_json::json!({"text": "crafted"}),
    );
    crafted.sync.metadata = serde_json::json!({
        "verified_content_locators_v1": {"local_path_capability": "/attacker/source"},
        "preserved": true
    });
    let archive = SessionHistoryArchive {
        schema_version: 2,
        version: 2,
        events: vec![crafted.clone()],
        ..SessionHistoryArchive::default()
    };
    store.import_archive(&archive, false).unwrap();
    assert_eq!(
        store.get_event(crafted.id).unwrap().sync.metadata,
        serde_json::json!({"preserved": true})
    );
}

#[test]
fn result_payload_blobs_are_rejected_at_direct_and_archive_boundaries() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("ctx.db")).unwrap();
    let mut direct = event(
        1,
        EventType::CommandOutput,
        serde_json::json!({"output": "blob-backed result"}),
    );
    direct.payload_blob_id = Some(new_id());
    assert!(matches!(
        store.upsert_event(&direct).unwrap_err(),
        StoreError::ResultPayloadBlobUnsupported { id } if id == direct.id
    ));

    let mut archived = event(
        2,
        EventType::ToolOutput,
        serde_json::json!({"output": "archived blob-backed result"}),
    );
    archived.payload_blob_id = Some(new_id());
    let archive = SessionHistoryArchive {
        schema_version: 2,
        version: 2,
        events: vec![archived.clone()],
        ..SessionHistoryArchive::default()
    };
    assert!(matches!(
        store.import_archive(&archive, false).unwrap_err(),
        StoreError::ResultPayloadBlobUnsupported { id } if id == archived.id
    ));
    assert!(matches!(
        store.get_event(archived.id),
        Err(StoreError::NotFound(_))
    ));

    let mut replay = event(
        3,
        EventType::CommandOutput,
        serde_json::json!({"output": "first valid result"}),
    );
    replay.dedupe_key = Some("result-replay".to_owned());
    store.upsert_event(&replay).unwrap();
    replay.payload_blob_id = Some(new_id());
    assert!(matches!(
        store.upsert_event(&replay).unwrap_err(),
        StoreError::ResultPayloadBlobUnsupported { id } if id == replay.id
    ));
}

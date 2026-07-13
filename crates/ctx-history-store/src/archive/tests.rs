use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, Artifact, ArtifactKind, EntityTimestamps, Event, EventRole, EventType, Fidelity,
    HistoryRecord, SessionHistoryArchive, SyncMetadata, SyncState, Visibility,
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

fn record(id: Uuid, marker: &str) -> HistoryRecord {
    let mut record = HistoryRecord::new(
        format!("Archive {marker}"),
        format!("archive projection {marker}"),
        Vec::new(),
        "task",
        Some("/workspace/archive".into()),
    );
    record.id = id;
    record.created_at = fixed_time();
    record.updated_at = fixed_time();
    record
}

fn event(id: Uuid, record_id: Uuid, marker: &str) -> Event {
    Event {
        id,
        seq: 1,
        history_record_id: Some(record_id),
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({ "text": format!("archive event {marker}") }),
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    }
}

fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

fn archive_with_searchable_entities(
    record: HistoryRecord,
    event: Event,
    artifact: Artifact,
) -> SessionHistoryArchive {
    SessionHistoryArchive {
        records: vec![record],
        events: vec![event],
        artifact_records: vec![artifact],
        ..SessionHistoryArchive::default()
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
fn archive_overwrite_rebuilds_all_search_projections_without_stale_rows() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record_id = new_id();
    let event_id = new_id();
    let artifact_id = new_id();
    let blob = b"archive searchable artifact";
    let blob_hash = sha256_hex(blob);
    write_blob(&temp.path().join("objects"), &blob_hash, blob);

    let mut old_artifact = artifact(artifact_id, blob_hash.clone(), blob.len() as u64);
    old_artifact.preview_text = Some("archive artifact oldprojectionterm".into());
    store
        .import_archive(
            &archive_with_searchable_entities(
                record(record_id, "oldprojectionterm"),
                event(event_id, record_id, "oldprojectionterm"),
                old_artifact,
            ),
            false,
        )
        .unwrap();

    store
        .with_write_transaction(|| {
            store.conn.execute(
                r#"
                INSERT INTO ctx_history_search
                (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
                VALUES ('orphan-record', 'orphanprojectionterm', '', '', '', '', '')
                "#,
                [],
            )?;
            store.conn.execute(
                r#"
                INSERT INTO event_search
                (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                VALUES ('orphan-event', NULL, NULL, 'user', 'orphanprojectionterm', 'message')
                "#,
                [],
            )?;
            store.conn.execute(
                r#"
                INSERT INTO artifact_search (artifact_id, history_record_id, preview_text)
                VALUES ('orphan-artifact', NULL, 'orphanprojectionterm')
                "#,
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let mut new_artifact = artifact(artifact_id, blob_hash, blob.len() as u64);
    new_artifact.preview_text = Some("archive artifact newprojectionterm".into());
    store
        .import_archive(
            &archive_with_searchable_entities(
                record(record_id, "newprojectionterm"),
                event(event_id, record_id, "newprojectionterm"),
                new_artifact,
            ),
            true,
        )
        .unwrap();

    assert!(store
        .search_records("oldprojectionterm", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .search_records("newprojectionterm", 10)
            .unwrap()
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        vec![record_id]
    );
    assert!(store
        .search_event_hits("oldprojectionterm", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .search_event_hits("newprojectionterm", 10)
            .unwrap()
            .into_iter()
            .map(|hit| hit.event_id)
            .collect::<Vec<_>>(),
        vec![event_id]
    );
    for table in ["ctx_history_search", "event_search", "artifact_search"] {
        let orphan_count: i64 = store
            .conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {table} MATCH ?1"),
                ["orphanprojectionterm"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 0, "stale projection survived in {table}");
    }
    assert_eq!(
        store.list_artifacts().unwrap()[0].preview_text.as_deref(),
        Some("archive artifact newprojectionterm")
    );
    let artifact_projection_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM artifact_search", [], |row| row.get(0))
        .unwrap();
    assert_eq!(artifact_projection_count, 0);
}

#[test]
fn archive_import_failure_rolls_back_rows_and_preserves_object_store() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record_id = new_id();
    let blob = b"validated archive rollback blob";
    let blob_hash = sha256_hex(blob);
    let blob_dir = temp.path().join("objects");
    write_blob(&blob_dir, &blob_hash, blob);
    let blob_path = blob_dir.join(&blob_hash[..2]).join(&blob_hash);

    let mut invalid_event = event(new_id(), record_id, "rollbackprojectionterm");
    invalid_event.session_id = Some(new_id());
    let error = store
        .import_archive(
            &archive_with_searchable_entities(
                record(record_id, "rollbackprojectionterm"),
                invalid_event,
                artifact(new_id(), blob_hash, blob.len() as u64),
            ),
            false,
        )
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Sql(_)),
        "unexpected error: {error:?}"
    );

    assert!(store.list_records(usize::MAX).unwrap().is_empty());
    assert!(store.list_events().unwrap().is_empty());
    assert!(store.list_artifacts().unwrap().is_empty());
    assert_eq!(fs::read(&blob_path).unwrap(), blob);
    let object_file_count = fs::read_dir(blob_path.parent().unwrap())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .count();
    assert_eq!(object_file_count, 1);
}

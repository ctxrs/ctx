use std::{
    cell::Cell,
    fs,
    path::Path,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, Artifact, ArtifactKind, EntityTimestamps, Event, EventRole, EventType, Fidelity,
    HistoryRecord, SessionHistoryArchive, SyncMetadata, SyncState, Visibility,
};
use rusqlite::hooks::{Action, AuthAction, Authorization, TransactionOperation};
use rusqlite::params;
use uuid::Uuid;

use crate::archive::import::{import_archive_stage_range_tx, ArchiveImportStage};
use crate::archive::{
    archive_import_batch_end, archive_import_fingerprint, ensure_archive_import_progress_table,
    estimate_archive_write_bytes, reject_import_stage_conflicts, set_archive_import_progress,
    validate_archive_artifact_record_blob, validate_archive_artifact_record_blobs,
    validate_archive_version, ArchiveWriteTransaction, ARCHIVE_IMPORT_BATCH_ROWS,
};
use crate::object_store::{object_relative_path, sha256_hex};
use crate::work_control::install_test_disk_space_probe;
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
fn archive_blob_validation_streams_large_content() {
    let temp = tempdir();
    let content = (0..(12 * 1024 * 1024))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let artifact = artifact(new_id(), sha256_hex(&content), content.len() as u64);
    write_blob(temp.path(), &artifact.blob_hash, &content);

    let validated = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap();

    assert_eq!(validated.id, artifact.id);
}

#[cfg(target_os = "linux")]
#[test]
fn archive_blob_validation_does_not_retain_one_descriptor_per_artifact() {
    let temp = tempdir();
    let mut artifacts = Vec::new();
    for index in 0..256_u32 {
        let content = index.to_le_bytes();
        let blob_hash = sha256_hex(&content);
        write_blob(temp.path(), &blob_hash, &content);
        artifacts.push(artifact(new_id(), blob_hash, content.len() as u64));
    }
    let archive = SessionHistoryArchive {
        artifact_records: artifacts,
        ..SessionHistoryArchive::default()
    };
    let before = fs::read_dir("/proc/self/fd").unwrap().count();

    let validated = validate_archive_artifact_record_blobs(temp.path(), &archive).unwrap();
    let after = fs::read_dir("/proc/self/fd").unwrap().count();

    assert_eq!(validated.blobs.len(), 256);
    assert!(
        after <= before + 2,
        "blob validation retained {} descriptors for 256 artifacts",
        after.saturating_sub(before)
    );
}

#[test]
fn archive_blob_revalidation_rejects_replacement_before_commit() {
    let temp = tempdir();
    let content = b"content validated before writer admission";
    let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);
    write_blob(temp.path(), &artifact.blob_hash, content);
    let archive = SessionHistoryArchive {
        artifact_records: vec![artifact.clone()],
        ..SessionHistoryArchive::default()
    };
    let validated = validate_archive_artifact_record_blobs(temp.path(), &archive).unwrap();
    let blob_path = temp
        .path()
        .join(&artifact.blob_hash[..2])
        .join(&artifact.blob_hash);
    fs::remove_file(&blob_path).unwrap();
    fs::write(&blob_path, vec![b'x'; content.len()]).unwrap();

    let error = validated.revalidate_paths().unwrap_err();

    assert!(matches!(
        error,
        StoreError::ArchiveArtifactChangedDuringValidation { id } if id == artifact.id
    ));
}

#[test]
fn archive_import_revalidates_blob_identity_at_commit_boundary() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let content = b"validated archive content";
    let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);
    let blob_path = temp
        .path()
        .join("objects")
        .join(&artifact.blob_hash[..2])
        .join(&artifact.blob_hash);
    write_blob(&temp.path().join("objects"), &artifact.blob_hash, content);
    let archive = SessionHistoryArchive {
        artifact_records: vec![artifact.clone()],
        ..SessionHistoryArchive::default()
    };
    let changed = Arc::new(AtomicBool::new(false));
    let hook_changed = Arc::clone(&changed);
    store.conn.update_hook(Some(
        move |action: Action, _database: &str, table: &str, _rowid: i64| {
            if action == Action::SQLITE_INSERT
                && table == "artifacts"
                && fs::write(&blob_path, b"replacement with a different length").is_ok()
            {
                hook_changed.store(true, Ordering::SeqCst);
            }
        },
    ));

    let error = store.import_archive(&archive, false).unwrap_err();
    store.conn.update_hook(None::<fn(Action, &str, &str, i64)>);

    assert!(changed.load(Ordering::SeqCst));
    assert!(matches!(
        error,
        StoreError::ArchiveArtifactChangedDuringValidation { id } if id == artifact.id
    ));
    let imported: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM artifacts WHERE id = ?1",
            [artifact.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(imported, 0);
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
                record(record_id, "oldprojectionterm 旧投影検索語"),
                event(event_id, record_id, "oldprojectionterm 旧投影検索語"),
                old_artifact,
            ),
            false,
        )
        .unwrap();
    while store.run_event_search_maintenance_slice().unwrap() {}
    assert!(!store.search_projection_maintenance_pending().unwrap());

    store
        .with_write_transaction(|| {
            for index in 0..70 {
                store.conn.execute(
                    r#"
                    INSERT INTO ctx_history_search
                    (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
                    VALUES (?1, 'orphanprojectionterm', '', '', '', '', '')
                    "#,
                    [format!("orphan-record-{index}")],
                )?;
            }
            store.conn.execute(
                "INSERT INTO ctx_history_search_scriptgram (record_id, token_text) VALUES ('orphan-record', '孤立検索語')",
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
                INSERT INTO event_search_scriptgram
                (event_id, history_record_id, session_id, role, token_text, rank_bucket)
                VALUES ('orphan-event', NULL, NULL, 'user', '孤立検索語', 'message')
                "#,
                [],
            )?;
            store.conn.execute(
                r#"
                UPDATE event_search_lookup
                SET preview_text = 'orphanprojectionterm'
                WHERE event_id = ?1
                "#,
                params![event_id.to_string()],
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
                record(record_id, "newprojectionterm 新投影検索語"),
                event(event_id, record_id, "newprojectionterm 新投影検索語"),
                new_artifact,
            ),
            true,
        )
        .unwrap();
    while store.run_event_search_maintenance_slice().unwrap() {}
    assert!(!store.search_projection_maintenance_pending().unwrap());
    assert_eq!(
        store.cached_event_embedding_document_count().unwrap(),
        Some(1)
    );

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
    assert!(store.search_records("旧投影検索語", 10).unwrap().is_empty());
    assert_eq!(
        store
            .search_records("新投影検索語", 10)
            .unwrap()
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        vec![record_id]
    );
    assert!(store
        .search_event_hits("旧投影検索語", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .search_event_hits("新投影検索語", 10)
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
    for table in ["ctx_history_search_scriptgram", "event_search_scriptgram"] {
        let orphan_count: i64 = store
            .conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {table} MATCH ?1"),
                ["孤立検索語"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 0, "stale projection survived in {table}");
    }
    let orphan_lookup_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM event_search_lookup WHERE preview_text = ?1",
            params!["orphanprojectionterm"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_lookup_count, 0);
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

#[test]
fn post_commit_projection_failure_reports_durable_archive_outcome() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record_id = new_id();
    let mut archive = archive_with_searchable_entities(
        record(record_id, "durablemaintenance"),
        event(new_id(), record_id, "durablemaintenance"),
        artifact(new_id(), sha256_hex(b""), 0),
    );
    archive.artifact_records.clear();
    let outcome = store
        .import_archive_after_commit(&archive, false, |conn| {
            conn.authorizer(Some(
                |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                    AuthAction::Transaction {
                        operation: TransactionOperation::Begin,
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                },
            ));
        })
        .unwrap();
    store
        .conn
        .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);

    assert!(!outcome.search_projection_ready);
    assert!(outcome.maintenance_error.is_some());
    assert_eq!(store.get_record(record_id).unwrap().id, record_id);
    assert!(store.search_projection_maintenance_pending().unwrap());
    while store.run_search_projection_maintenance_slice().unwrap() {}
    assert!(store.search_projection_ready().unwrap());
}

#[test]
fn large_archive_resumes_from_durable_batch_cursor_after_simulated_crash() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let archive = SessionHistoryArchive {
        records: (0..(ARCHIVE_IMPORT_BATCH_ROWS + 9))
            .map(|index| record(new_id(), &format!("resume-{index}")))
            .collect(),
        ..SessionHistoryArchive::default()
    };
    let fingerprint = archive_import_fingerprint(&archive, b"archive").unwrap();
    store
        .with_write_transaction(|| {
            let tx = ArchiveWriteTransaction { conn: &store.conn };
            ensure_archive_import_progress_table(&tx)?;
            import_archive_stage_range_tx(
                &tx,
                &archive,
                ArchiveImportStage::Records,
                0,
                ARCHIVE_IMPORT_BATCH_ROWS,
                None,
            )?;
            set_archive_import_progress(
                &tx,
                &fingerprint,
                ArchiveImportStage::Records,
                ARCHIVE_IMPORT_BATCH_ROWS,
                false,
            )
        })
        .unwrap();

    store.import_archive(&archive, false).unwrap();

    assert_eq!(
        store.list_records(usize::MAX).unwrap().len(),
        archive.records.len()
    );
    let progress: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM archive_import_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(progress, 0);
}

#[test]
fn archive_no_overwrite_rechecks_conflicts_after_read_validation() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let record_id = new_id();
    let incoming = record(record_id, "incoming");
    let archive = SessionHistoryArchive {
        records: vec![incoming],
        ..SessionHistoryArchive::default()
    };
    store
        .validate_archive_stage_snapshot(&archive, ArchiveImportStage::Records, 0, 1, false)
        .unwrap();

    let concurrent = Store::open(&path).unwrap();
    concurrent
        .upsert_record(&record(record_id, "concurrent-newer"))
        .unwrap();
    drop(concurrent);

    let error = store
        .with_write_transaction(|| {
            let tx = ArchiveWriteTransaction { conn: &store.conn };
            reject_import_stage_conflicts(&tx, &archive, ArchiveImportStage::Records, 0, 1, false)
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::ImportConflict { .. }));
    assert!(store
        .get_record(record_id)
        .unwrap()
        .title
        .contains("concurrent-newer"));
}

#[test]
fn archive_batching_limits_a_single_large_payload_to_one_row_transaction() {
    let mut huge = record(new_id(), "huge");
    huge.body = "x".repeat(super::ARCHIVE_IMPORT_BATCH_BYTES as usize + 1);
    let archive = SessionHistoryArchive {
        records: vec![huge, record(new_id(), "small")],
        ..SessionHistoryArchive::default()
    };

    assert_eq!(
        archive_import_batch_end(&archive, ArchiveImportStage::Records, 0, 2).unwrap(),
        1
    );
}

#[test]
fn archive_disk_estimate_saturates_across_metadata_payload_and_declared_blobs() {
    let temp = tempdir();
    let archive = SessionHistoryArchive {
        artifact_records: vec![artifact(new_id(), sha256_hex(b"x"), u64::MAX)],
        ..SessionHistoryArchive::default()
    };

    assert_eq!(
        estimate_archive_write_bytes(&temp.path().join("work.sqlite"), &archive).unwrap(),
        u64::MAX
    );
}

#[test]
fn archive_revalidates_disk_after_admission_before_publishing_rows() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record = record(new_id(), "external-reserve-consumption");
    let archive = SessionHistoryArchive {
        records: vec![record.clone()],
        ..SessionHistoryArchive::default()
    };
    let checks = Rc::new(Cell::new(0_usize));
    let hook_checks = Rc::clone(&checks);
    let _probe = install_test_disk_space_probe(move |_path, operation| {
        if operation == "history archive import" {
            let current = hook_checks.get();
            hook_checks.set(current + 1);
            return Ok(if current == 0 { u64::MAX } else { 0 });
        }
        Ok(u64::MAX)
    });

    let error = store.import_archive(&archive, false).unwrap_err();

    assert!(matches!(error, StoreError::InsufficientDiskSpace { .. }));
    assert!(checks.get() >= 2);
    assert!(matches!(
        store.get_record(record.id),
        Err(StoreError::NotFound(_))
    ));
}

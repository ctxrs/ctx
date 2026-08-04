use super::*;

#[test]
fn schema_and_projection_sqlite_failures_have_distinct_stable_diagnostics() {
    for (phase, error) in [
        (
            SqliteFailurePhase::Schema,
            OpenCodeSourceBackedError::Capture(CaptureError::Sqlite(
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                    None,
                ),
            )),
        ),
        (
            SqliteFailurePhase::Projection,
            OpenCodeSourceBackedError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                None,
            )),
        ),
    ] {
        let error = diagnose_provider_query_error(error, phase);
        let OpenCodeSourceBackedError::SqliteSource(source) = &error else {
            panic!("unexpected diagnosed OpenCode error: {error:?}");
        };
        let diagnostic = source.diagnostic().unwrap();
        assert_eq!(diagnostic.phase, phase);
        assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateBackup);
        assert_eq!(
            diagnostic.sqlite_primary_code,
            Some(rusqlite::ffi::SQLITE_CORRUPT)
        );
        assert_eq!(
            diagnostic.sqlite_extended_code,
            Some(rusqlite::ffi::SQLITE_CORRUPT)
        );
        assert_eq!(diagnostic.retry, SqliteRetryDecision::DoNotRetryCorrupt);
        let rendered = source.to_string();
        assert!(rendered.contains("sqlite_phase="));
        assert!(rendered.contains("artifact_kind=private_backup"));
        assert!(rendered.contains("sqlite_primary_code=11"));
        assert!(rendered.contains("sqlite_extended_code=11"));
        assert!(rendered.contains("copied_pages=0"));
        assert!(rendered.contains("copied_bytes=0"));
        assert!(rendered.contains("retry_decision=do_not_retry_corrupt"));
        assert!(rendered.contains("cleanup_status=not_required"));
        assert_eq!(
            adapter::route_error(error).kind,
            SourceBackedRouteErrorKind::Internal
        );
    }
}

#[test]
fn real_schema_and_projection_full_failures_are_route_fatal() {
    for (phase, error) in [
        (
            SqliteFailurePhase::Schema,
            OpenCodeSourceBackedError::Capture(CaptureError::Sqlite(actual_sqlite_full_error())),
        ),
        (
            SqliteFailurePhase::Projection,
            OpenCodeSourceBackedError::Sqlite(actual_sqlite_full_error()),
        ),
    ] {
        let error = diagnose_provider_query_error(error, phase);
        let OpenCodeSourceBackedError::SqliteSource(source) = &error else {
            panic!("unexpected diagnosed OpenCode error: {error:?}");
        };
        let diagnostic = source.diagnostic().unwrap();
        assert_eq!(diagnostic.phase, phase);
        assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateBackup);
        assert_eq!(
            diagnostic.sqlite_primary_code,
            Some(rusqlite::ffi::SQLITE_FULL)
        );
        assert_eq!(diagnostic.retry, SqliteRetryDecision::RouteFatalResource);
        assert_eq!(
            adapter::route_error(error).kind,
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
    }
}

fn actual_sqlite_full_error() -> rusqlite::Error {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("full.sqlite");
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA page_size=512; PRAGMA max_page_count=2; CREATE TABLE payload(value BLOB)",
        )
        .unwrap();
    for _ in 0..128 {
        if let Err(error) = connection.execute("INSERT INTO payload VALUES (zeroblob(4096))", []) {
            return error;
        }
    }
    panic!("bounded SQLite fixture did not produce SQLITE_FULL")
}

#[test]
fn provider_and_private_backup_corruption_take_distinct_routes() {
    for (phase, artifact, expected) in [
        (
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::ProviderDatabase,
            SourceBackedRouteErrorKind::InvalidSource,
        ),
        (
            SqliteFailurePhase::BackupValidation,
            SqliteArtifactKind::PrivateBackup,
            SourceBackedRouteErrorKind::Internal,
        ),
    ] {
        let source = SqliteSourceAccessError::SqliteControl {
            operation: "certifying the pinned SQLite snapshot",
            code: rusqlite::ffi::SQLITE_CORRUPT,
        }
        .with_diagnostic(phase, artifact, 2, 8_192, SqliteCleanupStatus::NotRequired);
        assert_eq!(
            adapter::route_error(OpenCodeSourceBackedError::SqliteSource(source)).kind,
            expected
        );
    }
}

#[test]
fn injected_private_source_copy_corruption_routes_internal() {
    let provider = tempfile::tempdir().unwrap();
    let database = provider.path().join("opencode.db");
    let writer = write_current_schema(&database, provider.path(), &json!({"type": "text"}));
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    writer
        .execute(
            "INSERT INTO event VALUES ('private-copy-route', 'current-session', 99, 'history.updated', '{}')",
            [],
        )
        .unwrap();
    let root = ProviderSourceRoot::open(provider.path()).unwrap();
    let directory = root.directory().unwrap();
    let parent_handle = directory.try_clone_authority_handle().unwrap();
    let authority = retain_sqlite_source_directory_authority(
        crate::test_provider_sqlite_data_root(),
        &parent_handle,
        provider.path(),
    )
    .unwrap();

    let source = open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test(
        &authority,
        OsStr::new("opencode.db"),
        |source_copy| {
            let mut copy = OpenOptions::new().write(true).open(source_copy).unwrap();
            copy.seek(SeekFrom::Start(0)).unwrap();
            copy.write_all(&[0_u8; 100]).unwrap();
            copy.sync_all().unwrap();
        },
    )
    .unwrap_err();
    let diagnostic = source.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::SourceValidation);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
    assert!(source.is_ctx_owned_corruption());
    assert_eq!(
        adapter::route_error(OpenCodeSourceBackedError::SqliteSource(source)).kind,
        SourceBackedRouteErrorKind::Internal
    );
}

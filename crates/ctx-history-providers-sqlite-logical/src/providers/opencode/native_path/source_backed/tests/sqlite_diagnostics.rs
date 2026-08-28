use std::path::PathBuf;

use super::*;

#[test]
fn snapshot_capacity_failure_is_isolated_to_the_opencode_route() {
    let error = OpenCodeSourceBackedError::SqliteSource(
        SqliteSourceAccessError::InsufficientScratchSpace {
            path: PathBuf::from("ctx-data"),
            required: 10 * 1024 * 1024 * 1024,
            available: 5 * 1024 * 1024 * 1024,
        },
    );

    let route = adapter::route_error(error);
    assert_eq!(route.kind, SourceBackedRouteErrorKind::Unavailable);
    assert_eq!(
        route.kind.source_failure_class(),
        Some(ctx_history_capture_runtime::SourceBackedSourceFailureClass::Unavailable)
    );
}

#[test]
fn terminal_change_with_resource_cleanup_failure_stays_systemic() {
    let error = OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Finalization {
        primary: Box::new(SqliteSourceAccessError::SourceChanged),
        cleanup: Box::new(SqliteSourceAccessError::ResourceUnavailable {
            operation: "cleaning an OpenCode SQLite snapshot",
            path: PathBuf::from("ctx-owned-snapshot.sqlite"),
            source: std::io::Error::from(std::io::ErrorKind::OutOfMemory),
        }),
    });

    assert_eq!(
        adapter::route_error(error).kind,
        SourceBackedRouteErrorKind::ResourceUnavailable
    );
}

#[test]
#[ignore = "explicit multi-gibibyte copy/open resource proof"]
fn opencode_source_above_two_gibibytes_copies_opens_observes_and_cleans_up() {
    const ABOVE_TWO_GIB: u64 = 2 * 1024 * 1024 * 1024 + 4096;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");
    let provider = temp.path().join("provider");
    fs::create_dir_all(&provider).unwrap();
    let database = provider.join("opencode.db");
    drop(write_current_schema(
        &database,
        &provider,
        &json!({"type": "text", "text": "large OpenCode proof"}),
    ));
    fs::OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap()
        .set_len(ABOVE_TWO_GIB)
        .unwrap();

    adapter::discover_document_tree_for_test(
        &data_root,
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();

    assert_no_opencode_snapshot_leaks(&data_root);
}

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
        assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
        assert_eq!(
            diagnostic.sqlite_primary_code,
            Some(rusqlite::ffi::SQLITE_CORRUPT)
        );
        assert_eq!(
            diagnostic.sqlite_extended_code,
            Some(rusqlite::ffi::SQLITE_CORRUPT)
        );
        assert_eq!(
            sqlite_retry_decision(source),
            SqliteRetryDecision::DoNotRetryCorrupt
        );
        let rendered = source.to_string();
        assert!(rendered.contains("sqlite_phase="));
        assert!(rendered.contains("artifact_kind=private_source_copy"));
        assert!(rendered.contains("sqlite_primary_code=11"));
        assert!(rendered.contains("sqlite_extended_code=11"));
        assert!(rendered.contains("copied_pages=0"));
        assert!(rendered.contains("copied_bytes=0"));
        assert!(rendered.contains("cleanup_status=not_required"));
        assert_eq!(
            adapter::route_error(error).kind,
            SourceBackedRouteErrorKind::Internal
        );
    }
}

#[test]
fn production_schema_and_projection_errors_explicitly_report_cleanup_failure_without_leaks() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");
    let provider = temp.path().join("provider");
    fs::create_dir_all(&provider).unwrap();
    let database = provider.join("opencode.db");
    Connection::open(&database)
        .unwrap()
        .execute_batch("CREATE TABLE unsupported(value TEXT)")
        .unwrap();
    fail_next_opened_snapshot_cleanup_for_test();

    let schema = adapter::discover_document_tree_for_test(
        &data_root,
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    );
    let schema = match schema {
        Ok(_) => panic!("unsupported OpenCode schema unexpectedly succeeded"),
        Err(error) => adapter::route_error(error),
    };
    assert_eq!(schema.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert!(schema.detail.contains("cleanup_status=failed"));
    assert_no_opencode_snapshot_leaks(&data_root);

    fs::remove_file(&database).unwrap();
    let writer = write_current_schema(&database, &provider, &json!({"type": "text"}));
    drop(writer);
    fail_next_opened_snapshot_cleanup_for_test();
    let authorized = open_root_authorized_snapshot_retained(&data_root, &database).unwrap();
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let projection = scan_pinned_source(
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |_| {
            Err(OpenCodeSourceBackedError::Route(
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    "injected OpenCode projection failure",
                ),
            ))
        },
    )
    .unwrap_err();
    let projection = adapter::route_error(projection);
    assert_eq!(
        projection.kind,
        SourceBackedRouteErrorKind::ResourceUnavailable
    );
    assert!(projection.detail.contains("cleanup_status=failed"));
    assert_no_opencode_snapshot_leaks(&data_root);
}

fn assert_no_opencode_snapshot_leaks(data_root: &Path) {
    let staging = data_root.join("tmp/provider-sqlite");
    if staging.exists() {
        assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
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
        assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
        assert_eq!(
            diagnostic.sqlite_primary_code,
            Some(rusqlite::ffi::SQLITE_FULL)
        );
        assert_eq!(
            sqlite_retry_decision(source),
            SqliteRetryDecision::RouteFatalResource
        );
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
fn provider_and_private_copy_corruption_take_distinct_routes() {
    for (phase, artifact, expected) in [
        (
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::ProviderDatabase,
            SourceBackedRouteErrorKind::InvalidSource,
        ),
        (
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::PrivateSourceCopy,
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

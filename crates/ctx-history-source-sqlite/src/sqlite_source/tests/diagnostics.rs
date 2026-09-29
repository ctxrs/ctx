use super::*;

#[test]
fn contextual_source_errors_preserve_absence_and_resource_classification() {
    for (kind, unavailable, resource) in [
        (io::ErrorKind::NotFound, true, false),
        (io::ErrorKind::PermissionDenied, true, false),
        (io::ErrorKind::StorageFull, false, true),
        (io::ErrorKind::OutOfMemory, false, true),
    ] {
        let error = super::super::map_provider_source_error(
            ctx_history_source_io::SourceIoError::SystemIo {
                operation: "provider source target open",
                source: io::Error::from(kind),
            },
            "opening a SQLite source family member",
            Path::new("provider.sqlite-wal"),
        );
        assert_eq!(error.is_provider_path_unavailable(), unavailable);
        assert_eq!(error.is_systemic_resource_failure(), resource);
        if resource {
            assert_revalidation_resource_unavailable(map_revalidation_error(error));
        } else {
            assert!(map_revalidation_error(error).is_source_changed());
        }
    }
}

#[test]
fn snapshot_capacity_failures_are_distinct_from_runtime_resource_failures() {
    let capacity = SqliteSourceAccessError::InsufficientScratchSpace {
        path: PathBuf::from("ctx-data"),
        required: 10,
        available: 9,
    };
    assert!(capacity.is_snapshot_capacity_failure());
    assert!(capacity.is_systemic_resource_failure());

    let diagnosed = capacity.with_diagnostic(
        SqliteFailurePhase::SourceAcquisition,
        SqliteArtifactKind::PrivateSourceCopy,
        0,
        0,
        SqliteCleanupStatus::NotRequired,
    );
    assert!(diagnosed.is_snapshot_capacity_failure());

    let runtime = SqliteSourceAccessError::ScratchIoUnavailable {
        operation: "writing a private SQLite source-family copy",
        path: PathBuf::from("ctx-data/source.sqlite"),
        source: io::Error::from(io::ErrorKind::StorageFull),
    };
    assert!(!runtime.is_snapshot_capacity_failure());
    assert!(runtime.is_systemic_resource_failure());

    let finalization = SqliteSourceAccessError::Finalization {
        primary: Box::new(diagnosed),
        cleanup: Box::new(runtime),
    };
    assert!(!finalization.is_snapshot_capacity_failure());
    assert!(finalization.is_systemic_resource_failure());
}

fn assert_revalidation_resource_unavailable(error: SqliteSourceAccessError) {
    assert!(error.is_systemic_resource_failure());
    assert!(matches!(
        error,
        SqliteSourceAccessError::ResourceUnavailable { .. }
    ));
}

#[cfg(unix)]
#[test]
fn sqlite_authority_revalidation_preserves_descriptor_exhaustion() {
    let path = Path::new("/provider/provider.sqlite");
    for code in [libc::EMFILE, libc::ENFILE] {
        let error = map_revalidation_io_error(
            io::Error::from_raw_os_error(code),
            "retaining SQLite authority in the regression test",
            path,
        );
        match &error {
            SqliteSourceAccessError::ResourceUnavailable {
                operation,
                path: error_path,
                source,
            } => {
                assert_eq!(
                    *operation,
                    "retaining SQLite authority in the regression test"
                );
                assert_eq!(error_path, path);
                assert_eq!(source.raw_os_error(), Some(code));
            }
            other => panic!("unexpected revalidation error: {other:?}"),
        }
        assert_revalidation_resource_unavailable(error);
    }
}

#[test]
fn sqlite_authority_revalidation_preserves_portable_resource_exhaustion() {
    let path = Path::new("provider.sqlite");
    for kind in [
        io::ErrorKind::OutOfMemory,
        io::ErrorKind::StorageFull,
        io::ErrorKind::QuotaExceeded,
    ] {
        let error = map_revalidation_io_error(
            io::Error::from(kind),
            "retaining SQLite authority in the portable regression test",
            path,
        );
        assert!(matches!(
            &error,
            SqliteSourceAccessError::ResourceUnavailable { source, .. }
                if source.kind() == kind
        ));
        assert_revalidation_resource_unavailable(error);
    }
}

#[test]
fn sqlite_primary_resource_codes_are_exhaustive_for_control_and_rusqlite_errors() {
    for code in [
        ffi::SQLITE_FULL,
        ffi::SQLITE_NOMEM,
        ffi::SQLITE_IOERR,
        ffi::SQLITE_CANTOPEN,
        ffi::SQLITE_PERM,
        ffi::SQLITE_READONLY,
    ] {
        let control = SqliteSourceAccessError::SqliteControl {
            operation: "classifying a SQLite control failure",
            code,
        };
        assert!(
            control.is_systemic_resource_failure(),
            "control code {code}"
        );
        let rusqlite = SqliteSourceAccessError::Sqlite {
            operation: "classifying a rusqlite failure",
            source: rusqlite::Error::SqliteFailure(ffi::Error::new(code), None),
        };
        assert!(
            rusqlite.is_systemic_resource_failure(),
            "rusqlite code {code}"
        );
    }
    for code in [
        ffi::SQLITE_BUSY,
        ffi::SQLITE_LOCKED,
        ffi::SQLITE_CORRUPT,
        ffi::SQLITE_NOTADB,
    ] {
        let error = SqliteSourceAccessError::Sqlite {
            operation: "classifying a non-resource SQLite failure",
            source: rusqlite::Error::SqliteFailure(ffi::Error::new(code), None),
        };
        assert!(!error.is_systemic_resource_failure(), "SQLite code {code}");
    }
}

#[cfg(unix)]
#[test]
fn raw_io_resource_codes_are_systemic_without_revalidation_rewriting() {
    for code in [
        libc::ENOSPC,
        libc::EMFILE,
        libc::ENFILE,
        libc::ENOMEM,
        libc::EDQUOT,
    ] {
        let error = SqliteSourceAccessError::Io {
            operation: "reading a SQLite resource fixture",
            path: PathBuf::from("provider.sqlite"),
            source: io::Error::from_raw_os_error(code),
        };
        assert!(error.is_systemic_resource_failure(), "raw OS code {code}");
    }
}

#[test]
fn sqlite_authority_revalidation_keeps_mutation_as_source_changed() {
    assert!(matches!(
        map_revalidation_error(SqliteSourceAccessError::ConnectionIdentityMismatch),
        SqliteSourceAccessError::SourceChanged
    ));
    assert!(matches!(
        map_revalidation_error(SqliteSourceAccessError::SourceChanged),
        SqliteSourceAccessError::SourceChanged
    ));
    assert!(matches!(
        map_revalidation_io_error(
            io::Error::from(io::ErrorKind::NotFound),
            "reopening mutated SQLite authority in the regression test",
            Path::new("provider.sqlite"),
        ),
        SqliteSourceAccessError::SourceChanged
    ));
}

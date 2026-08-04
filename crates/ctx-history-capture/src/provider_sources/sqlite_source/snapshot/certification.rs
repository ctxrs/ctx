use super::*;

#[derive(Clone, Copy)]
pub(super) struct OnlineBackupBounds {
    pub(super) page_count: u64,
    pub(super) page_size: u64,
    pub(super) bytes: u64,
}

pub(super) fn enforce_online_backup_bounds(
    connection: &Connection,
    path: &Path,
    scratch_limit: u64,
) -> SqliteSourceAccessResult<OnlineBackupBounds> {
    let page_count: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|source| sqlite_error("reading online-backup page count", source))?;
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|source| sqlite_error("reading online-backup page size", source))?;
    let page_count =
        u64::try_from(page_count).map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the provider SQLite page count is negative".to_owned(),
        })?;
    let page_size =
        u64::try_from(page_size).map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the provider SQLite page size is negative".to_owned(),
        })?;
    let bytes = page_count.checked_mul(page_size).ok_or_else(|| {
        SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length: u64::MAX,
            maximum: scratch_limit,
        }
    })?;
    if bytes > scratch_limit {
        return Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length: bytes,
            maximum: scratch_limit,
        });
    }
    Ok(OnlineBackupBounds {
        page_count,
        page_size,
        bytes,
    })
}

pub(super) fn certify_sqlite_snapshot(
    connection: &Connection,
    bounds: OnlineBackupBounds,
    phase: SqliteFailurePhase,
    artifact: SqliteArtifactKind,
    copied_pages: u64,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<SqliteValidationMeasurement> {
    let started = Instant::now();
    let deadline = started
        .checked_add(SQLITE_CERTIFICATION_DEADLINE)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the SQLite certification deadline overflowed".to_owned(),
        })?;
    connection.progress_handler(
        SQLITE_CERTIFICATION_PROGRESS_OPS,
        Some(move || Instant::now() >= deadline),
    );
    let result = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0));
    connection.progress_handler(0, None::<fn() -> bool>);
    let result = result.map_err(|source| {
        sqlite_error("certifying the pinned SQLite snapshot", source).with_diagnostic(
            phase,
            artifact,
            copied_pages,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        )
    })?;
    if result != "ok" {
        return Err(SqliteSourceAccessError::SqliteControl {
            operation: "certifying the pinned SQLite snapshot",
            code: ffi::SQLITE_CORRUPT,
        }
        .with_diagnostic(
            phase,
            artifact,
            copied_pages,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        ));
    }
    Ok(SqliteValidationMeasurement {
        pages: bounds.page_count,
        bytes: bounds.bytes,
        #[cfg(test)]
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

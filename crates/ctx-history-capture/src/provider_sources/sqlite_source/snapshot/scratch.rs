use super::*;

const SQLITE_PRIVATE_SCRATCH_PAGE_BYTES: u64 = 4 * 1024;
const SQLITE_PRIVATE_SCRATCH_CACHE_KIB: i64 = 512;

impl SqliteSourceReadSnapshot {
    /// Runs one bounded external operation against an ordinary SQLite database
    /// in a private ctx-owned directory.
    ///
    /// The caller must build persistent tables/indexes incrementally rather
    /// than requesting SQLite temporary tables or temp B-tree sorts. This
    /// database is the explicit spill authority; it and all of its sidecars
    /// are removed when the callback returns, including on error unwind.
    pub(crate) fn with_private_scratch_database<T, E>(
        &self,
        prefix: &str,
        maximum_bytes: u64,
        use_scratch: impl FnOnce(&Connection, &Path) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<SqliteSourceAccessError>,
    {
        self.with_private_scratch_database_inner(prefix, maximum_bytes, use_scratch, |_| {})
    }

    fn with_private_scratch_database_inner<T, E>(
        &self,
        prefix: &str,
        maximum_bytes: u64,
        use_scratch: impl FnOnce(&Connection, &Path) -> Result<T, E>,
        after_use: impl FnOnce(&Path),
    ) -> Result<T, E>
    where
        E: From<SqliteSourceAccessError>,
    {
        self.connection().map_err(E::from)?;
        let maximum_pages = maximum_bytes / SQLITE_PRIVATE_SCRATCH_PAGE_BYTES;
        let maximum_pages = i64::try_from(maximum_pages).map_err(|_| {
            E::from(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the private SQLite scratch page limit is not representable".to_owned(),
            })
        })?;
        if maximum_pages == 0 {
            return Err(E::from(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the private SQLite scratch limit is smaller than one page".to_owned(),
            }));
        }
        let directory =
            create_scratch_directory(&self.snapshot_context.data_root, prefix).map_err(E::from)?;
        let scratch_directory_path = directory.path().to_path_buf();
        let scratch_path = directory.path().join("scratch.sqlite");
        let connection = match Connection::open_with_flags(
            &scratch_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ) {
            Ok(connection) => connection,
            Err(source) => {
                let open_error = E::from(scratch_sqlite_error(
                    "creating the private provider SQLite scratch database",
                    source,
                ));
                return match directory.close() {
                    Ok(()) => Err(open_error),
                    Err(source) => Err(E::from(scratch_io_error(
                        "cleaning the private provider SQLite scratch directory",
                        scratch_directory_path,
                        source,
                    ))),
                };
            }
        };
        let operation = (|| {
            connection
                .pragma_update(None, "page_size", SQLITE_PRIVATE_SCRATCH_PAGE_BYTES)
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "configuring the private provider SQLite scratch page size",
                        source,
                    ))
                })?;
            let configured_page_bytes: i64 = connection
                .pragma_query_value(None, "page_size", |row| row.get(0))
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "verifying the private provider SQLite scratch page size",
                        source,
                    ))
                })?;
            if u64::try_from(configured_page_bytes) != Ok(SQLITE_PRIVATE_SCRATCH_PAGE_BYTES) {
                return Err(E::from(SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the private SQLite scratch page size was not enforced".to_owned(),
                }));
            }
            let journal_mode: String = connection
                .query_row("PRAGMA journal_mode=OFF", [], |row| row.get(0))
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "disabling the disposable SQLite scratch journal",
                        source,
                    ))
                })?;
            if journal_mode != "off" {
                return Err(E::from(SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the private SQLite scratch journal could not be disabled".to_owned(),
                }));
            }
            // The ordinary scratch database is the bounded, disk-backed sorter.
            // MEMORY here is only a fail-closed guard against any unapproved
            // SQLite temp object; callers also preflight and runtime-check plans so
            // corpus ordering never falls back to SQLite's in-memory temp store.
            connection
                .pragma_update(None, "synchronous", "OFF")
                .and_then(|()| connection.pragma_update(None, "mmap_size", 0_i64))
                .and_then(|()| {
                    connection.pragma_update(None, "cache_size", -SQLITE_PRIVATE_SCRATCH_CACHE_KIB)
                })
                .and_then(|()| connection.pragma_update(None, "temp_store", "MEMORY"))
                .and_then(|()| connection.pragma_update(None, "max_page_count", maximum_pages))
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "bounding the private provider SQLite scratch database",
                        source,
                    ))
                })?;
            let configured_pages: i64 = connection
                .pragma_query_value(None, "max_page_count", |row| row.get(0))
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "verifying the private provider SQLite scratch limit",
                        source,
                    ))
                })?;
            let configured_cache_kib: i64 = connection
                .pragma_query_value(None, "cache_size", |row| row.get(0))
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "verifying the private provider SQLite scratch cache bound",
                        source,
                    ))
                })?;
            let configured_temp_store: i64 = connection
                .pragma_query_value(None, "temp_store", |row| row.get(0))
                .map_err(|source| {
                    E::from(scratch_sqlite_error(
                        "verifying the private provider SQLite temp authority",
                        source,
                    ))
                })?;
            if configured_pages > maximum_pages
                || configured_cache_kib != -SQLITE_PRIVATE_SCRATCH_CACHE_KIB
                || configured_temp_store != 2
            {
                return Err(E::from(SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the private SQLite scratch bounds were not enforced".to_owned(),
                }));
            }
            use_scratch(&connection, &scratch_path)
        })();
        after_use(directory.path());
        let close = connection.close().map_err(|(_, source)| {
            E::from(scratch_sqlite_error(
                "closing the private provider SQLite scratch database",
                source,
            ))
        });
        let cleanup = directory.close().map_err(|source| {
            E::from(scratch_io_error(
                "cleaning the private provider SQLite scratch directory",
                scratch_directory_path,
                source,
            ))
        });
        match (operation, close, cleanup) {
            (_, _, Err(cleanup)) => Err(cleanup),
            (_, Err(close), Ok(())) => Err(close),
            (operation, Ok(()), Ok(())) => operation,
        }
    }

    #[cfg(test)]
    pub(in crate::provider_sources::sqlite_source) fn with_private_scratch_database_after_use_for_test<
        T,
        E,
    >(
        &self,
        prefix: &str,
        maximum_bytes: u64,
        use_scratch: impl FnOnce(&Connection, &Path) -> Result<T, E>,
        after_use: impl FnOnce(&Path),
    ) -> Result<T, E>
    where
        E: From<SqliteSourceAccessError>,
    {
        self.with_private_scratch_database_inner(prefix, maximum_bytes, use_scratch, after_use)
    }
}

fn create_scratch_directory(data_root: &Path, prefix: &str) -> SqliteSourceAccessResult<TempDir> {
    let staging_root = data_root.join("tmp").join("provider-sqlite-scratch");
    create_private_directory_all(&staging_root).map_err(|source| {
        scratch_io_error(
            "creating the private provider SQLite scratch root",
            staging_root.clone(),
            source,
        )
    })?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&staging_root)
        .map_err(|source| {
            scratch_io_error(
                "creating a private provider SQLite scratch directory",
                staging_root,
                source,
            )
        })
}

fn scratch_sqlite_error(
    operation: &'static str,
    source: rusqlite::Error,
) -> SqliteSourceAccessError {
    let error = SqliteSourceAccessError::private_scratch_sqlite(operation, source);
    if operation.starts_with("closing") {
        error.with_diagnostic(
            SqliteFailurePhase::Cleanup,
            SqliteArtifactKind::PrivateScratch,
            0,
            0,
            SqliteCleanupStatus::Failed,
        )
    } else {
        error
    }
}

fn scratch_io_error(
    operation: &'static str,
    path: PathBuf,
    source: std::io::Error,
) -> SqliteSourceAccessError {
    let error = SqliteSourceAccessError::ScratchIoUnavailable {
        operation,
        path,
        source,
    };
    if operation.starts_with("cleaning") {
        error.with_diagnostic(
            SqliteFailurePhase::Cleanup,
            SqliteArtifactKind::PrivateScratch,
            0,
            0,
            SqliteCleanupStatus::Failed,
        )
    } else {
        error
    }
}

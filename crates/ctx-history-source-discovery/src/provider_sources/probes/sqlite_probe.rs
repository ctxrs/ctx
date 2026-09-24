use super::*;

#[derive(Clone, Copy)]
pub(super) struct SqliteProbeLimits {
    pub(super) max_total_bytes: u64,
    pub(super) deadline: Duration,
    pub(super) max_progress_calls: usize,
}

#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "typed probe failures are inspected by direct tests"
    )
)]
pub(super) enum SqliteProbePrimaryError {
    BudgetExhausted,
    Connection(SqliteSourceAccessError),
    Configuration(rusqlite::Error),
    Query(rusqlite::Error),
}

pub(super) type SqliteProbeExecutionError =
    SqliteReadFinalizationError<SqliteProbePrimaryError, SqliteSourceAccessError>;

#[cfg(test)]
pub(super) fn fail_next_sqlite_probe_connection_for_test() {
    FAIL_NEXT_SQLITE_PROBE_CONNECTION.with(|fail| fail.set(true));
}

#[cfg(test)]
fn take_sqlite_probe_connection_failure_for_test() -> bool {
    FAIL_NEXT_SQLITE_PROBE_CONNECTION.with(|fail| fail.replace(false))
}

impl Default for SqliteProbeLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: SQLITE_PROBE_MAX_TOTAL_BYTES,
            deadline: SQLITE_PROBE_DEADLINE,
            max_progress_calls: SQLITE_PROBE_MAX_PROGRESS_CALLS,
        }
    }
}

pub(super) fn sqlite_structural_probe(
    data_root: Option<&Path>,
    path: &Path,
    limits: SqliteProbeLimits,
    query: impl FnOnce(&Connection) -> rusqlite::Result<bool>,
) -> BoundedProbe {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return BoundedProbe::IoError;
    };
    let Some(database_name) = path.file_name() else {
        return BoundedProbe::IoError;
    };
    let source_root = match ProviderSourceRoot::open(parent) {
        Ok(root) => root,
        Err(_) => return BoundedProbe::IoError,
    };
    let source_directory = match source_root.directory() {
        Ok(directory) => directory,
        Err(_) => return BoundedProbe::IoError,
    };
    let parent_handle = match source_directory.try_clone_authority_handle() {
        Ok(handle) => handle,
        Err(_) => return BoundedProbe::IoError,
    };
    let (scratch_root, snapshot_limits) = data_root.map_or_else(
        || {
            (
                parent,
                SqliteSourceSnapshotLimits::without_scratch(limits.max_total_bytes),
            )
        },
        |data_root| {
            (
                data_root,
                SqliteSourceSnapshotLimits::new(limits.max_total_bytes),
            )
        },
    );
    let authority =
        match retain_sqlite_source_directory_authority(scratch_root, &parent_handle, parent) {
            Ok(authority) => authority,
            Err(_) => return BoundedProbe::IoError,
        };
    let snapshot = match open_root_handle_sqlite_source_snapshot_with_limits(
        &authority,
        database_name,
        snapshot_limits,
    ) {
        Ok(snapshot) => snapshot,
        Err(error) if error.is_systemic_resource_failure() => return BoundedProbe::BudgetExhausted,
        Err(error) if error.is_provider_path_unavailable() => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    };
    classify_sqlite_probe_execution(execute_sqlite_structural_probe(
        snapshot,
        limits,
        configure_sqlite_probe,
        query,
    ))
}

pub(super) fn execute_sqlite_structural_probe(
    snapshot: SqliteSourceReadSnapshot,
    limits: SqliteProbeLimits,
    configure: impl FnOnce(&Connection, Duration) -> rusqlite::Result<()>,
    query: impl FnOnce(&Connection) -> rusqlite::Result<bool>,
) -> Result<bool, Box<SqliteProbeExecutionError>> {
    let exhausted = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + limits.deadline;
    let progress_exhausted = Arc::clone(&exhausted);
    let mut progress_calls = 0usize;
    #[cfg(test)]
    let connection = if take_sqlite_probe_connection_failure_for_test() {
        Err(SqliteSourceAccessError::SnapshotNotActive)
    } else {
        snapshot.connection()
    };
    #[cfg(not(test))]
    let connection = snapshot.connection();
    let query_result = match connection {
        Ok(connection) => {
            let configured = connection
                .progress_handler(
                    SQLITE_PROBE_PROGRESS_OPS,
                    Some(move || {
                        progress_calls = progress_calls.saturating_add(1);
                        let stop = progress_calls > limits.max_progress_calls
                            || Instant::now() >= deadline;
                        if stop {
                            progress_exhausted.store(true, Ordering::Relaxed);
                        }
                        stop
                    }),
                )
                .and_then(|()| configure(connection, limits.deadline));
            let result = match configured {
                Ok(()) => query(connection).map_err(SqliteProbePrimaryError::Query),
                Err(error) => Err(SqliteProbePrimaryError::Configuration(error)),
            };
            let cleared = connection.progress_handler(0, None::<fn() -> bool>);
            result.and_then(|value| {
                cleared
                    .map(|()| value)
                    .map_err(SqliteProbePrimaryError::Configuration)
            })
        }
        Err(error) => Err(SqliteProbePrimaryError::Connection(error)),
    };
    let primary = if exhausted.load(Ordering::Relaxed) {
        Err(SqliteProbePrimaryError::BudgetExhausted)
    } else {
        query_result
    };
    snapshot.finish_with(primary).map_err(Box::new)
}

fn classify_sqlite_probe_execution(
    result: Result<bool, Box<SqliteProbeExecutionError>>,
) -> BoundedProbe {
    match result {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(error) => match *error {
            SqliteReadFinalizationError::Primary(SqliteProbePrimaryError::BudgetExhausted) => {
                BoundedProbe::BudgetExhausted
            }
            SqliteReadFinalizationError::Primary(SqliteProbePrimaryError::Connection(error))
                if error.is_systemic_resource_failure() =>
            {
                BoundedProbe::BudgetExhausted
            }
            SqliteReadFinalizationError::Primary(_)
            | SqliteReadFinalizationError::Finalization(_)
            | SqliteReadFinalizationError::PrimaryAndFinalization { .. } => BoundedProbe::IoError,
        },
    }
}

pub(super) fn configure_sqlite_probe(
    connection: &Connection,
    deadline: Duration,
) -> rusqlite::Result<()> {
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(i32::MAX);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, value_limit)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_COLUMN, 256)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_EXPR_DEPTH, 100)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_COMPOUND_SELECT, 16)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_VDBE_OP, 100_000)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_ATTACHED, 0)?;
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_WORKER_THREADS, 0)?;
    connection.busy_timeout(deadline)?;
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "trusted_schema", false)
}

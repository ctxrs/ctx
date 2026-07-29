use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Days, Utc};
use ctx_history_core::platform_security::{
    create_private_directory_all, restrict_private_file_handle, verify_private_file,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

use super::{CompletedOperation, CTX_VERSION, DEFINITION_VERSION, RETENTION_DAYS};

mod file_family;

#[cfg(test)]
pub(super) use file_family::capture_with_between_reads_for_test;
#[cfg(all(test, windows))]
pub(super) use file_family::{assert_single_link_for_test, verify_same_file_for_test};
use file_family::{
    capture_checkpointed_image, deserialize_read_only, open_nofollow, preflight_auxiliaries,
    preflight_existing_family, protect_sqlite_files, reopen_same_file, verify_file_owner,
    verify_metadata_owner, verify_private_directory_and_owner, verify_same_file,
    verify_single_link, FamilyGuard,
};

pub(crate) const USAGE_FILE: &str = "usage.sqlite";
const APPLICATION_ID: i64 = 0x4354_5855;
const LEGACY_SCHEMA_VERSION: i64 = 1;
const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_millis(25);
const PAGE_SIZE_BYTES: i64 = 4 * 1024;
const MAX_DATABASE_BYTES: i64 = 6 * 1024 * 1024;
const MAX_PAGE_COUNT: i64 = MAX_DATABASE_BYTES / PAGE_SIZE_BYTES;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 64;
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 1024 * 1024;
const STALE_INIT_AGE: Duration = Duration::from_secs(60 * 60);
const INIT_SLOT_COUNT: usize = 8;

const DAILY_USAGE_SCHEMA_V1: &str = r#"
CREATE TABLE daily_usage (
    day_utc TEXT NOT NULL
        CHECK (
            length(day_utc) = 10
            AND day_utc GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(day_utc) IS NOT NULL
            AND date(day_utc) = day_utc
        ),
    definition_version INTEGER NOT NULL CHECK (definition_version = 1),
    ctx_version TEXT NOT NULL
        CHECK (
            length(ctx_version) BETWEEN 1 AND 64
            AND ctx_version NOT GLOB '*[^0-9A-Za-z.+-]*'
        ),
    surface TEXT NOT NULL CHECK (surface IN ('cli', 'mcp')),
    operation TEXT NOT NULL CHECK (
        (
            surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show',
                'locate', 'search', 'pro_setup', 'pro_manage', 'pro_uninstall',
                'blame', 'sql', 'docs', 'integrations', 'daemon_status',
                'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            surface = 'mcp'
            AND operation IN (
                'status', 'sources', 'search', 'sql', 'show_session',
                'show_event', 'pro_status', 'blame'
            )
        )
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failure')),
    value_class TEXT NOT NULL
        CHECK (value_class IN ('result_bearing', 'empty', 'not_applicable')),
    duration_bucket TEXT NOT NULL
        CHECK (duration_bucket IN (
            'under_10_ms', '10_to_49_ms', '50_to_249_ms', '250_to_999_ms',
            '1_to_4_s', '5_to_29_s', '30_s_or_more'
        )),
    target_type TEXT NOT NULL
        CHECK (target_type IN ('file', 'commit', 'pull_request', 'not_applicable')),
    pro_outcome TEXT NOT NULL
        CHECK (
            (
                operation = 'blame'
                AND (
                    (outcome = 'failure' AND pro_outcome = 'error')
                    OR
                    (
                        outcome = 'success'
                        AND pro_outcome IN ('produced', 'possible', 'none')
                    )
                )
            )
            OR (operation != 'blame' AND pro_outcome = 'not_applicable')
        ),
    calls INTEGER NOT NULL CHECK (calls > 0),
    result_count INTEGER NOT NULL CHECK (result_count >= 0),
    citation_count INTEGER NOT NULL
        CHECK (citation_count >= 0 AND (operation = 'blame' OR citation_count = 0)),
    response_bytes INTEGER NOT NULL
        CHECK (
            (surface = 'cli' AND response_bytes = 0)
            OR (surface = 'mcp' AND response_bytes > 0)
        ),
    CHECK (
        (
            outcome = 'failure'
            AND value_class = 'not_applicable'
            AND result_count = 0
            AND citation_count = 0
        )
        OR outcome = 'success'
    ),
    CHECK (
        (value_class = 'result_bearing' AND result_count >= calls)
        OR (
            value_class IN ('empty', 'not_applicable')
            AND result_count = 0
            AND citation_count = 0
        )
    ),
    CHECK (
        operation = 'blame'
        OR (
            target_type = 'not_applicable'
            AND pro_outcome = 'not_applicable'
            AND citation_count = 0
        )
    ),
    CHECK (
        operation != 'blame'
        OR (
            target_type IN ('file', 'commit', 'pull_request')
            OR (outcome = 'failure' AND target_type = 'not_applicable')
        )
    ),
    CHECK (
        outcome = 'failure'
        OR (
            surface = 'cli'
            AND (
                (operation = 'blame' AND value_class IN ('result_bearing', 'empty'))
                OR (operation != 'blame' AND value_class = 'not_applicable')
            )
        )
        OR (
            surface = 'mcp'
            AND (
                (
                    operation IN (
                        'sources', 'search', 'sql', 'show_session', 'show_event', 'blame'
                    )
                    AND value_class IN ('result_bearing', 'empty')
                )
                OR (
                    operation IN ('status', 'pro_status')
                    AND value_class = 'not_applicable'
                )
            )
        )
    ),
    PRIMARY KEY (
        day_utc, definition_version, ctx_version, surface, operation, outcome,
        value_class, duration_bucket, target_type, pro_outcome
    )
) WITHOUT ROWID, STRICT;
"#;

const DAILY_USAGE_SCHEMA: &str = r#"
CREATE TABLE daily_usage (
    day_utc TEXT NOT NULL
        CHECK (
            length(day_utc) = 10
            AND day_utc GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(day_utc) IS NOT NULL
            AND date(day_utc) = day_utc
        ),
    definition_version INTEGER NOT NULL CHECK (definition_version = 2),
    ctx_version TEXT NOT NULL
        CHECK (
            length(ctx_version) BETWEEN 1 AND 64
            AND ctx_version NOT GLOB '*[^0-9A-Za-z.+-]*'
        ),
    surface TEXT NOT NULL CHECK (surface IN ('cli', 'mcp')),
    operation TEXT NOT NULL CHECK (
        (
            surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show',
                'locate', 'search', 'pro_setup', 'pro_manage', 'pro_uninstall',
                'blame', 'sql', 'docs', 'integrations', 'daemon_status',
                'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            surface = 'mcp'
            AND operation IN (
                'status', 'sources', 'search', 'sql', 'show_session',
                'show_event', 'pro_status', 'blame'
            )
        )
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failure')),
    value_class TEXT NOT NULL
        CHECK (value_class IN ('result_bearing', 'empty', 'not_applicable')),
    duration_bucket TEXT NOT NULL
        CHECK (duration_bucket IN (
            'under_10_ms', '10_to_49_ms', '50_to_249_ms', '250_to_999_ms',
            '1_to_4_s', '5_to_29_s', '30_s_or_more'
        )),
    target_type TEXT NOT NULL
        CHECK (target_type IN ('file', 'commit', 'pull_request', 'not_applicable')),
    pro_outcome TEXT NOT NULL
        CHECK (
            (
                operation = 'blame'
                AND (
                    (outcome = 'failure' AND pro_outcome = 'error')
                    OR
                    (
                        outcome = 'success'
                        AND pro_outcome IN ('produced', 'possible', 'none')
                    )
                )
            )
            OR (operation != 'blame' AND pro_outcome = 'not_applicable')
        ),
    result_action TEXT NOT NULL CHECK (
        result_action IN (
            'search', 'open_session', 'open_event', 'locate',
            'sources', 'sql', 'blame', 'not_applicable'
        )
    ),
    calls INTEGER NOT NULL CHECK (calls > 0),
    result_count INTEGER NOT NULL CHECK (result_count >= 0),
    citation_count INTEGER NOT NULL CHECK (citation_count >= 0),
    latency_ms INTEGER NOT NULL CHECK (latency_ms >= 0),
    latency_samples INTEGER NOT NULL
        CHECK (latency_samples BETWEEN 0 AND calls),
    response_bytes INTEGER NOT NULL CHECK (response_bytes >= 0),
    response_byte_samples INTEGER NOT NULL
        CHECK (response_byte_samples BETWEEN 0 AND calls),
    output_bytes INTEGER NOT NULL CHECK (output_bytes >= 0),
    output_byte_samples INTEGER NOT NULL
        CHECK (output_byte_samples BETWEEN 0 AND calls),
    context_bytes INTEGER NOT NULL CHECK (context_bytes >= 0),
    context_byte_samples INTEGER NOT NULL
        CHECK (context_byte_samples BETWEEN 0 AND calls),
    search_result_bytes INTEGER NOT NULL CHECK (search_result_bytes >= 0),
    search_result_byte_samples INTEGER NOT NULL
        CHECK (search_result_byte_samples BETWEEN 0 AND calls),
    context_searches INTEGER NOT NULL CHECK (context_searches >= 0),
    context_found INTEGER NOT NULL CHECK (context_found >= 0),
    context_opened INTEGER NOT NULL CHECK (context_opened >= 0),
    context_cited INTEGER NOT NULL CHECK (context_cited >= 0),
    validated_discoveries INTEGER NOT NULL CHECK (validated_discoveries >= 0),
    CHECK (
        (
            outcome = 'failure'
            AND value_class = 'not_applicable'
            AND result_action = 'not_applicable'
            AND result_count = 0
            AND citation_count = 0
            AND context_bytes = 0
            AND context_byte_samples = 0
            AND search_result_bytes = 0
            AND search_result_byte_samples = 0
            AND context_searches = 0
            AND context_found = 0
            AND context_opened = 0
            AND context_cited = 0
            AND validated_discoveries = 0
        )
        OR outcome = 'success'
    ),
    CHECK (
        (value_class = 'result_bearing' AND result_count >= calls)
        OR (
            value_class IN ('empty', 'not_applicable')
            AND result_count = 0
            AND citation_count = 0
        )
    ),
    CHECK (
        (latency_samples = 0 AND latency_ms = 0)
        OR latency_samples > 0
    ),
    CHECK (
        (context_byte_samples = 0 AND context_bytes = 0)
        OR context_byte_samples > 0
    ),
    CHECK (
        (
            surface = 'mcp'
            AND response_byte_samples = calls
            AND response_bytes > 0
            AND output_byte_samples = 0
            AND output_bytes = 0
        )
        OR (
            surface = 'cli'
            AND response_byte_samples = 0
            AND response_bytes = 0
            AND (
                (output_byte_samples = 0 AND output_bytes = 0)
                OR output_byte_samples > 0
            )
        )
    ),
    CHECK (
        (
            search_result_byte_samples = 0
            AND search_result_bytes = 0
        )
        OR search_result_byte_samples > 0
    ),
    CHECK (
        (
            operation = 'search'
            AND outcome = 'success'
            AND value_class = 'result_bearing'
            AND search_result_bytes <= context_bytes
            AND search_result_byte_samples <= context_byte_samples
        )
        OR (
            search_result_bytes = 0
            AND search_result_byte_samples = 0
        )
    ),
    CHECK (
        (
            outcome = 'success'
            AND result_action = 'search'
            AND context_searches BETWEEN 0 AND calls
            AND context_found BETWEEN 0 AND result_count
            AND (context_found = 0 OR context_searches > 0)
            AND context_opened = 0
            AND context_cited = 0
            AND validated_discoveries = 0
        )
        OR (
            outcome = 'success'
            AND result_action IN ('open_session', 'open_event')
            AND context_searches = 0
            AND context_found = 0
            AND context_opened BETWEEN 0 AND calls
            AND context_cited = 0
            AND validated_discoveries BETWEEN 0 AND calls
            AND validated_discoveries <= context_opened + context_cited
        )
        OR (
            context_searches = 0
            AND context_found = 0
            AND context_opened = 0
            AND context_cited = 0
            AND validated_discoveries = 0
        )
    ),
    CHECK (
        operation = 'blame'
        OR (
            target_type = 'not_applicable'
            AND pro_outcome = 'not_applicable'
        )
    ),
    CHECK (
        operation != 'blame'
        OR (
            target_type IN ('file', 'commit', 'pull_request')
            OR (outcome = 'failure' AND target_type = 'not_applicable')
        )
    ),
    CHECK (
        result_action = 'not_applicable'
        OR (result_action = 'search' AND operation = 'search')
        OR (
            result_action = 'open_session'
            AND (
                (surface = 'cli' AND operation = 'show')
                OR (surface = 'mcp' AND operation = 'show_session')
            )
        )
        OR (
            result_action = 'open_event'
            AND (
                (surface = 'cli' AND operation = 'show')
                OR (surface = 'mcp' AND operation = 'show_event')
            )
        )
        OR (result_action = 'locate' AND surface = 'cli' AND operation = 'locate')
        OR (result_action = 'sources' AND operation = 'sources')
        OR (result_action = 'sql' AND operation = 'sql')
        OR (result_action = 'blame' AND operation = 'blame')
    ),
    CHECK (
        value_class = 'not_applicable'
        OR result_action != 'not_applicable'
    ),
    CHECK (
        outcome = 'failure'
        OR surface = 'cli'
        OR (
            surface = 'mcp'
            AND operation IN (
                'sources', 'search', 'sql', 'show_session', 'show_event', 'blame'
            )
            AND value_class IN ('result_bearing', 'empty')
            AND result_action != 'not_applicable'
        )
        OR (
            surface = 'mcp'
            AND operation IN ('status', 'pro_status')
            AND value_class = 'not_applicable'
            AND result_action = 'not_applicable'
        )
    ),
    PRIMARY KEY (
        day_utc, definition_version, ctx_version, surface, operation, outcome,
        value_class, duration_bucket, target_type, pro_outcome, result_action
    )
) WITHOUT ROWID, STRICT;
"#;

const MAINTENANCE_SCHEMA: &str = r#"
CREATE TABLE maintenance (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    last_retention_day TEXT NOT NULL CHECK (
        length(last_retention_day) = 10
        AND last_retention_day GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
        AND date(last_retention_day) IS NOT NULL
        AND date(last_retention_day) = last_retention_day
    )
) WITHOUT ROWID, STRICT;
"#;

const EXPECTED_DAILY_COLUMNS: &[&str] = &[
    "day_utc",
    "definition_version",
    "ctx_version",
    "surface",
    "operation",
    "outcome",
    "value_class",
    "duration_bucket",
    "target_type",
    "pro_outcome",
    "result_action",
    "calls",
    "result_count",
    "citation_count",
    "latency_ms",
    "latency_samples",
    "response_bytes",
    "response_byte_samples",
    "output_bytes",
    "output_byte_samples",
    "context_bytes",
    "context_byte_samples",
    "search_result_bytes",
    "search_result_byte_samples",
    "context_searches",
    "context_found",
    "context_opened",
    "context_cited",
    "validated_discoveries",
];

const EXPECTED_DAILY_COLUMNS_V1: &[&str] = &[
    "day_utc",
    "definition_version",
    "ctx_version",
    "surface",
    "operation",
    "outcome",
    "value_class",
    "duration_bucket",
    "target_type",
    "pro_outcome",
    "calls",
    "result_count",
    "citation_count",
    "response_bytes",
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum UsageStoreError {
    #[error("usage store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("usage store SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("usage store has an unsupported application ID")]
    ApplicationId,
    #[error("usage store has unsupported schema version {0}")]
    SchemaVersion(i64),
    #[error("usage store schema does not match its declared version")]
    SchemaIdentity,
    #[error("usage store exceeds its size limit")]
    GrowthLimit,
    #[error("usage store contains inconsistent aggregates")]
    Integrity,
    #[error("usage store date is ahead of the current UTC day")]
    FutureDate,
    #[error("usage store cannot be reported without changing its SQLite file family")]
    UnsafeReadState,
}

impl UsageStoreError {
    pub(crate) const fn public_message(&self) -> &'static str {
        match self {
            Self::ApplicationId
            | Self::SchemaVersion(_)
            | Self::SchemaIdentity
            | Self::Integrity => "local usage store format is not supported",
            Self::FutureDate => "local usage store date is ahead of the current UTC day",
            Self::GrowthLimit => "local usage store exceeds its size limit",
            Self::Io(_) | Self::Sql(_) | Self::UnsafeReadState => {
                "local usage store could not be read"
            }
        }
    }
}

pub(crate) fn usage_path(data_root: &Path) -> PathBuf {
    data_root.join(USAGE_FILE)
}

pub(crate) fn usage_store_exists(data_root: &Path) -> Result<bool, UsageStoreError> {
    let path = usage_path(data_root);
    let Some(parent) = path.parent() else {
        return Err(UsageStoreError::SchemaIdentity);
    };
    match parent.symlink_metadata() {
        Ok(_) => verify_private_directory_and_owner(parent)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    match path.symlink_metadata() {
        Ok(_) => {
            let _guard = preflight_existing_family(&path, true)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            preflight_auxiliaries(&path, false)?;
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn record(
    data_root: &Path,
    operation: CompletedOperation,
) -> Result<(), UsageStoreError> {
    record_at(data_root, operation, SystemTime::now(), BUSY_TIMEOUT)
}

fn record_at(
    data_root: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
) -> Result<(), UsageStoreError> {
    record_at_with_ctx_version(data_root, operation, now, busy_timeout, CTX_VERSION)
}

fn record_at_with_ctx_version(
    data_root: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
    ctx_version: &str,
) -> Result<(), UsageStoreError> {
    let path = usage_path(data_root);
    let WritableStore {
        mut conn,
        family_guard,
    } = open_writable(&path, true, busy_timeout)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let day = utc_day(now);
    let cutoff = retention_cutoff(now);
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    reject_future_daily_dates(&transaction, &day)?;
    transaction.execute(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome, result_action,
            calls, result_count, citation_count, latency_ms, latency_samples,
            response_bytes, response_byte_samples, output_bytes, output_byte_samples,
            context_bytes, context_byte_samples, search_result_bytes,
            search_result_byte_samples, context_searches, context_found,
            context_opened, context_cited, validated_discoveries
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            1, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
        )
        ON CONFLICT (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome, result_action
        ) DO UPDATE SET
            calls = calls + 1,
            result_count = result_count + excluded.result_count,
            citation_count = citation_count + excluded.citation_count,
            latency_ms = latency_ms + excluded.latency_ms,
            latency_samples = latency_samples + excluded.latency_samples,
            response_bytes = response_bytes + excluded.response_bytes,
            response_byte_samples = response_byte_samples + excluded.response_byte_samples,
            output_bytes = output_bytes + excluded.output_bytes,
            output_byte_samples = output_byte_samples + excluded.output_byte_samples,
            context_bytes = context_bytes + excluded.context_bytes,
            context_byte_samples = context_byte_samples + excluded.context_byte_samples,
            search_result_bytes = search_result_bytes + excluded.search_result_bytes,
            search_result_byte_samples =
                search_result_byte_samples + excluded.search_result_byte_samples,
            context_searches = context_searches + excluded.context_searches,
            context_found = context_found + excluded.context_found,
            context_opened = context_opened + excluded.context_opened,
            context_cited = context_cited + excluded.context_cited,
            validated_discoveries =
                validated_discoveries + excluded.validated_discoveries
        "#,
        params![
            day,
            DEFINITION_VERSION,
            ctx_version,
            operation.surface.as_str(),
            operation.operation,
            operation.outcome.as_str(),
            operation.value_class.as_str(),
            operation.duration.as_str(),
            operation.target_type.as_str(),
            operation.pro_outcome.as_str(),
            operation
                .result_action
                .map_or("not_applicable", super::ResultObservationAction::as_str),
            operation.result_count,
            operation.citation_count,
            operation.latency_ms,
            operation.latency_samples,
            operation.response_bytes,
            operation.response_byte_samples,
            operation.output_bytes,
            operation.output_byte_samples,
            operation.context_bytes,
            operation.context_byte_samples,
            operation.search_result_bytes,
            operation.search_result_byte_samples,
            operation.context.context_searches,
            operation.context.context_found,
            operation.context.context_opened,
            operation.context.context_cited,
            operation.context.validated_discoveries,
        ],
    )?;
    let last_retention_day = transaction
        .query_row(
            "SELECT last_retention_day FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if last_retention_day.as_deref() != Some(day.as_str()) {
        transaction.execute(
            r#"
            INSERT INTO maintenance (singleton, last_retention_day)
            VALUES (1, ?1)
            ON CONFLICT (singleton) DO UPDATE SET last_retention_day = excluded.last_retention_day
            "#,
            [day.as_str()],
        )?;
        transaction.execute("DELETE FROM daily_usage WHERE day_utc < ?1", [cutoff])?;
    }
    family_guard.recheck(&path)?;
    let commit_guard = preflight_existing_family(&path, true)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    transaction.commit()?;
    drop(commit_guard);
    let _ = protect_sqlite_files(&path);
    Ok(())
}

#[cfg(test)]
pub(super) fn record_at_for_test(
    data_root: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
) -> Result<(), UsageStoreError> {
    record_at(data_root, operation, now, busy_timeout)
}

#[cfg(test)]
pub(super) fn growth_policy_for_test(
    data_root: &Path,
) -> Result<(i64, i64, i64, i64), UsageStoreError> {
    let path = usage_path(data_root);
    let opened =
        open_writable(&path, true, BUSY_TIMEOUT)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let conn = &opened.conn;
    Ok((
        conn.pragma_query_value(None, "page_size", |row| row.get(0))?,
        conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?,
        conn.pragma_query_value(None, "wal_autocheckpoint", |row| row.get(0))?,
        conn.pragma_query_value(None, "journal_size_limit", |row| row.get(0))?,
    ))
}

#[cfg(test)]
pub(super) fn fill_to_capacity_for_test(data_root: &Path) -> Result<String, UsageStoreError> {
    let path = usage_path(data_root);
    let WritableStore { mut conn, .. } =
        open_writable(&path, true, BUSY_TIMEOUT)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let day = utc_day(SystemTime::now());
    let sql = r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            result_action, calls, result_count, citation_count, latency_ms,
            latency_samples, response_bytes, response_byte_samples, output_bytes,
            output_byte_samples, context_bytes, context_byte_samples,
            search_result_bytes, search_result_byte_samples, context_searches,
            context_found, context_opened, context_cited, validated_discoveries
        ) VALUES (
            ?1, 2, ?2, 'cli', 'doctor', 'success',
            'not_applicable', 'under_10_ms', 'not_applicable',
            'not_applicable', 'not_applicable', 1, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        )
    "#;
    let mut next = 0_u64;
    loop {
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut full = None;
        for _ in 0..256 {
            let version = format!("0.26.0-cap-{next:08}");
            next += 1;
            if let Err(error) = transaction.execute(sql, params![day, version]) {
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
                    full = Some(version);
                    break;
                }
                return Err(error.into());
            }
        }
        if let Some(mut version) = full {
            // SQLITE_FULL may have already rolled the transaction back.
            drop(transaction);
            loop {
                match conn.execute(sql, params![day, version]) {
                    Ok(_) => {
                        version = format!("0.26.0-cap-{next:08}");
                        next += 1;
                    }
                    Err(error)
                        if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) =>
                    {
                        return Ok(version);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        transaction.commit()?;
    }
}

#[cfg(test)]
pub(super) fn record_with_ctx_version_for_test(
    data_root: &Path,
    operation: CompletedOperation,
    ctx_version: &str,
) -> Result<(), UsageStoreError> {
    record_at_with_ctx_version(
        data_root,
        operation,
        SystemTime::now(),
        BUSY_TIMEOUT,
        ctx_version,
    )
}

#[cfg(test)]
pub(super) fn create_mixed_v1_fixture_for_test(data_root: &Path) -> Result<(), UsageStoreError> {
    create_private_directory_all(data_root)?;
    verify_private_directory_and_owner(data_root)?;
    let path = usage_path(data_root);
    if path.exists() {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let mut conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.pragma_update(None, "page_size", PAGE_SIZE_BYTES)?;
    let day = utc_day(SystemTime::now());
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(DAILY_USAGE_SCHEMA_V1)?;
    transaction.execute_batch(MAINTENANCE_SCHEMA)?;
    let insert = r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            ?1, 1, '0.25.0-legacy', ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12
        )
    "#;
    for row in [
        (
            "cli",
            "doctor",
            "success",
            "not_applicable",
            "under_10_ms",
            "not_applicable",
            "not_applicable",
            2_i64,
            0_i64,
            0_i64,
            0_i64,
        ),
        (
            "mcp",
            "search",
            "success",
            "result_bearing",
            "50_to_249_ms",
            "not_applicable",
            "not_applicable",
            3,
            6,
            0,
            900,
        ),
        (
            "mcp",
            "show_session",
            "success",
            "result_bearing",
            "10_to_49_ms",
            "not_applicable",
            "not_applicable",
            1,
            2,
            0,
            300,
        ),
        (
            "cli",
            "blame",
            "success",
            "result_bearing",
            "250_to_999_ms",
            "commit",
            "produced",
            1,
            1,
            1,
            0,
        ),
        (
            "mcp",
            "blame",
            "success",
            "empty",
            "250_to_999_ms",
            "file",
            "possible",
            1,
            0,
            0,
            200,
        ),
        (
            "mcp",
            "search",
            "failure",
            "not_applicable",
            "10_to_49_ms",
            "not_applicable",
            "not_applicable",
            1,
            0,
            0,
            100,
        ),
    ] {
        transaction.execute(
            insert,
            params![
                day, row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO maintenance(singleton, last_retention_day) VALUES (1, ?1)",
        [day],
    )?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
    transaction.commit()?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    drop(conn);
    protect_sqlite_files(&path)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn fail_v1_migration_before_commit_for_test(
    data_root: &Path,
) -> Result<(), UsageStoreError> {
    let path = usage_path(data_root);
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_transient(&conn, BUSY_TIMEOUT)?;
    migrate_to_current(&mut conn, || Err::<(), _>(UsageStoreError::Integrity))
}

pub(crate) struct ReadOnlyStore {
    conn: Connection,
    family_guard: FamilyGuard,
    path: PathBuf,
}

impl ReadOnlyStore {
    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), UsageStoreError> {
        self.family_guard.recheck_unchanged(&self.path)
    }
}

pub(crate) fn open_read_only(path: &Path) -> Result<ReadOnlyStore, UsageStoreError> {
    let guard = preflight_existing_family(path, true)?;
    if guard.has_nonempty_auxiliary()? {
        return Err(UsageStoreError::UnsafeReadState);
    }
    let image = capture_checkpointed_image(path, &guard, || {})?;
    let mut conn = deserialize_read_only(image)?;
    migrate_to_current(&mut conn, || Ok(()))?;
    configure_report_connection(&conn)?;
    guard.recheck_unchanged(path)?;
    Ok(ReadOnlyStore {
        conn,
        family_guard: guard,
        path: path.to_path_buf(),
    })
}

pub(crate) fn reset(data_root: &Path) -> Result<bool, UsageStoreError> {
    reset_with_post_commit(data_root, |_| ())
}

fn reset_with_post_commit<T>(
    data_root: &Path,
    after_commit: impl FnOnce(&Path) -> T,
) -> Result<bool, UsageStoreError> {
    let path = usage_path(data_root);
    let Some(WritableStore {
        mut conn,
        family_guard,
    }) = open_writable(&path, false, BUSY_TIMEOUT)?
    else {
        return Ok(false);
    };
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    transaction.execute("DELETE FROM daily_usage", [])?;
    transaction.execute("DELETE FROM maintenance", [])?;
    family_guard.recheck(&path)?;
    let commit_guard = preflight_existing_family(&path, true)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    transaction.commit()?;
    drop(commit_guard);
    let _post_commit_guard = after_commit(&path);
    // Reset promises logical deletion, not forensic erasure. Truncate the WAL
    // when no reader prevents it, but do not turn a completed logical reset
    // into an error if this best-effort checkpoint is busy.
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    let _ = protect_sqlite_files(&path);
    Ok(true)
}

#[cfg(test)]
pub(super) fn reset_with_post_commit_for_test<T>(
    data_root: &Path,
    after_commit: impl FnOnce(&Path) -> T,
) -> Result<bool, UsageStoreError> {
    reset_with_post_commit(data_root, after_commit)
}

enum PreparedFile {
    Missing,
    NewInitialized(FamilyGuard),
    Existing(FamilyGuard),
}

struct WritableStore {
    conn: Connection,
    family_guard: FamilyGuard,
}

fn open_writable(
    path: &Path,
    create: bool,
    busy_timeout: Duration,
) -> Result<Option<WritableStore>, UsageStoreError> {
    let prepared = prepare_file(path, create)?;
    let newly_created = matches!(prepared, PreparedFile::NewInitialized(_));
    let guard = match prepared {
        PreparedFile::Missing => return Ok(None),
        PreparedFile::NewInitialized(guard) | PreparedFile::Existing(guard) => guard,
    };
    if !newly_created {
        // A nonempty WAL may contain changes absent from the main image, while
        // a nonempty SHM cannot be proven source-stable portably. Reject either
        // from retained native handles before SQLite can open the source
        // pathname and checkpoint or remove any family member.
        if guard.has_nonempty_auxiliary()? {
            return Err(UsageStoreError::UnsafeReadState);
        }
        let image = capture_checkpointed_image(path, &guard, || {})?;
        let detached = deserialize_read_only(image)?;
        let schema_version = verify_supported_schema(&detached)?;
        super::report::validate_rows_for_schema(&detached, schema_version)?;
        drop(detached);
        guard.recheck_unchanged(path)?;
        cleanup_stale_initializer_slots(path, SystemTime::now())?;
        guard.recheck_unchanged(path)?;
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if newly_created {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut conn = Connection::open_with_flags(path, flags)?;
    verify_same_file(path, guard.main_file())?;
    verify_single_link(guard.main_file())?;
    let schema_version = verify_supported_schema(&conn)?;
    super::report::validate_rows_for_schema(&conn, schema_version)?;
    configure_transient(&conn, busy_timeout)?;
    if schema_version == LEGACY_SCHEMA_VERSION {
        // A quiescent v1 store can have a WAL-mode main header without
        // auxiliaries. Opening it natively creates fresh WAL/SHM files, which
        // cannot be part of the pre-open family guard. Return to rollback
        // journal mode before migration so the guarded family is main-only
        // again; v2 persistent configuration restores WAL after commit.
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if journal_mode != "wal" {
            return Err(UsageStoreError::SchemaIdentity);
        }
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if journal_mode != "delete" {
            return Err(UsageStoreError::UnsafeReadState);
        }
        guard.recheck(&path)?;
    }
    migrate_to_current(&mut conn, || {
        guard.recheck(path)?;
        preflight_existing_family(path, true)
    })?;
    configure_persistent(&conn)?;
    verify_schema(&conn)?;
    super::report::validate_rows(&conn)?;
    drop(guard);
    protect_sqlite_files(path)?;
    let family_guard = preflight_existing_family(path, true)?;
    Ok(Some(WritableStore { conn, family_guard }))
}

fn configure_persistent(conn: &Connection) -> Result<(), UsageStoreError> {
    conn.pragma_update(None, "max_page_count", MAX_PAGE_COUNT)?;
    let max_page_count: i64 = conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
    if max_page_count > MAX_PAGE_COUNT {
        return Err(UsageStoreError::GrowthLimit);
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    Ok(())
}

fn prepare_file(path: &Path, create: bool) -> Result<PreparedFile, UsageStoreError> {
    let Some(parent) = path.parent() else {
        return Err(UsageStoreError::SchemaIdentity);
    };
    match parent.symlink_metadata() {
        Ok(_) => verify_private_directory_and_owner(parent)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            create_private_directory_all(parent)?;
            verify_private_directory_and_owner(parent)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PreparedFile::Missing);
        }
        Err(error) => return Err(error.into()),
    }
    match path.symlink_metadata() {
        Ok(_) => {
            return Ok(PreparedFile::Existing(preflight_existing_family(
                path, true,
            )?));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    preflight_auxiliaries(path, false)?;
    if !create {
        return Ok(PreparedFile::Missing);
    }
    cleanup_stale_initializer_slots(path, SystemTime::now())?;
    initialize_and_publish(path)
}

static INIT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct RemoveTemporary(PathBuf);

impl Drop for RemoveTemporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn initialize_and_publish(path: &Path) -> Result<PreparedFile, UsageStoreError> {
    let start = usize::try_from(
        INIT_SEQUENCE.fetch_add(1, Ordering::Relaxed) % u64::try_from(INIT_SLOT_COUNT).unwrap_or(1),
    )
    .unwrap_or(0);
    let (temporary, file) = (0..INIT_SLOT_COUNT)
        .find_map(|offset| {
            let slot = (start + offset) % INIT_SLOT_COUNT;
            let temporary = initializer_slot_path(path, slot);
            match create_initializer_slot(&temporary) {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(UsageStoreError::Io(error))),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            UsageStoreError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "local usage initialization slots are busy",
            ))
        })?;
    let _cleanup = RemoveTemporary(temporary.clone());
    initialize_slot(path, temporary, file)
}

fn create_initializer_slot(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn initialize_slot(
    path: &Path,
    temporary: PathBuf,
    file: File,
) -> Result<PreparedFile, UsageStoreError> {
    restrict_private_file_handle(&file)?;
    verify_file_owner(&file)?;
    verify_single_link(&file)?;
    let mut conn = Connection::open_with_flags(
        &temporary,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    verify_same_file(&temporary, &file)?;
    configure_transient(&conn, BUSY_TIMEOUT)?;
    conn.pragma_update(None, "page_size", PAGE_SIZE_BYTES)?;
    initialize_schema(&mut conn)?;
    conn.pragma_update(None, "max_page_count", MAX_PAGE_COUNT)?;
    verify_schema(&conn)?;
    drop(conn);
    restrict_private_file_handle(&file)?;

    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            fs::remove_file(&temporary)?;
            let published = reopen_same_file(path, &file)?;
            verify_single_link(&published)?;
            Ok(PreparedFile::NewInitialized(FamilyGuard::main_only(
                published,
            )?))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temporary)?;
            let existing = preflight_existing_family(path, true)?;
            Ok(PreparedFile::Existing(existing))
        }
        Err(error) => Err(error.into()),
    }
}

fn initializer_slot_path(path: &Path, slot: usize) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(format!(".init-{slot}"));
    PathBuf::from(value)
}

fn cleanup_stale_initializer_slots(path: &Path, now: SystemTime) -> Result<usize, UsageStoreError> {
    let mut removed = 0;
    for slot in 0..INIT_SLOT_COUNT {
        let candidate = initializer_slot_path(path, slot);
        if verify_private_file(&candidate).is_err() {
            continue;
        }
        let metadata = match candidate.symlink_metadata() {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && verify_metadata_owner(&metadata).is_ok() =>
            {
                metadata
            }
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let candidate_handle = match open_nofollow(&candidate, true) {
            Ok(file)
                if verify_same_file(&candidate, &file).is_ok()
                    && verify_file_owner(&file).is_ok()
                    && verify_single_link(&file).is_ok() =>
            {
                file
            }
            Ok(_) | Err(_) => continue,
        };
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_INIT_AGE);
        if stale {
            verify_same_file(&candidate, &candidate_handle)?;
            // Windows pathname deletion cannot proceed while the hardened
            // no-delete-sharing handle is retained.
            #[cfg(windows)]
            drop(candidate_handle);
            fs::remove_file(candidate)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn configure_transient(conn: &Connection, busy_timeout: Duration) -> Result<(), UsageStoreError> {
    conn.busy_timeout(busy_timeout)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    Ok(())
}

fn configure_report_connection(conn: &Connection) -> Result<(), UsageStoreError> {
    configure_transient(conn, BUSY_TIMEOUT)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(())
}

fn initialize_schema(conn: &mut Connection) -> Result<(), UsageStoreError> {
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id != 0 || user_version != 0 || !database_is_empty(conn)? {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(DAILY_USAGE_SCHEMA)?;
    transaction.execute_batch(MAINTENANCE_SCHEMA)?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn migrate_to_current<T>(
    conn: &mut Connection,
    before_commit: impl FnOnce() -> Result<T, UsageStoreError>,
) -> Result<(), UsageStoreError> {
    match verify_supported_schema(conn)? {
        SCHEMA_VERSION => return Ok(()),
        LEGACY_SCHEMA_VERSION => {}
        version => return Err(UsageStoreError::SchemaVersion(version)),
    }
    super::report::validate_rows_for_schema(conn, LEGACY_SCHEMA_VERSION)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch("ALTER TABLE daily_usage RENAME TO daily_usage_v1;")?;
    transaction.execute_batch(DAILY_USAGE_SCHEMA)?;
    transaction.execute_batch(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome, result_action,
            calls, result_count, citation_count, latency_ms, latency_samples,
            response_bytes, response_byte_samples, output_bytes, output_byte_samples,
            context_bytes, context_byte_samples, search_result_bytes,
            search_result_byte_samples, context_searches, context_found,
            context_opened, context_cited, validated_discoveries
        )
        SELECT
            day_utc,
            2,
            ctx_version,
            surface,
            operation,
            outcome,
            value_class,
            duration_bucket,
            target_type,
            pro_outcome,
            CASE
                WHEN outcome != 'success' OR value_class = 'not_applicable'
                    THEN 'not_applicable'
                WHEN operation = 'search' THEN 'search'
                WHEN surface = 'mcp' AND operation = 'show_session' THEN 'open_session'
                WHEN surface = 'mcp' AND operation = 'show_event' THEN 'open_event'
                WHEN operation = 'sources' THEN 'sources'
                WHEN operation = 'sql' THEN 'sql'
                WHEN operation = 'blame' THEN 'blame'
                ELSE 'not_applicable'
            END,
            calls,
            result_count,
            citation_count,
            0,
            0,
            response_bytes,
            CASE WHEN surface = 'mcp' THEN calls ELSE 0 END,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0
        FROM daily_usage_v1;
        DROP TABLE daily_usage_v1;
        "#,
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    let commit_guard = before_commit()?;
    transaction.commit()?;
    drop(commit_guard);
    verify_schema(conn)?;
    super::report::validate_rows(conn)?;
    Ok(())
}

pub(super) fn verify_supported_schema(conn: &Connection) -> Result<i64, UsageStoreError> {
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
    if page_size != PAGE_SIZE_BYTES {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |row| row.get(0))?;
    if page_size.saturating_mul(page_count) > MAX_DATABASE_BYTES {
        return Err(UsageStoreError::GrowthLimit);
    }
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(UsageStoreError::ApplicationId);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    verify_schema_object_allowlist(conn)?;
    match user_version {
        LEGACY_SCHEMA_VERSION => {
            verify_daily_schema(conn, DAILY_USAGE_SCHEMA_V1, EXPECTED_DAILY_COLUMNS_V1)?
        }
        SCHEMA_VERSION => verify_daily_schema(conn, DAILY_USAGE_SCHEMA, EXPECTED_DAILY_COLUMNS)?,
        _ => return Err(UsageStoreError::SchemaVersion(user_version)),
    }
    verify_table_schema(conn, "maintenance", MAINTENANCE_SCHEMA)?;
    Ok(user_version)
}

pub(super) fn verify_schema(conn: &Connection) -> Result<(), UsageStoreError> {
    let version = verify_supported_schema(conn)?;
    if version != SCHEMA_VERSION {
        return Err(UsageStoreError::SchemaVersion(version));
    }
    Ok(())
}

fn verify_daily_schema(
    conn: &Connection,
    expected_schema: &str,
    expected_columns: &[&str],
) -> Result<(), UsageStoreError> {
    let mut statement = conn.prepare("PRAGMA table_info(daily_usage)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns
        .iter()
        .map(String::as_str)
        .ne(expected_columns.iter().copied())
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    verify_table_schema(conn, "daily_usage", expected_schema)?;
    Ok(())
}

fn verify_schema_object_allowlist(conn: &Connection) -> Result<(), UsageStoreError> {
    let mut statement =
        conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name")?;
    let objects = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut daily_usage = false;
    let mut maintenance = false;
    for object in objects {
        let (kind, name, table, sql) = object?;
        match (kind.as_str(), name.as_str(), table.as_str(), sql.as_deref()) {
            ("table", "daily_usage", "daily_usage", Some(_)) if !daily_usage => {
                daily_usage = true;
            }
            ("table", "maintenance", "maintenance", Some(_)) if !maintenance => {
                maintenance = true;
            }
            // SQLite may own implicit indexes. WITHOUT ROWID currently needs
            // none, but permit only SQLite-internal, SQL-less indexes attached
            // to one of the two exact tables.
            ("index", name, "daily_usage" | "maintenance", None) if name.starts_with("sqlite_") => {
            }
            _ => return Err(UsageStoreError::SchemaIdentity),
        }
    }
    if !daily_usage || !maintenance {
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(())
}

fn verify_table_schema(
    conn: &Connection,
    table: &str,
    expected: &str,
) -> Result<(), UsageStoreError> {
    let actual = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(UsageStoreError::SchemaIdentity)?;
    if canonical_schema(&actual) != canonical_schema(expected) {
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(())
}

fn canonical_schema(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(';')
        .to_owned()
}

fn database_is_empty(conn: &Connection) -> Result<bool, UsageStoreError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_none())
}

fn reject_future_dates(conn: &Connection, day: &str) -> Result<(), UsageStoreError> {
    let latest: Option<String> = conn.query_row(
        r#"
        SELECT MAX(value) FROM (
            SELECT MAX(day_utc) AS value FROM daily_usage
            UNION ALL
            SELECT MAX(last_retention_day) AS value FROM maintenance
        )
        "#,
        [],
        |row| row.get(0),
    )?;
    if latest.as_deref().is_some_and(|latest| latest > day) {
        return Err(UsageStoreError::FutureDate);
    }
    Ok(())
}

fn reject_future_daily_dates(conn: &Connection, day: &str) -> Result<(), UsageStoreError> {
    let latest: Option<String> =
        conn.query_row("SELECT MAX(day_utc) FROM daily_usage", [], |row| row.get(0))?;
    if latest.as_deref().is_some_and(|latest| latest > day) {
        return Err(UsageStoreError::FutureDate);
    }
    Ok(())
}

pub(crate) fn verify_report_dates(
    conn: &Connection,
    now: SystemTime,
) -> Result<(), UsageStoreError> {
    reject_future_dates(conn, &utc_day(now))
}

fn utc_day(now: SystemTime) -> String {
    let now: DateTime<Utc> = now.into();
    now.date_naive().format("%Y-%m-%d").to_string()
}

fn retention_cutoff(now: SystemTime) -> String {
    let now: DateTime<Utc> = now.into();
    let retained_prior_days = u64::try_from(RETENTION_DAYS.saturating_sub(1)).unwrap_or(0);
    now.date_naive()
        .checked_sub_days(Days::new(retained_prior_days))
        .unwrap_or(now.date_naive())
        .format("%Y-%m-%d")
        .to_string()
}

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{
    utc_day, UsageStoreError, APPLICATION_ID, LEGACY_SCHEMA_VERSION, MAX_DATABASE_BYTES,
    PAGE_SIZE_BYTES, SCHEMA_VERSION,
};

pub(super) const DAILY_USAGE_SCHEMA_V1: &str = r#"
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
        operation != 'blame'
        OR outcome = 'failure'
        OR (
            (pro_outcome NOT IN ('produced', 'possible') OR value_class = 'result_bearing')
            AND (value_class != 'empty' OR pro_outcome = 'none')
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

const BLAME_VALUE_CLASS_CHECK: &str = r#"
    CHECK (
        operation != 'blame'
        OR outcome = 'failure'
        OR (
            (pro_outcome NOT IN ('produced', 'possible') OR value_class = 'result_bearing')
            AND (value_class != 'empty' OR pro_outcome = 'none')
        )
    ),
"#;

pub(super) fn legacy_daily_usage_schema_v1() -> String {
    DAILY_USAGE_SCHEMA_V1.replacen(BLAME_VALUE_CLASS_CHECK, "", 1)
}

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
        operation != 'blame'
        OR outcome = 'failure'
        OR (
            (pro_outcome NOT IN ('produced', 'possible') OR value_class = 'result_bearing')
            AND (value_class != 'empty' OR pro_outcome = 'none')
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

pub(super) const LEGACY_MAINTENANCE_SCHEMA: &str = r#"
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

pub(super) const MAINTENANCE_SCHEMA: &str = r#"
CREATE TABLE maintenance (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    last_retention_day TEXT NOT NULL CHECK (
        length(last_retention_day) = 10
        AND last_retention_day GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
        AND date(last_retention_day) IS NOT NULL
        AND date(last_retention_day) = last_retention_day
    ),
    store_generation INTEGER NOT NULL CHECK (store_generation >= 0)
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

pub(super) fn initialize_schema(conn: &mut Connection) -> Result<(), UsageStoreError> {
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

pub(super) fn migrate_to_current<T>(
    conn: &mut Connection,
    before_commit: impl FnOnce() -> Result<T, UsageStoreError>,
) -> Result<(), UsageStoreError> {
    let version = verify_supported_schema(conn)?;
    let maintenance_is_current = maintenance_schema_is_current(conn)?;
    if version == SCHEMA_VERSION && maintenance_is_current {
        return Ok(());
    }
    if !matches!(version, LEGACY_SCHEMA_VERSION | SCHEMA_VERSION) {
        return Err(UsageStoreError::SchemaVersion(version));
    }
    super::super::report::validate_rows_for_schema(conn, version)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if version == LEGACY_SCHEMA_VERSION {
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
        WITH normalized AS (
            SELECT
                day_utc,
                ctx_version,
                surface,
                operation,
                outcome,
                value_class,
                duration_bucket,
                target_type,
                CASE
                    WHEN operation = 'blame'
                        AND pro_outcome IN ('produced', 'possible')
                        AND value_class != 'result_bearing'
                        THEN 'none'
                    ELSE pro_outcome
                END AS normalized_pro_outcome,
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
                END AS normalized_result_action,
                calls,
                result_count,
                citation_count,
                response_bytes
            FROM daily_usage_v1
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
            normalized_pro_outcome,
            normalized_result_action,
            SUM(calls),
            SUM(result_count),
            SUM(citation_count),
            0,
            0,
            SUM(response_bytes),
            CASE WHEN surface = 'mcp' THEN SUM(calls) ELSE 0 END,
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
        FROM normalized
        GROUP BY
            day_utc,
            ctx_version,
            surface,
            operation,
            outcome,
            value_class,
            duration_bucket,
            target_type,
            normalized_pro_outcome,
            normalized_result_action;
        DROP TABLE daily_usage_v1;
        "#,
        )?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    if !maintenance_is_current {
        transaction
            .execute_batch("ALTER TABLE maintenance RENAME TO maintenance_without_generation;")?;
        transaction.execute_batch(MAINTENANCE_SCHEMA)?;
        transaction.execute_batch(
            r#"
            INSERT INTO maintenance (singleton, last_retention_day, store_generation)
            SELECT singleton, last_retention_day, 0
            FROM maintenance_without_generation;
            DROP TABLE maintenance_without_generation;
            "#,
        )?;
    }
    verify_schema(&transaction)?;
    super::super::report::validate_rows(&transaction)?;
    let commit_guard = before_commit()?;
    transaction.commit()?;
    drop(commit_guard);
    verify_schema(conn)?;
    super::super::report::validate_rows(conn)?;
    Ok(())
}

pub(in crate::local_usage) fn verify_supported_schema(
    conn: &Connection,
) -> Result<i64, UsageStoreError> {
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
        LEGACY_SCHEMA_VERSION => verify_daily_schema_v1(conn)?,
        SCHEMA_VERSION => verify_daily_schema(conn, DAILY_USAGE_SCHEMA, EXPECTED_DAILY_COLUMNS)?,
        _ => return Err(UsageStoreError::SchemaVersion(user_version)),
    }
    maintenance_schema_is_current(conn)?;
    Ok(user_version)
}

pub(super) fn verify_schema(conn: &Connection) -> Result<(), UsageStoreError> {
    let version = verify_supported_schema(conn)?;
    if version != SCHEMA_VERSION {
        return Err(UsageStoreError::SchemaVersion(version));
    }
    if !maintenance_schema_is_current(conn)? {
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(())
}

fn maintenance_schema_is_current(conn: &Connection) -> Result<bool, UsageStoreError> {
    let actual = table_schema(conn, "maintenance")?;
    if canonical_schema(&actual) == canonical_schema(MAINTENANCE_SCHEMA) {
        return Ok(true);
    }
    if canonical_schema(&actual) == canonical_schema(LEGACY_MAINTENANCE_SCHEMA) {
        return Ok(false);
    }
    Err(UsageStoreError::SchemaIdentity)
}

fn verify_daily_schema_v1(conn: &Connection) -> Result<(), UsageStoreError> {
    let legacy_schema = legacy_daily_usage_schema_v1();
    verify_daily_schema_variants(
        conn,
        &[DAILY_USAGE_SCHEMA_V1, legacy_schema.as_str()],
        EXPECTED_DAILY_COLUMNS_V1,
    )
}

fn verify_daily_schema(
    conn: &Connection,
    expected_schema: &str,
    expected_columns: &[&str],
) -> Result<(), UsageStoreError> {
    verify_daily_schema_variants(conn, &[expected_schema], expected_columns)
}

fn verify_daily_schema_variants(
    conn: &Connection,
    expected_schemas: &[&str],
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
    let actual = table_schema(conn, "daily_usage")?;
    if !expected_schemas
        .iter()
        .any(|expected| canonical_schema(&actual) == canonical_schema(expected))
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(())
}

pub(in crate::local_usage) fn v1_uses_legacy_blame_schema(
    conn: &Connection,
) -> Result<bool, UsageStoreError> {
    let actual = table_schema(conn, "daily_usage")?;
    Ok(canonical_schema(&actual) == canonical_schema(&legacy_daily_usage_schema_v1()))
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

fn table_schema(conn: &Connection, table: &str) -> Result<String, UsageStoreError> {
    conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .ok_or(UsageStoreError::SchemaIdentity)
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

pub(super) fn reject_future_daily_dates(
    conn: &Connection,
    day: &str,
) -> Result<(), UsageStoreError> {
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

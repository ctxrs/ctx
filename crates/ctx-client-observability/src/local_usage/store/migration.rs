use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{
    utc_day, UsageStoreError, APPLICATION_ID, LEGACY_SCHEMA_VERSION, MAX_DATABASE_BYTES,
    PAGE_SIZE_BYTES, PREVIOUS_SCHEMA_VERSION, PRIOR_SCHEMA_VERSION, RELEASED_SCHEMA_VERSION,
    SCHEMA_VERSION,
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

const V1_LATE_ADDITIONAL_VALUE_CHECK: &str = r#"
    CHECK (
        operation != 'blame'
        OR outcome = 'failure'
        OR (
            (pro_outcome NOT IN ('produced', 'possible') OR value_class = 'result_bearing')
            AND (value_class != 'empty' OR pro_outcome = 'none')
        )
    ),
"#;

pub(super) fn released_daily_usage_schema_v1_initial() -> String {
    DAILY_USAGE_SCHEMA_V1.replacen(V1_LATE_ADDITIONAL_VALUE_CHECK, "", 1)
}

pub(super) const DAILY_USAGE_SCHEMA_V2: &str = r#"
CREATE TABLE daily_usage (
    day_utc TEXT NOT NULL
        CHECK (
            length(day_utc) = 10
            AND day_utc GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(day_utc) IS NOT NULL
            AND date(day_utc) = day_utc
        ),
    definition_version INTEGER NOT NULL CHECK (definition_version IN (1, 2)),
    ctx_version TEXT NOT NULL
        CHECK (
            length(ctx_version) BETWEEN 1 AND 64
            AND ctx_version NOT GLOB '*[^0-9A-Za-z.+-]*'
        ),
    surface TEXT NOT NULL CHECK (surface IN ('cli', 'mcp')),
    operation TEXT NOT NULL CHECK (
        (
            definition_version = 1
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show',
                'locate', 'search', 'pro_setup', 'pro_manage', 'pro_uninstall',
                'blame', 'sql', 'docs', 'integrations', 'daemon_status',
                'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            definition_version = 2
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show_session',
                'show_event', 'locate', 'search', 'pro_setup', 'pro_manage',
                'pro_uninstall', 'blame', 'sql', 'docs', 'integrations',
                'daemon_status', 'daemon_enable', 'daemon_disable', 'upgrade',
                'doctor'
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
    context_coverage TEXT NOT NULL
        CHECK (context_coverage IN ('complete', 'unavailable', 'not_applicable')),
    calls INTEGER NOT NULL CHECK (calls > 0),
    result_count INTEGER NOT NULL CHECK (result_count >= 0),
    citation_count INTEGER NOT NULL CHECK (citation_count >= 0),
    delivered_output_bytes INTEGER NOT NULL CHECK (delivered_output_bytes >= 0),
    delivered_context_bytes INTEGER NOT NULL CHECK (delivered_context_bytes >= 0),
    matched_normalized_session_bytes INTEGER NOT NULL
        CHECK (matched_normalized_session_bytes >= 0),
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
        citation_count = 0
        OR (operation = 'blame' AND outcome = 'success')
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
        definition_version = 1
        OR operation != 'blame'
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
                (
                    definition_version = 1
                    AND (
                        (operation = 'blame' AND value_class IN ('result_bearing', 'empty'))
                        OR (operation != 'blame' AND value_class = 'not_applicable')
                    )
                )
                OR (
                    definition_version = 2
                    AND (
                        (
                            operation IN ('search', 'blame')
                            AND value_class IN ('result_bearing', 'empty')
                        )
                        OR (
                            operation NOT IN ('search', 'blame')
                            AND value_class = 'not_applicable'
                        )
                    )
                )
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
    CHECK (
        (
            definition_version = 2
            AND operation = 'search'
            AND outcome = 'success'
            AND value_class = 'result_bearing'
            AND (
                (
                    context_coverage = 'complete'
                    AND delivered_context_bytes > 0
                    AND matched_normalized_session_bytes > 0
                    AND matched_normalized_session_bytes >= delivered_context_bytes
                )
                OR (
                    context_coverage = 'unavailable'
                    AND delivered_context_bytes = 0
                    AND matched_normalized_session_bytes = 0
                )
            )
        )
        OR (
            context_coverage = 'not_applicable'
            AND delivered_context_bytes = 0
            AND matched_normalized_session_bytes = 0
        )
    ),
    PRIMARY KEY (
        day_utc, definition_version, ctx_version, surface, operation, outcome,
        value_class, duration_bucket, target_type, pro_outcome, context_coverage
    )
) WITHOUT ROWID, STRICT;
"#;

const V2_DELIVERED_OUTPUT_BYTES_COLUMN: &str =
    "    delivered_output_bytes INTEGER NOT NULL CHECK (delivered_output_bytes >= 0),";

const CURRENT_DELIVERED_OUTPUT_BYTES_COLUMN: &str = r#"    delivered_output_bytes INTEGER NOT NULL
        CHECK (
            delivered_output_bytes >= 0
            AND (
                (
                    definition_version = 1
                    AND (
                        (surface = 'cli' AND delivered_output_bytes = 0)
                        OR (surface = 'mcp' AND delivered_output_bytes > 0)
                    )
                )
                OR (
                    definition_version = 2
                    AND (
                        delivered_output_bytes > 0
                        OR (surface = 'cli' AND outcome = 'failure')
                    )
                )
            )
        ),"#;

fn daily_usage_schema_v3() -> String {
    DAILY_USAGE_SCHEMA_V2.replacen(
        V2_DELIVERED_OUTPUT_BYTES_COLUMN,
        CURRENT_DELIVERED_OUTPUT_BYTES_COLUMN,
        1,
    )
}

pub(super) const DAILY_USAGE_SCHEMA_V4: &str = r#"
CREATE TABLE daily_usage (
    day_utc TEXT NOT NULL
        CHECK (
            length(day_utc) = 10
            AND day_utc GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(day_utc) IS NOT NULL
            AND date(day_utc) = day_utc
        ),
    definition_version INTEGER NOT NULL CHECK (definition_version IN (1, 2)),
    ctx_version TEXT NOT NULL
        CHECK (
            length(ctx_version) BETWEEN 1 AND 64
            AND ctx_version NOT GLOB '*[^0-9A-Za-z.+-]*'
        ),
    surface TEXT NOT NULL CHECK (surface IN ('cli', 'mcp')),
    operation TEXT NOT NULL CHECK (
        (
            definition_version = 1
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show', 'locate',
                'search', 'docs', 'integrations', 'daemon_status',
                'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            definition_version = 2
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show_session',
                'show_event', 'locate', 'search', 'docs',
                'integrations', 'daemon_status', 'daemon_enable',
                'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            surface = 'mcp'
            AND operation IN (
                'status', 'sources', 'search', 'show_session', 'show_event'
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
    context_coverage TEXT NOT NULL
        CHECK (context_coverage IN ('complete', 'unavailable', 'not_applicable')),
    calls INTEGER NOT NULL CHECK (calls > 0),
    result_count INTEGER NOT NULL CHECK (result_count >= 0),
    delivered_output_bytes INTEGER NOT NULL
        CHECK (
            delivered_output_bytes >= 0
            AND (
                (
                    definition_version = 1
                    AND (
                        (surface = 'cli' AND delivered_output_bytes = 0)
                        OR (surface = 'mcp' AND delivered_output_bytes > 0)
                    )
                )
                OR (
                    definition_version = 2
                    AND (
                        delivered_output_bytes > 0
                        OR (surface = 'cli' AND outcome = 'failure')
                    )
                )
            )
        ),
    delivered_context_bytes INTEGER NOT NULL CHECK (delivered_context_bytes >= 0),
    matched_normalized_session_bytes INTEGER NOT NULL
        CHECK (matched_normalized_session_bytes >= 0),
    CHECK (
        (
            outcome = 'failure'
            AND value_class = 'not_applicable'
            AND result_count = 0
        )
        OR outcome = 'success'
    ),
    CHECK (
        (value_class = 'result_bearing' AND result_count >= calls)
        OR (value_class IN ('empty', 'not_applicable') AND result_count = 0)
    ),
    CHECK (
        outcome = 'failure'
        OR (
            surface = 'cli'
            AND (
                (definition_version = 2 AND operation = 'search'
                    AND value_class IN ('result_bearing', 'empty'))
                OR ((definition_version != 2 OR operation != 'search')
                    AND value_class = 'not_applicable')
            )
        )
        OR (
            surface = 'mcp'
            AND (
                (operation IN ('sources', 'search', 'show_session', 'show_event')
                    AND value_class IN ('result_bearing', 'empty'))
                OR (operation = 'status' AND value_class = 'not_applicable')
            )
        )
    ),
    CHECK (
        (
            definition_version = 2
            AND operation = 'search'
            AND outcome = 'success'
            AND value_class = 'result_bearing'
            AND (
                (context_coverage = 'complete'
                    AND delivered_context_bytes > 0
                    AND matched_normalized_session_bytes >= delivered_context_bytes)
                OR (context_coverage = 'unavailable'
                    AND delivered_context_bytes = 0
                    AND matched_normalized_session_bytes = 0)
            )
        )
        OR (context_coverage = 'not_applicable'
            AND delivered_context_bytes = 0
            AND matched_normalized_session_bytes = 0)
    ),
    PRIMARY KEY (
        day_utc, definition_version, ctx_version, surface, operation, outcome,
        value_class, duration_bucket, context_coverage
    )
) WITHOUT ROWID, STRICT;
"#;

pub(super) const DAILY_USAGE_SCHEMA: &str = include_str!("schema_v5.sql");

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

pub(super) const MAINTENANCE_SCHEMA: &str = LEGACY_MAINTENANCE_SCHEMA;

const EXPECTED_PREDECESSOR_DAILY_COLUMNS: &[&str] = &[
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
    "context_coverage",
    "calls",
    "result_count",
    "citation_count",
    "delivered_output_bytes",
    "delivered_context_bytes",
    "matched_normalized_session_bytes",
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

const EXPECTED_CURRENT_DAILY_COLUMNS: &[&str] = &[
    "day_utc",
    "definition_version",
    "ctx_version",
    "surface",
    "operation",
    "outcome",
    "value_class",
    "duration_bucket",
    "context_coverage",
    "calls",
    "result_count",
    "delivered_output_bytes",
    "delivered_context_bytes",
    "matched_normalized_session_bytes",
];

#[cfg(test)]
pub(super) fn released_daily_usage_schema(version: i64) -> Result<String, UsageStoreError> {
    match version {
        LEGACY_SCHEMA_VERSION => Ok(DAILY_USAGE_SCHEMA_V1.to_owned()),
        PREVIOUS_SCHEMA_VERSION => Ok(DAILY_USAGE_SCHEMA_V2.to_owned()),
        RELEASED_SCHEMA_VERSION => Ok(daily_usage_schema_v3()),
        PRIOR_SCHEMA_VERSION => Ok(DAILY_USAGE_SCHEMA_V4.to_owned()),
        _ => Err(UsageStoreError::SchemaVersion(version)),
    }
}

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
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    super::super::report::validate_rows_for_schema(conn, version)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch("ALTER TABLE daily_usage RENAME TO daily_usage_previous;")?;
    transaction.execute_batch(DAILY_USAGE_SCHEMA)?;
    if version == LEGACY_SCHEMA_VERSION {
        transaction.execute_batch(
            r#"
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, context_coverage,
                calls, result_count, delivered_output_bytes,
                delivered_context_bytes, matched_normalized_session_bytes
            )
            SELECT day_utc, 1, ctx_version, surface, operation, outcome,
                value_class, duration_bucket, 'not_applicable', SUM(calls),
                SUM(result_count),
                SUM(CASE WHEN surface = 'mcp' THEN response_bytes ELSE 0 END),
                0, 0
            FROM daily_usage_previous
            WHERE
                (surface = 'cli' AND operation IN (
                    'setup', 'index', 'sources', 'import', 'show', 'locate',
                    'search', 'docs', 'integrations', 'daemon_status',
                    'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
                ))
                OR (surface = 'mcp' AND operation IN (
                    'status', 'sources', 'search', 'show_session', 'show_event'
                ))
            GROUP BY day_utc, ctx_version, surface, operation, outcome,
                value_class, duration_bucket;
            "#,
        )?;
    } else {
        transaction.execute_batch(
            r#"
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, context_coverage,
                calls, result_count, delivered_output_bytes,
                delivered_context_bytes, matched_normalized_session_bytes
            )
            SELECT day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, context_coverage,
                SUM(calls), SUM(result_count), SUM(delivered_output_bytes),
                SUM(delivered_context_bytes), SUM(matched_normalized_session_bytes)
            FROM daily_usage_previous
            WHERE
                (definition_version = 1 AND surface = 'cli' AND operation IN (
                    'setup', 'index', 'sources', 'import', 'show', 'locate',
                    'search', 'docs', 'integrations', 'daemon_status',
                    'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
                ))
                OR (definition_version = 2 AND surface = 'cli' AND operation IN (
                    'setup', 'index', 'sources', 'import', 'show_session',
                    'show_event', 'locate', 'search', 'docs',
                    'integrations', 'daemon_status', 'daemon_enable',
                    'daemon_disable', 'upgrade', 'doctor'
                ))
                OR (surface = 'mcp' AND operation IN (
                    'status', 'sources', 'search', 'show_session', 'show_event'
                ))
            GROUP BY day_utc, definition_version, ctx_version, surface,
                operation, outcome, value_class, duration_bucket, context_coverage;
            "#,
        )?;
    }
    transaction.execute_batch("DROP TABLE daily_usage_previous;")?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
        PREVIOUS_SCHEMA_VERSION => verify_daily_schema(
            conn,
            DAILY_USAGE_SCHEMA_V2,
            EXPECTED_PREDECESSOR_DAILY_COLUMNS,
        )?,
        RELEASED_SCHEMA_VERSION => {
            let released_schema = daily_usage_schema_v3();
            verify_daily_schema(conn, &released_schema, EXPECTED_PREDECESSOR_DAILY_COLUMNS)?;
        }
        PRIOR_SCHEMA_VERSION => {
            verify_daily_schema(conn, DAILY_USAGE_SCHEMA_V4, EXPECTED_CURRENT_DAILY_COLUMNS)?
        }
        SCHEMA_VERSION => {
            verify_daily_schema(conn, DAILY_USAGE_SCHEMA, EXPECTED_CURRENT_DAILY_COLUMNS)?
        }
        _ => return Err(UsageStoreError::SchemaVersion(user_version)),
    }
    verify_maintenance_schema(conn)?;
    Ok(user_version)
}

pub(super) fn verify_schema(conn: &Connection) -> Result<(), UsageStoreError> {
    let version = verify_supported_schema(conn)?;
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(UsageStoreError::SchemaVersion(version))
    }
}

fn verify_maintenance_schema(conn: &Connection) -> Result<(), UsageStoreError> {
    let actual = table_schema(conn, "maintenance")?;
    if canonical_schema(&actual) == canonical_schema(MAINTENANCE_SCHEMA) {
        Ok(())
    } else {
        Err(UsageStoreError::SchemaIdentity)
    }
}

fn verify_daily_schema_v1(conn: &Connection) -> Result<(), UsageStoreError> {
    let initial_schema = released_daily_usage_schema_v1_initial();
    verify_daily_schema_variants(
        conn,
        &[DAILY_USAGE_SCHEMA_V1, initial_schema.as_str()],
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
    if expected_schemas
        .iter()
        .any(|expected| canonical_schema(&actual) == canonical_schema(expected))
    {
        Ok(())
    } else {
        Err(UsageStoreError::SchemaIdentity)
    }
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
            ("index", name, "daily_usage" | "maintenance", None) if name.starts_with("sqlite_") => {
            }
            _ => return Err(UsageStoreError::SchemaIdentity),
        }
    }
    if daily_usage && maintenance {
        Ok(())
    } else {
        Err(UsageStoreError::SchemaIdentity)
    }
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

pub fn verify_report_dates(conn: &Connection, now: SystemTime) -> Result<(), UsageStoreError> {
    reject_future_dates(conn, &utc_day(now))
}

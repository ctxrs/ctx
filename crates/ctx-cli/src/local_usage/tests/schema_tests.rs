use super::*;

fn assert_constraint_rejected(conn: &Connection, sql: &str) {
    conn.execute_batch("SAVEPOINT invalid_row").unwrap();
    let result = conn.execute(sql, []);
    conn.execute_batch("ROLLBACK TO invalid_row; RELEASE invalid_row")
        .unwrap();
    assert!(result.is_err(), "{sql}");
}

#[test]
fn daily_upsert_uses_closed_content_free_dimensions() {
    let root = private_tempdir();
    store::record(
        root.path(),
        mcp_operation("search", true, ValueClass::ResultBearing, 1),
    )
    .unwrap();
    store::record(
        root.path(),
        mcp_operation("search", true, ValueClass::ResultBearing, 1),
    )
    .unwrap();

    let report = read_report(root.path(), true, true);
    let summary = report.summary.unwrap();
    assert_eq!(report.state, "ready");
    assert_eq!(summary.calls, 2);
    assert_eq!(summary.result_bearing_calls, 2);
    assert_eq!(summary.not_applicable_calls, 0);
    assert_eq!(
        summary.result_bearing_calls + summary.empty_calls + summary.not_applicable_calls,
        summary.calls
    );
    assert_eq!(summary.ctx_versions, [CTX_VERSION]);
    assert_eq!(
        report.details.unwrap().by_operation[0].ctx_version,
        CTX_VERSION
    );

    let conn = Connection::open(usage_path(root.path())).unwrap();
    let columns = conn
        .prepare("PRAGMA table_info(daily_usage)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for forbidden in [
        "query",
        "path",
        "repository",
        "selector",
        "session",
        "event",
        "citation_id",
        "prompt",
        "argument",
        "timestamp",
        "transcript",
        "output_body",
        "machine",
        "identity",
        "token",
        "saving",
    ] {
        assert!(!columns.iter().any(|column| column.contains(forbidden)));
    }
    assert!(columns.contains(&"ctx_version".to_owned()));
    assert!(columns.contains(&"citation_count".to_owned()));
}

#[test]
fn value_classes_reconcile_successes_and_failures() {
    let root = private_tempdir();
    store::record(
        root.path(),
        mcp_operation("search", true, ValueClass::ResultBearing, 1),
    )
    .unwrap();
    store::record(
        root.path(),
        mcp_operation("search", true, ValueClass::Empty, 0),
    )
    .unwrap();
    store::record(
        root.path(),
        mcp_operation("search", false, ValueClass::NotApplicable, 0),
    )
    .unwrap();

    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.calls, 3);
    assert_eq!(summary.successful_calls, 2);
    assert_eq!(summary.failed_calls, 1);
    assert_eq!(summary.result_bearing_calls, 1);
    assert_eq!(summary.empty_calls, 1);
    assert_eq!(summary.not_applicable_calls, 1);
    assert_eq!(
        summary.result_bearing_calls + summary.empty_calls + summary.not_applicable_calls,
        summary.calls
    );
    let successful_not_applicable = summary
        .not_applicable_calls
        .saturating_sub(summary.failed_calls);
    assert_eq!(
        summary.result_bearing_calls + summary.empty_calls + successful_not_applicable,
        summary.successful_calls
    );
}

#[test]
fn v1_schema_rejects_impossible_blame_value_classifications() {
    let root = private_tempdir();
    store::create_mixed_v1_fixture_for_test(root.path()).unwrap();
    let conn = Connection::open(usage_path(root.path())).unwrap();
    assert_constraint_rejected(
        &conn,
        "UPDATE daily_usage SET pro_outcome = 'possible' \
         WHERE operation = 'blame' AND value_class = 'empty'",
    );
    assert_constraint_rejected(
        &conn,
        "UPDATE daily_usage SET value_class = 'empty', result_count = 0, citation_count = 0 \
         WHERE operation = 'blame' AND pro_outcome = 'produced'",
    );
}

#[test]
fn sqlite_rejects_unknown_and_cross_surface_operations() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    store::record(
        root.path(),
        mcp_operation("status", true, ValueClass::NotApplicable, 0),
    )
    .unwrap();
    let conn = Connection::open(usage_path(root.path())).unwrap();
    for sql in [
        "UPDATE daily_usage SET operation = 'query=private-content' \
         WHERE surface = 'cli' AND operation = 'doctor'",
        "UPDATE daily_usage SET operation = 'status' \
         WHERE surface = 'cli' AND operation = 'doctor'",
        "UPDATE daily_usage SET operation = 'unknown' \
         WHERE surface = 'mcp' AND operation = 'status'",
        "UPDATE daily_usage SET operation = 'show_session' \
         WHERE surface = 'cli' AND operation = 'doctor'",
        "UPDATE daily_usage SET operation = 'pro_manage' \
         WHERE surface = 'mcp' AND operation = 'status'",
    ] {
        assert_constraint_rejected(&conn, sql);
    }
}

#[test]
fn sqlite_enforces_counter_and_dimension_applicability_invariants() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    store::record(
        root.path(),
        mcp_operation("search", true, ValueClass::ResultBearing, 1),
    )
    .unwrap();
    store::record(
        root.path(),
        mcp_operation("status", true, ValueClass::NotApplicable, 0),
    )
    .unwrap();
    let mut open = mcp_operation("show_session", true, ValueClass::ResultBearing, 1);
    open.context.context_opened = 1;
    open.context.validated_discoveries = 1;
    store::record(root.path(), open).unwrap();
    let mut produced_blame = mcp_operation("blame", true, ValueClass::ResultBearing, 1);
    produced_blame.target_type = TargetType::Commit;
    produced_blame.pro_outcome = ProOutcome::Produced;
    store::record(root.path(), produced_blame).unwrap();
    let mut empty_blame = mcp_operation("blame", true, ValueClass::Empty, 0);
    empty_blame.target_type = TargetType::File;
    empty_blame.pro_outcome = ProOutcome::None;
    store::record(root.path(), empty_blame).unwrap();
    let conn = Connection::open(usage_path(root.path())).unwrap();
    let invalid_updates = [
        // Failures are N/A and cannot retain result or citation counts.
        "UPDATE daily_usage SET outcome = 'failure' \
         WHERE surface = 'mcp' AND operation = 'search'",
        // Empty/N/A classes carry no exact result or citation totals.
        "UPDATE daily_usage SET value_class = 'empty' \
         WHERE surface = 'mcp' AND operation = 'search'",
        // Result actions cannot be assigned to an unrelated operation.
        "UPDATE daily_usage SET result_action = 'open_event' \
         WHERE surface = 'mcp' AND operation = 'search'",
        // Successful result-capable operations must classify nonempty vs empty.
        "UPDATE daily_usage SET value_class = 'not_applicable', result_count = 0, \
         context_bytes = 0, context_byte_samples = 0, search_result_bytes = 0, \
         search_result_byte_samples = 0 \
         WHERE surface = 'mcp' AND operation = 'search'",
        // Status operations have no result classification.
        "UPDATE daily_usage SET value_class = 'empty', result_action = 'sources' \
         WHERE surface = 'mcp' AND operation = 'status'",
        // Non-blame rows cannot carry target or Pro dimensions.
        "UPDATE daily_usage SET target_type = 'file' \
         WHERE surface = 'mcp' AND operation = 'status'",
        // Successful MCP blame cannot use the pre-target N/A target.
        "UPDATE daily_usage SET operation = 'blame', value_class = 'empty', \
         result_action = 'blame', pro_outcome = 'none' \
         WHERE surface = 'mcp' AND operation = 'status'",
        // Produced/possible outcomes require results, while an empty blame
        // response can only classify as no attribution.
        "UPDATE daily_usage SET value_class = 'empty', result_count = 0 \
         WHERE surface = 'mcp' AND operation = 'blame' AND pro_outcome = 'produced'",
        "UPDATE daily_usage SET pro_outcome = 'possible' \
         WHERE surface = 'mcp' AND operation = 'blame' AND value_class = 'empty'",
        // Transport bytes apply only to delivered MCP responses.
        "UPDATE daily_usage SET response_bytes = 1, response_byte_samples = 1 \
         WHERE surface = 'cli' AND operation = 'doctor'",
        "UPDATE daily_usage SET response_bytes = 0 \
         WHERE surface = 'mcp' AND operation = 'status'",
        // Search-result bytes are a strict subset of one-copy semantic bytes.
        "UPDATE daily_usage SET search_result_bytes = context_bytes + 1 \
         WHERE surface = 'mcp' AND operation = 'search'",
        // Search correlation is bounded by calls and factual result counts.
        "UPDATE daily_usage SET context_searches = calls + 1 \
         WHERE surface = 'mcp' AND operation = 'search'",
        "UPDATE daily_usage SET context_searches = 1, context_found = result_count + 1 \
         WHERE surface = 'mcp' AND operation = 'search'",
        "UPDATE daily_usage SET context_searches = 0, context_found = 1 \
         WHERE surface = 'mcp' AND operation = 'search'",
        // Open correlation is bounded, validation needs an open, and cite is
        // closed until a real result action supports it.
        "UPDATE daily_usage SET context_opened = calls + 1 \
         WHERE surface = 'mcp' AND operation = 'show_session'",
        "UPDATE daily_usage SET context_opened = 0, validated_discoveries = 1 \
         WHERE surface = 'mcp' AND operation = 'show_session'",
        "UPDATE daily_usage SET context_cited = 1 \
         WHERE surface = 'mcp' AND operation = 'show_session'",
        "UPDATE daily_usage SET context_found = 1 \
         WHERE surface = 'mcp' AND operation = 'show_session'",
        // Unrelated rows cannot carry correlation proxies.
        "UPDATE daily_usage SET context_searches = 1 \
         WHERE surface = 'mcp' AND operation = 'status'",
    ];
    for sql in invalid_updates {
        assert_constraint_rejected(&conn, sql);
    }
    conn.execute(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            result_action, calls, result_count, citation_count, latency_ms,
            latency_samples, response_bytes, response_byte_samples, output_bytes,
            output_byte_samples, context_bytes, context_byte_samples,
            search_result_bytes, search_result_byte_samples, context_searches,
            context_found, context_opened, context_cited, validated_discoveries
        ) VALUES (
            '2026-07-25', 2, '0.26.0', 'cli', 'blame', 'failure',
            'not_applicable', 'under_10_ms', 'not_applicable', 'error',
            'not_applicable', 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0
        )
        "#,
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO maintenance(singleton, last_retention_day) VALUES (1, '2026-02-30')",
            [],
        )
        .is_err());
}

#[test]
fn sqlite_identity_permissions_and_journal_policy_are_explicit() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let (page_size, max_page_count, wal_autocheckpoint, journal_size_limit) =
        store::growth_policy_for_test(root.path()).unwrap();
    assert_eq!(page_size, 4 * 1024);
    assert_eq!(max_page_count * page_size, 6 * 1024 * 1024);
    assert_eq!(wal_autocheckpoint, 64);
    assert_eq!(journal_size_limit, 1024 * 1024);
    let path = usage_path(root.path());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
            .unwrap(),
        0x4354_5855
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o177, 0);
    }
}

#[test]
fn incompatible_page_size_is_rejected_as_a_schema_error() {
    let root = private_tempdir();
    let path = usage_path(root.path());
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "page_size", 8 * 1024).unwrap();
    conn.execute_batch("CREATE TABLE incompatible(value INTEGER) STRICT;")
        .unwrap();
    drop(conn);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert_eq!(
        report.error.unwrap().message,
        "local usage store format is not supported"
    );
}

#[test]
fn unknown_existing_sidecar_is_not_mutated_or_switched_to_wal() {
    let root = private_tempdir();
    let path = usage_path(root.path());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE foreign_data(value TEXT);")
        .unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    drop(conn);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let before = fs::read(&path).unwrap();

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert_eq!(fs::read(&path).unwrap(), before);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert!(!auxiliary(&path, "-wal").exists());
    assert!(!auxiliary(&path, "-shm").exists());
}

#[test]
fn wal_only_unknown_schema_is_rejected_without_mutating_the_sqlite_family() {
    let source = private_tempdir();
    store::record(source.path(), operation("doctor")).unwrap();
    let source_path = usage_path(source.path());
    let conn = Connection::open(&source_path).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute_batch(
        "CREATE TABLE wal_only_canary (exact_timestamp TEXT NOT NULL) STRICT;\
         INSERT INTO wal_only_canary VALUES (datetime('now'));",
    )
    .unwrap();
    assert!(fs::metadata(auxiliary(&source_path, "-wal")).unwrap().len() > 0);

    let crashed = private_tempdir();
    let crashed_path = usage_path(crashed.path());
    for suffix in ["", "-wal", "-shm"] {
        let from = if suffix.is_empty() {
            source_path.clone()
        } else {
            auxiliary(&source_path, suffix)
        };
        let to = if suffix.is_empty() {
            crashed_path.clone()
        } else {
            auxiliary(&crashed_path, suffix)
        };
        fs::copy(from, to).unwrap();
    }
    drop(conn);

    let before = sqlite_family_snapshot(&crashed_path);
    assert!(matches!(
        store::record(crashed.path(), operation("docs")),
        Err(store::UsageStoreError::UnsafeReadState)
    ));
    assert_eq!(sqlite_family_snapshot(&crashed_path), before);
    assert!(matches!(
        reset(crashed.path()),
        Err(store::UsageStoreError::UnsafeReadState)
    ));
    assert_eq!(sqlite_family_snapshot(&crashed_path), before);
}

#[test]
fn nonempty_shm_only_family_is_rejected_without_mutation() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    let shm = auxiliary(&path, "-shm");
    fs::write(&shm, b"unsafe-shm-only-state").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        fs::set_permissions(&shm, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::metadata(&shm).unwrap().nlink(), 1);
    }

    let before = sqlite_family_snapshot(&path);
    assert!(matches!(
        store::record(root.path(), operation("docs")),
        Err(store::UsageStoreError::UnsafeReadState)
    ));
    assert_eq!(sqlite_family_snapshot(&path), before);
    assert!(matches!(
        reset(root.path()),
        Err(store::UsageStoreError::UnsafeReadState)
    ));
    assert_eq!(sqlite_family_snapshot(&path), before);
}

#[test]
fn record_and_reset_reject_constraint_bypassed_rows_without_mutation() {
    for tamper in [
        "UPDATE daily_usage SET calls = -1",
        "UPDATE maintenance SET singleton = 2",
        "UPDATE daily_usage SET operation = 'blame', value_class = 'empty', \
         target_type = 'file', pro_outcome = 'possible', result_action = 'blame'",
    ] {
        let root = private_tempdir();
        store::record(root.path(), operation("doctor")).unwrap();
        let path = usage_path(root.path());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        conn.execute(tamper, []).unwrap();
        drop(conn);

        let before = sqlite_family_snapshot(&path);
        assert_eq!(read_report(root.path(), true, false).state, "error");
        assert!(matches!(
            store::record(root.path(), operation("docs")),
            Err(store::UsageStoreError::Integrity)
        ));
        assert_eq!(sqlite_family_snapshot(&path), before, "{tamper}");
        assert!(matches!(
            reset(root.path()),
            Err(store::UsageStoreError::Integrity)
        ));
        assert_eq!(sqlite_family_snapshot(&path), before, "{tamper}");
    }
}

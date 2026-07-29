use super::*;

#[cfg(unix)]
#[test]
fn report_discovery_opens_the_store_with_read_only_filesystem_access() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let report = read_report(root.path(), true, false);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(report.state, "ready", "{:?}", report.error);
    assert_eq!(report.summary.unwrap().calls, 1);
}

#[test]
fn reporting_never_changes_checkpointed_or_active_sqlite_families() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    assert!(!auxiliary(&path, "-wal").exists());
    assert!(!auxiliary(&path, "-shm").exists());
    let checkpointed_before = directory_bytes(root.path());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    assert_eq!(directory_bytes(root.path()), checkpointed_before);
    let mut snapshot = store::open_read_only(&path).unwrap();
    assert_eq!(
        snapshot
            .connection_mut()
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(snapshot
        .connection_mut()
        .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
        .unwrap());
    let write = snapshot
        .connection_mut()
        .execute("DELETE FROM daily_usage", []);
    assert!(
        matches!(
            write,
            Err(ref error)
                if error.sqlite_error_code() == Some(ErrorCode::ReadOnly)
        ),
        "{write:?}"
    );
    snapshot.verify_unchanged().unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
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
            date('now'), 2, '0.26.0-active-wal', 'cli', 'doctor',
            'success', 'not_applicable', '10_to_49_ms', 'not_applicable',
            'not_applicable', 'not_applicable', 1, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        )
        "#,
        [],
    )
    .unwrap();
    let wal = auxiliary(&path, "-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);
    let active_before = directory_bytes(root.path());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "error");
    assert_eq!(report.error.unwrap().code, "usage_store_unavailable");
    assert_eq!(directory_bytes(root.path()), active_before);
    drop(conn);
}

#[test]
fn v1_detached_read_migration_normalizes_impossible_blame_without_mutating_source() {
    let root = private_tempdir();
    store::create_legacy_impossible_blame_v1_fixture_for_test(root.path()).unwrap();
    let path = usage_path(root.path());
    let before = sqlite_family_snapshot(&path);

    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    let summary = report.summary.unwrap();
    assert_eq!(summary.calls, 9);
    assert_eq!(summary.result_count, 9);
    assert_eq!(summary.citation_count, 1);
    assert_eq!(summary.pro_blame.citation_count, 1);
    assert_eq!(summary.pro_blame.produced_attribution_requests, 1);
    assert_eq!(summary.pro_blame.possible_or_reference_only_requests, 0);
    assert_eq!(summary.pro_blame.no_confident_attribution_requests, 1);
    assert_eq!(summary.mcp_response_bytes, 1_500);
    assert_eq!(summary.mcp_response_byte_samples, 6);
    assert_eq!(summary.cli_output_bytes, 0);
    assert_eq!(summary.cli_output_byte_samples, 0);
    assert_eq!(summary.measured_latency_samples, 0);
    assert_eq!(summary.semantic_context_bytes, 0);
    assert_eq!(summary.semantic_context_byte_samples, 0);
    assert_eq!(summary.semantic_search_result_bytes, 0);
    assert_eq!(summary.semantic_search_result_byte_samples, 0);
    assert_eq!(summary.result_actions.searches, 3);
    assert_eq!(summary.result_actions.result_bearing_searches, 3);
    assert_eq!(summary.result_actions.sessions_opened, 1);
    assert_eq!(summary.result_actions.blame_requests, 2);
    assert_eq!(summary.context.context_found, 0);
    assert_eq!(summary.context.context_opened, 0);
    let estimates = report.estimates.unwrap();
    assert_eq!(
        estimates.approximate_context_tokens.coverage,
        EstimateCoverage::UnavailableLegacy
    );
    assert_eq!(
        estimates.approximate_context_tokens.approximate_tokens,
        None
    );
    assert_eq!(
        estimates.approximate_avoided_context_tokens.coverage,
        EstimateCoverage::UnavailableLegacy
    );
    assert_eq!(
        estimates
            .approximate_avoided_context_tokens
            .approximate_tokens,
        None
    );
    assert_eq!(estimates.estimated_time_saved_seconds, 480);
    assert_eq!(sqlite_family_snapshot(&path), before);

    let conn = Connection::open(path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn v1_writable_migration_preserves_facts_and_leaves_new_measurements_unknown() {
    let root = private_tempdir();
    store::create_legacy_impossible_blame_v1_fixture_for_test(root.path()).unwrap();
    store::growth_policy_for_test(root.path()).unwrap();
    let path = usage_path(root.path());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    let migrated = conn
        .query_row(
            r#"
            SELECT
                SUM(calls), SUM(result_count), SUM(citation_count),
                SUM(response_bytes), SUM(response_byte_samples),
                SUM(latency_ms), SUM(latency_samples),
                SUM(output_bytes), SUM(output_byte_samples),
                SUM(context_bytes), SUM(context_byte_samples),
                SUM(search_result_bytes), SUM(search_result_byte_samples),
                SUM(context_searches), SUM(context_found), SUM(context_opened),
                SUM(context_cited), SUM(validated_discoveries),
                MIN(definition_version), MAX(definition_version)
            FROM daily_usage
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, i64>(18)?,
                    row.get::<_, i64>(19)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(migrated.0, 9);
    assert_eq!(migrated.1, 9);
    assert_eq!(migrated.2, 1);
    assert_eq!(migrated.3, 1_500);
    assert_eq!(migrated.4, 6);
    assert_eq!(
        [
            migrated.5,
            migrated.6,
            migrated.7,
            migrated.8,
            migrated.9,
            migrated.10,
            migrated.11,
            migrated.12,
            migrated.13,
            migrated.14,
            migrated.15,
            migrated.16,
            migrated.17,
        ],
        [0; 13]
    );
    assert_eq!((migrated.18, migrated.19), (2, 2));
    assert_eq!(
        conn.query_row(
            "SELECT store_generation FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM daily_usage \
             WHERE operation = 'blame' AND value_class = 'empty' AND pro_outcome != 'none'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    drop(conn);

    store::growth_policy_for_test(root.path()).unwrap();
    let report = read_report(root.path(), true, false);
    assert_eq!(report.summary.unwrap().calls, 9);
    assert_eq!(
        report
            .estimates
            .unwrap()
            .approximate_avoided_context_tokens
            .coverage,
        EstimateCoverage::UnavailableLegacy
    );

    store::record(
        root.path(),
        mcp_operation("search", true, ValueClass::ResultBearing, 1),
    )
    .unwrap();
    let report = read_report(root.path(), true, false);
    assert_eq!(
        report
            .estimates
            .unwrap()
            .approximate_avoided_context_tokens
            .coverage,
        EstimateCoverage::Partial
    );
}

#[test]
fn pre_generation_v2_reports_read_only_and_migrates_in_place_on_next_write() {
    let root = private_tempdir();
    store::create_legacy_v2_fixture_for_test(root.path()).unwrap();
    let path = usage_path(root.path());
    let before = sqlite_family_snapshot(&path);

    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    assert_eq!(report.summary.unwrap().calls, 1);
    assert_eq!(sqlite_family_snapshot(&path), before);
    let conn = Connection::open(&path).unwrap();
    let columns = conn
        .prepare("PRAGMA table_info(maintenance)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(columns, ["singleton", "last_retention_day"]);
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
    drop(conn);

    store::growth_policy_for_test(root.path()).unwrap();
    let conn = Connection::open(&path).unwrap();
    let columns = conn
        .prepare("PRAGMA table_info(maintenance)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        columns,
        ["singleton", "last_retention_day", "store_generation"]
    );
    assert_eq!(
        conn.query_row(
            "SELECT store_generation FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    drop(conn);
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        1
    );
}

#[test]
fn v1_migration_coalesces_rows_that_conservative_blame_normalization_collides() {
    let root = private_tempdir();
    store::create_legacy_colliding_blame_v1_fixture_for_test(root.path()).unwrap();
    store::growth_policy_for_test(root.path()).unwrap();

    let conn = Connection::open(usage_path(root.path())).unwrap();
    let normalized: (i64, i64, i64) = conn
        .query_row(
            "SELECT calls, response_bytes, response_byte_samples FROM daily_usage \
             WHERE operation = 'blame' AND value_class = 'empty' AND pro_outcome = 'none'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(normalized, (2, 400, 2));
    drop(conn);

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    let summary = report.summary.unwrap();
    assert_eq!(summary.calls, 10);
    assert_eq!(summary.pro_blame.possible_or_reference_only_requests, 0);
    assert_eq!(summary.pro_blame.no_confident_attribution_requests, 2);
    assert_eq!(report.estimates.unwrap().estimated_time_saved_seconds, 480);
}

#[test]
fn v1_open_writable_migration_retries_after_post_delete_failure() {
    let rollback = private_tempdir();
    store::create_mixed_v1_fixture_for_test(rollback.path()).unwrap();
    let rollback_path = usage_path(rollback.path());
    assert!(matches!(
        store::fail_v1_migration_before_commit_for_test(rollback.path()),
        Err(store::UsageStoreError::Integrity)
    ));
    let conn = Connection::open(&rollback_path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'daily_usage_v1'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(
        conn.query_row("SELECT SUM(calls) FROM daily_usage", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        9
    );
    drop(conn);

    store::growth_policy_for_test(rollback.path()).unwrap();
    let conn = Connection::open(&rollback_path).unwrap();
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
    drop(conn);
    let report = read_report(rollback.path(), true, false);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    assert_eq!(report.summary.unwrap().calls, 9);
}

#[test]
fn unknown_versions_fail_closed_without_mutation() {
    let unknown = private_tempdir();
    store::create_mixed_v1_fixture_for_test(unknown.path()).unwrap();
    let unknown_path = usage_path(unknown.path());
    let conn = Connection::open(&unknown_path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    let before = sqlite_family_snapshot(&unknown_path);
    assert_eq!(read_report(unknown.path(), true, false).state, "error");
    assert!(matches!(
        store::growth_policy_for_test(unknown.path()),
        Err(store::UsageStoreError::SchemaVersion(99))
    ));
    assert_eq!(sqlite_family_snapshot(&unknown_path), before);
}

#[test]
fn report_rejects_semantic_sample_coverage_above_eligible_actions() {
    let root = private_tempdir();
    store::record(
        root.path(),
        mcp_operation("status", true, ValueClass::NotApplicable, 0),
    )
    .unwrap();
    let path = usage_path(root.path());
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    conn.execute(
        "UPDATE daily_usage SET context_bytes = 20, context_byte_samples = 1",
        [],
    )
    .unwrap();
    conn.pragma_update(None, "ignore_check_constraints", false)
        .unwrap();
    drop(conn);
    assert_eq!(read_report(root.path(), true, false).state, "error");
}

#[test]
fn blame_citations_are_separate_from_global_delivery_citations() {
    let root = private_tempdir();
    let mut search = mcp_operation("search", true, ValueClass::ResultBearing, 1);
    search.citation_count = 3;
    store::record(root.path(), search).unwrap();

    let mut blame = mcp_operation("blame", true, ValueClass::ResultBearing, 1);
    blame.target_type = TargetType::Commit;
    blame.pro_outcome = ProOutcome::Produced;
    blame.citation_count = 2;
    store::record(root.path(), blame).unwrap();

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    let summary = report.summary.unwrap();
    assert_eq!(summary.citation_count, 5);
    assert_eq!(summary.pro_blame.citation_count, 2);
}

#[test]
fn report_accepts_open_after_best_effort_search_write_is_dropped() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let lock = Connection::open(usage_path(root.path())).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut search = mcp_operation("search", true, ValueClass::ResultBearing, 1);
    search.context.context_searches = 1;
    search.context.context_found = 1;
    super::super::record_best_effort(root.path(), true, search);
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);

    let mut opened = mcp_operation("show_session", true, ValueClass::ResultBearing, 1);
    opened.context.context_opened = 1;
    opened.context.validated_discoveries = 1;
    store::record(root.path(), opened).unwrap();

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    let summary = report.summary.unwrap();
    assert_eq!(summary.calls, 2);
    assert_eq!(summary.context.context_found, 0);
    assert_eq!(summary.context.context_opened, 1);
    assert_eq!(summary.context.context_cited_coverage, "unsupported");
    assert_eq!(summary.context.validated_discoveries, 1);
    assert_eq!(
        report.estimates.unwrap().estimated_time_saved_seconds,
        ESTIMATE_MODEL.discovered_record_open_seconds
    );
}

#[test]
fn report_accepts_show_persisted_after_reset_removed_its_search() {
    let root = private_tempdir();
    let mut search = mcp_operation("search", true, ValueClass::ResultBearing, 1);
    search.context.context_searches = 1;
    search.context.context_found = 1;
    store::record(root.path(), search).unwrap();
    assert!(reset(root.path()).unwrap());

    let mut opened = mcp_operation("show_event", true, ValueClass::ResultBearing, 1);
    opened.context.context_opened = 1;
    opened.context.validated_discoveries = 1;
    store::record(root.path(), opened).unwrap();

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    let summary = report.summary.unwrap();
    assert_eq!(summary.calls, 1);
    assert_eq!(summary.context.context_found, 0);
    assert_eq!(summary.context.context_opened, 1);
    assert_eq!(summary.context.validated_discoveries, 1);
}

#[test]
fn reset_migrates_v1_then_logically_deletes_every_aggregate() {
    let root = private_tempdir();
    store::create_mixed_v1_fixture_for_test(root.path()).unwrap();
    assert!(reset(root.path()).unwrap());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "empty");
    assert_eq!(report.summary.unwrap().calls, 0);
    let conn = Connection::open(usage_path(root.path())).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT store_generation FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

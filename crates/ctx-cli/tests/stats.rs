mod support;

use std::fs;

use predicates::prelude::*;
use rusqlite::Connection;
use support::*;

fn enabled(command: &mut assert_cmd::Command) -> &mut assert_cmd::Command {
    command.env_remove("CTX_LOCAL_USAGE_ENABLED")
}

#[test]
fn pristine_and_disabled_stats_are_truthful_and_create_nothing() {
    let pristine = tempdir();
    let report = json_output(enabled(ctx(&pristine).args(["stats", "--format=json"])));
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["local_usage"]["enabled"], true);
    assert_eq!(report["local_usage"]["state"], "empty");
    assert_eq!(report["measured"]["delivery"]["calls"], 0);
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["approximate_tokens"],
        0
    );
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["coverage"],
        "complete"
    );
    assert_eq!(
        report["estimated"]["approximate_avoided_context_tokens"]["approximate_tokens"],
        0
    );
    assert_eq!(report["estimated"]["estimated_time_saved_seconds"], 0);
    assert_eq!(report["estimated"]["model"]["version"], 1);
    assert_eq!(report["local_only"], true);
    assert_eq!(report["read_only"], true);
    assert!(!pristine.path().join("usage.sqlite").exists());
    assert!(!pristine.path().join("config.toml").exists());

    let disabled = tempdir();
    let report = json_output(
        ctx(&disabled)
            .args(["stats", "--format=json"])
            .env("CTX_LOCAL_USAGE_ENABLED", "false"),
    );
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["local_usage"]["enabled"], false);
    assert_eq!(report["local_usage"]["state"], "disabled");
    assert!(report["measured"].is_null());
    assert!(report["estimated"].is_null());
    assert!(!disabled.path().join("usage.sqlite").exists());
    assert!(!disabled.path().join("config.toml").exists());
}

#[test]
fn malformed_config_returns_one_content_free_stats_json_error() {
    let temp = tempdir();
    let marker = "PRIVATE_STATS_CONFIG_79af";
    fs::write(
        temp.path().join("config.toml"),
        format!("[local_usage]\nenabled = nope\n# {marker}\n"),
    )
    .unwrap();

    let output = enabled(ctx(&temp).args(["stats", "--format=json"]))
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty());
    let encoded = String::from_utf8(output.stderr).unwrap();
    let report: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["local_usage"]["state"], "error");
    assert_eq!(
        report["local_usage"]["error"]["code"],
        "local_usage_config_unavailable"
    );
    assert!(report["measured"].is_null());
    assert!(report["estimated"].is_null());
    assert_eq!(report["local_only"], true);
    assert_eq!(report["read_only"], true);
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!temp.path().join("usage.sqlite").exists());
}

#[test]
fn stats_is_read_only_and_does_not_count_itself() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let usage_path = temp.path().join("usage.sqlite");
    let before = fs::read(&usage_path).unwrap();
    let before_modified = fs::metadata(&usage_path).unwrap().modified().unwrap();

    for args in [
        &["stats"][..],
        &["stats", "--detail"],
        &["stats", "--format=json"],
    ] {
        enabled(ctx(&temp).args(args)).assert().success();
    }

    let connection = Connection::open(&usage_path).unwrap();
    let calls: u64 = connection
        .query_row("SELECT SUM(calls) FROM daily_usage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(calls, 1);
    drop(connection);
    assert_eq!(fs::read(&usage_path).unwrap(), before);
    assert_eq!(
        fs::metadata(&usage_path).unwrap().modified().unwrap(),
        before_modified
    );
    assert!(!temp.path().join("usage.sqlite-wal").exists());
    assert!(!temp.path().join("usage.sqlite-shm").exists());
}

#[test]
fn v2_json_keeps_measured_channels_actions_proxies_and_estimates_distinct() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    connection
        .execute_batch(
            r#"
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                result_action, calls, result_count, citation_count,
                latency_ms, latency_samples, response_bytes, response_byte_samples,
                output_bytes, output_byte_samples, context_bytes, context_byte_samples,
                search_result_bytes, search_result_byte_samples, context_searches,
                context_found, context_opened, context_cited, validated_discoveries
            ) VALUES (
                '2026-07-25', 2, '0.26.0', 'mcp', 'search', 'success',
                'result_bearing', '10_to_49_ms', 'not_applicable', 'not_applicable',
                'search', 1, 2, 3, 25, 1, 9000, 1, 0, 0, 800, 1, 400, 1,
                1, 1, 0, 0, 0
            );
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                result_action, calls, result_count, citation_count,
                latency_ms, latency_samples, response_bytes, response_byte_samples,
                output_bytes, output_byte_samples, context_bytes, context_byte_samples,
                search_result_bytes, search_result_byte_samples, context_searches,
                context_found, context_opened, context_cited, validated_discoveries
            ) VALUES (
                '2026-07-25', 2, '0.26.0', 'mcp', 'show_session', 'success',
                'result_bearing', 'under_10_ms', 'not_applicable', 'not_applicable',
                'open_session', 1, 2, 0, 5, 1, 1000, 1, 0, 0, 100, 1, 0, 0,
                0, 0, 1, 0, 1
            );
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                result_action, calls, result_count, citation_count,
                latency_ms, latency_samples, response_bytes, response_byte_samples,
                output_bytes, output_byte_samples, context_bytes, context_byte_samples,
                search_result_bytes, search_result_byte_samples, context_searches,
                context_found, context_opened, context_cited, validated_discoveries
            ) VALUES (
                '2026-07-25', 2, '0.26.0', 'mcp', 'show_event', 'success',
                'result_bearing', 'under_10_ms', 'not_applicable', 'not_applicable',
                'open_event', 1, 1, 0, 5, 1, 1100, 1, 0, 0, 120, 1, 0, 0,
                0, 0, 0, 0, 0
            );
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                result_action, calls, result_count, citation_count,
                latency_ms, latency_samples, response_bytes, response_byte_samples,
                output_bytes, output_byte_samples, context_bytes, context_byte_samples,
                search_result_bytes, search_result_byte_samples, context_searches,
                context_found, context_opened, context_cited, validated_discoveries
            ) VALUES (
                '2026-07-25', 2, '0.26.0', 'cli', 'locate', 'success',
                'result_bearing', 'under_10_ms', 'not_applicable', 'not_applicable',
                'locate', 1, 3, 0, 5, 1, 0, 0, 300, 1, 80, 1, 0, 0,
                0, 0, 0, 0, 0
            );
            INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                result_action, calls, result_count, citation_count,
                latency_ms, latency_samples, response_bytes, response_byte_samples,
                output_bytes, output_byte_samples, context_bytes, context_byte_samples,
                search_result_bytes, search_result_byte_samples, context_searches,
                context_found, context_opened, context_cited, validated_discoveries
            ) VALUES (
                '2026-07-25', 2, '0.26.0', 'mcp', 'blame', 'success',
                'result_bearing', 'under_10_ms', 'commit', 'produced',
                'blame', 1, 1, 2, 10, 1, 500, 1, 0, 0, 200, 1, 0, 0,
                0, 0, 0, 0, 0
            );
            "#,
        )
        .unwrap();
    drop(connection);

    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(report["schema_version"], 2);
    let history = &report["measured"]["history_retrieval"];
    assert_eq!(history["searches"], 1);
    assert_eq!(history["result_bearing_searches"], 1);
    assert_eq!(history["sessions_or_events_opened"], 2);
    assert_eq!(history["records_located"], 3);
    assert_eq!(history["discovery_proxy"]["context_searches"], 1);
    assert_eq!(history["discovery_proxy"]["context_found"], 1);
    assert_eq!(history["discovery_proxy"]["context_opened"], 1);
    assert_eq!(history["discovery_proxy"]["context_cited"], 0);
    assert_eq!(history["discovery_proxy"]["validated_discoveries"], 1);

    let provenance = &report["measured"]["code_provenance"];
    assert_eq!(provenance["blame_investigations"], 1);
    assert_eq!(provenance["origins_identified"], 1);
    assert_eq!(provenance["citations"], 2);

    let delivery = &report["measured"]["delivery"];
    assert_eq!(delivery["citations"], 5);
    assert!(delivery["cli_output_bytes"]["bytes"].as_u64().unwrap() >= 300);
    assert_eq!(delivery["mcp_transport_response_bytes"]["bytes"], 11_600);
    assert_eq!(delivery["semantic_context_bytes"]["bytes"], 1_300);
    assert_eq!(delivery["semantic_context_bytes"]["measured_samples"], 5);
    assert_eq!(delivery["semantic_search_result_bytes"]["bytes"], 400);
    assert_eq!(
        delivery["approximate_context_tokens"]["approximate_tokens"],
        325
    );
    assert_eq!(
        delivery["approximate_context_tokens"]["coverage"],
        "complete"
    );

    let estimated = &report["estimated"];
    assert_eq!(estimated["model"]["version"], 1);
    assert_eq!(estimated["model"]["avoided_search_token_multiplier"], 49);
    assert_eq!(
        estimated["approximate_avoided_context_tokens"]["approximate_tokens"],
        4_900
    );
    assert_eq!(
        estimated["approximate_avoided_context_tokens"]["coverage"],
        "complete"
    );
    assert_eq!(estimated["estimated_time_saved_seconds"], 375);
    assert_ne!(
        delivery["mcp_transport_response_bytes"]["bytes"],
        delivery["semantic_context_bytes"]["bytes"]
    );
    let operation_details = report["local_usage"]["details"]["by_operation"]
        .as_array()
        .unwrap();
    assert!(operation_details
        .iter()
        .any(|operation| operation["surface"] == "cli"));
    assert!(operation_details
        .iter()
        .any(|operation| operation["surface"] == "mcp"));
}

#[test]
fn migrated_v1_stats_keep_tokens_unavailable_and_count_based_time_available() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let usage_path = temp.path().join("usage.sqlite");
    let connection = Connection::open(&usage_path).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode = DELETE;")
        .unwrap();
    connection.execute_batch("DROP TABLE daily_usage;").unwrap();
    connection.execute_batch(LEGACY_DAILY_USAGE_SCHEMA).unwrap();
    connection
        .execute(
            "INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                calls, result_count, citation_count, response_bytes
             ) VALUES (
                '2026-07-25', 1, '0.25.0', 'mcp', 'search', 'success',
                'result_bearing', '10_to_49_ms', 'not_applicable',
                'not_applicable', 3, 6, 0, 1500
             )",
            [],
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);

    let before = fs::read(&usage_path).unwrap();
    let before_modified = fs::metadata(&usage_path).unwrap().modified().unwrap();
    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(report["local_usage"]["state"], "ready");
    assert_eq!(report["measured"]["history_retrieval"]["searches"], 3);
    assert_eq!(
        report["measured"]["delivery"]["semantic_context_bytes"]["bytes"],
        0
    );
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["approximate_tokens"],
        serde_json::Value::Null
    );
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["coverage"],
        "unavailable_legacy"
    );
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["eligible_samples"],
        3
    );
    assert_eq!(
        report["estimated"]["approximate_avoided_context_tokens"]["approximate_tokens"],
        serde_json::Value::Null
    );
    assert_eq!(
        report["estimated"]["approximate_avoided_context_tokens"]["coverage"],
        "unavailable_legacy"
    );
    assert_eq!(report["estimated"]["estimated_time_saved_seconds"], 180);
    assert_eq!(fs::read(&usage_path).unwrap(), before);
    assert_eq!(
        fs::metadata(&usage_path).unwrap().modified().unwrap(),
        before_modified
    );

    enabled(ctx(&temp).args(["stats"]))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Approximate context tokens: unavailable",
        ))
        .stdout(predicate::str::contains(
            "Approximate context tokens avoided: unavailable",
        ))
        .stdout(predicate::str::contains(
            "Estimated time saved: 180 seconds",
        ));
    assert_eq!(fs::read(&usage_path).unwrap(), before);
}

#[test]
fn partial_byte_coverage_is_explicit_in_json_and_human_output() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    connection
        .execute(
            "INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                result_action, calls, result_count, citation_count,
                latency_ms, latency_samples, response_bytes, response_byte_samples,
                output_bytes, output_byte_samples, context_bytes, context_byte_samples,
                search_result_bytes, search_result_byte_samples, context_searches,
                context_found, context_opened, context_cited, validated_discoveries
             ) VALUES (
                '2026-07-25', 2, '0.26.0', 'mcp', 'search', 'success',
                'result_bearing', '10_to_49_ms', 'not_applicable', 'not_applicable',
                'search', 2, 4, 0, 25, 1, 9000, 2, 0, 0, 400, 1, 200, 1,
                0, 0, 0, 0, 0
             )",
            [],
        )
        .unwrap();
    drop(connection);

    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["coverage"],
        "partial"
    );
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["measured_samples"],
        1
    );
    assert_eq!(
        report["measured"]["delivery"]["approximate_context_tokens"]["eligible_samples"],
        2
    );
    assert_eq!(
        report["estimated"]["approximate_avoided_context_tokens"]["coverage"],
        "partial"
    );

    enabled(ctx(&temp).args(["stats"]))
        .assert()
        .success()
        .stdout(predicate::str::contains("partial; 1/2 measured samples"));
}

#[test]
fn human_stats_use_product_actions_and_detail_is_explicit() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();

    enabled(ctx(&temp).args(["stats"]))
        .assert()
        .success()
        .stdout(predicate::str::contains("History retrieval"))
        .stdout(predicate::str::contains("Searches:"))
        .stdout(predicate::str::contains("Sessions/events opened:"))
        .stdout(predicate::str::contains("Records located:"))
        .stdout(predicate::str::contains("Code provenance"))
        .stdout(predicate::str::contains("Measured delivery"))
        .stdout(predicate::str::contains("Estimated savings"))
        .stdout(predicate::str::contains("50× raw-search benchmark"))
        .stdout(predicate::str::contains("  Calls:").not())
        .stdout(predicate::str::contains("CLI / MCP detail").not());

    enabled(ctx(&temp).args(["stats", "--detail"]))
        .assert()
        .success()
        .stdout(predicate::str::contains("CLI / MCP detail"))
        .stdout(predicate::str::contains("usage_operation: cli/doctor"));
}

#[test]
fn status_stays_health_focused_while_stats_owns_usage_reporting() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();

    let status = enabled(ctx(&temp).args(["status", "--format=json"]))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(status["local_usage"]["state"], "ready");
    assert!(status["local_usage"].get("summary").is_none());
    assert!(status["local_usage"].get("details").is_none());
    assert!(status["local_usage"].get("estimates").is_none());
    assert!(status.get("local_usage_action").is_none());
    assert_eq!(status["read_only"], true);

    enabled(ctx(&temp).args(["status"]))
        .assert()
        .success()
        .stdout(predicate::str::contains("local_usage: ready"))
        .stdout(predicate::str::contains("usage_calls:").not())
        .stdout(predicate::str::contains("Estimated savings").not());

    let stats = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(stats["local_usage"]["state"], "ready");
    assert!(stats["measured"].is_object());
    assert!(stats.get("estimated").is_some());
    assert_eq!(stats["local_only"], true);
    assert_eq!(stats["read_only"], true);
}

const LEGACY_DAILY_USAGE_SCHEMA: &str = r#"
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

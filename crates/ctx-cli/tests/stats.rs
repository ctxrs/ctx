mod support;

use std::fs;

use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;
use support::*;

fn enabled(command: &mut assert_cmd::Command) -> &mut assert_cmd::Command {
    command.env_remove("CTX_LOCAL_USAGE_ENABLED")
}

fn usage_db_path(temp: &tempfile::TempDir) -> std::path::PathBuf {
    data_root(temp).join("usage.sqlite")
}

fn definition(report: &Value, version: i64) -> &Value {
    report["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["definition_version"] == version)
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    connection: &Connection,
    definition_version: i64,
    surface: &str,
    operation: &str,
    value_class: &str,
    context_coverage: &str,
    calls: i64,
    results: i64,
    citations: i64,
    output_bytes: i64,
    context_bytes: i64,
    matched_bytes: i64,
) {
    connection
        .execute(
            "INSERT INTO daily_usage (
                day_utc, definition_version, ctx_version, surface, operation,
                outcome, value_class, duration_bucket, target_type, pro_outcome,
                context_coverage, calls, result_count, citation_count,
                delivered_output_bytes, delivered_context_bytes,
                matched_normalized_session_bytes
             ) VALUES (
                '2026-07-25', ?1, '0.26.0', ?2, ?3, 'success', ?4,
                '10_to_49_ms', 'not_applicable', 'not_applicable', ?5,
                ?6, ?7, ?8, ?9, ?10, ?11
             )",
            rusqlite::params![
                definition_version,
                surface,
                operation,
                value_class,
                context_coverage,
                calls,
                results,
                citations,
                output_bytes,
                context_bytes,
                matched_bytes,
            ],
        )
        .unwrap();
}

#[test]
fn pristine_disabled_and_malformed_stats_are_truthful_and_create_nothing() {
    let pristine = tempdir();
    let report = json_output(enabled(ctx(&pristine).args(["stats", "--format=json"])));
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["local_only"], true);
    assert_eq!(report["read_only"], true);
    assert_eq!(report["enabled"], true);
    assert_eq!(report["state"], "empty");
    assert_eq!(report["definitions"], serde_json::json!([]));
    assert!(report["estimates"].is_null());
    assert!(!usage_db_path(&pristine).exists());

    let disabled = tempdir();
    let report = json_output(
        ctx(&disabled)
            .args(["stats", "--format=json"])
            .env("CTX_LOCAL_USAGE_ENABLED", "false"),
    );
    assert_eq!(report["enabled"], false);
    assert_eq!(report["state"], "disabled");
    assert!(report.get("definitions").is_none());
    assert!(!usage_db_path(&disabled).exists());

    let malformed = tempdir();
    let marker = "PRIVATE_STATS_CONFIG_79af";
    fs::create_dir_all(data_root(&malformed)).unwrap();
    fs::write(
        data_root(&malformed).join("config.toml"),
        format!("[local_usage]\nenabled = nope\n# {marker}\n"),
    )
    .unwrap();
    let output = enabled(ctx(&malformed).args(["--color=always", "stats", "--format=json"]))
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty());
    let encoded = String::from_utf8(output.stderr).unwrap();
    assert!(!encoded.as_bytes().contains(&0x1b));
    let report: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(report["state"], "error");
    assert_eq!(report["error"]["code"], "local_usage_config_unavailable");
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains(malformed.path().to_string_lossy().as_ref()));
    assert!(!usage_db_path(&malformed).exists());
}

#[test]
fn definition_two_math_uses_only_complete_search_context_and_spec_coefficients() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let connection = Connection::open(usage_db_path(&temp)).unwrap();
    connection.execute("DELETE FROM daily_usage", []).unwrap();
    insert_row(
        &connection,
        2,
        "cli",
        "search",
        "result_bearing",
        "complete",
        1,
        2,
        0,
        60,
        19,
        59,
    );
    insert_row(
        &connection,
        2,
        "mcp",
        "search",
        "result_bearing",
        "unavailable",
        1,
        1,
        0,
        700,
        0,
        0,
    );
    drop(connection);

    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(
        report
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "definitions",
            "enabled",
            "estimates",
            "local_only",
            "read_only",
            "retention_days",
            "schema_version",
            "state",
        ])
    );
    let current = definition(&report, 2);
    assert_eq!(current["summary"]["calls"], 2);
    assert_eq!(current["summary"]["delivered_output_bytes"], 760);
    assert_eq!(current["summary"]["delivered_context_bytes"], 19);
    assert_eq!(current["summary"]["matched_normalized_session_bytes"], 59);
    assert_eq!(current["summary"]["complete_context_eligible_calls"], 1);
    assert_eq!(current["summary"]["unavailable_context_eligible_calls"], 1);
    assert_eq!(current["by_operation"].as_array().unwrap().len(), 2);
    assert_eq!(
        current["duration_buckets"][0]["duration_bucket"],
        "10_to_49_ms"
    );
    assert_eq!(current["duration_buckets"][0]["calls"], 2);

    let approximate = &report["estimates"]["approximate_context_tokens"];
    assert_eq!(
        approximate["coefficient_version"],
        "utf8_token_equivalent_range_v1"
    );
    assert_eq!(approximate["delivered_context_bytes"], 19);
    assert_eq!(approximate["low"], 3);
    assert_eq!(approximate["central"], 4);
    assert_eq!(approximate["high"], 7);

    let reduction = &report["estimates"]["estimated_context_reduction"];
    assert_eq!(
        reduction["estimate_model_version"],
        "matched_normalized_sessions_v1"
    );
    assert_eq!(reduction["covered_calls"], 1);
    assert_eq!(reduction["unavailable_calls"], 1);
    assert_eq!(reduction["comparison_baseline_bytes"], 59);
    assert_eq!(reduction["observed_delivered_context_bytes"], 19);
    assert_eq!(reduction["estimated_avoided_context_bytes"], 40);
    assert_eq!(reduction["low"], 8);
    assert_eq!(reduction["central"], 10);
    assert_eq!(reduction["high"], 16);

    let encoded = serde_json::to_string(&report).unwrap();
    for forbidden in [
        "time_saved",
        "open_credit",
        "blame_credit",
        "avoided_search_token_multiplier",
        "50×",
        "dollars",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "{forbidden} survived: {encoded}"
        );
    }
}

#[test]
fn definitions_are_reported_separately_and_definition_one_never_drives_estimates() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let connection = Connection::open(usage_db_path(&temp)).unwrap();
    connection.execute("DELETE FROM daily_usage", []).unwrap();
    insert_row(
        &connection,
        1,
        "mcp",
        "search",
        "result_bearing",
        "not_applicable",
        3,
        6,
        0,
        1_500,
        0,
        0,
    );
    insert_row(
        &connection,
        2,
        "cli",
        "doctor",
        "not_applicable",
        "not_applicable",
        1,
        0,
        0,
        0,
        0,
        0,
    );
    drop(connection);

    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(
        report["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|definition| definition["definition_version"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let legacy = definition(&report, 1);
    assert_eq!(legacy["summary"]["calls"], 3);
    assert_eq!(legacy["summary"]["delivered_output_bytes"], 1_500);
    assert_eq!(legacy["summary"]["complete_context_eligible_calls"], 0);
    assert_eq!(definition(&report, 2)["summary"]["calls"], 1);
    assert!(
        report["estimates"].is_null(),
        "definition-one bytes and non-search definition-two rows cannot drive estimates"
    );
}

#[test]
fn human_stats_label_measured_and_estimated_sections_without_time_claims() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let connection = Connection::open(usage_db_path(&temp)).unwrap();
    connection.execute("DELETE FROM daily_usage", []).unwrap();
    insert_row(
        &connection,
        2,
        "cli",
        "search",
        "result_bearing",
        "complete",
        1,
        1,
        0,
        20,
        20,
        100,
    );
    drop(connection);

    enabled(ctx(&temp).args(["stats"]))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Measured local facts · definition 2",
        ))
        .stdout(predicate::str::contains("Approximate token-equivalents"))
        .stdout(predicate::str::contains("Estimated context reduction"))
        .stdout(predicate::str::contains("matched_normalized_sessions_v1"))
        .stdout(predicate::str::contains("time saved").not())
        .stdout(predicate::str::contains("blame credit").not())
        .stdout(predicate::str::contains("50×").not());

    enabled(ctx(&temp).args(["stats", "--detail"]))
        .assert()
        .success()
        .stdout(predicate::str::contains("cli/search"))
        .stdout(predicate::str::contains("20 covered"))
        .stdout(predicate::str::contains("1 complete"));
}

#[test]
fn stats_are_self_excluding_and_status_remains_health_only() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let usage_path = usage_db_path(&temp);
    let before = fs::read(&usage_path).unwrap();
    for args in [
        &["stats"][..],
        &["stats", "--detail"],
        &["stats", "--format=json"],
    ] {
        enabled(ctx(&temp).args(args)).assert().success();
    }
    assert_eq!(fs::read(&usage_path).unwrap(), before);

    let status = json_output(enabled(ctx(&temp).args(["status", "--format=json"])));
    assert_eq!(status["local_usage"]["state"], "ready");
    assert!(status["local_usage"].get("summary").is_none());
    assert!(status["local_usage"].get("definitions").is_none());
    assert!(status["local_usage"].get("estimates").is_none());
    assert_eq!(status["read_only"], true);
    assert_eq!(fs::read(&usage_path).unwrap(), before);
}

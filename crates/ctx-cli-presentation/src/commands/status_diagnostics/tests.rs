use super::*;
use crate::{
    output::JsonOutputFormat,
    test_support::assert_fits,
    ui::{ColorMode, RenderContext, StreamKind, TestContext},
};
use clap::Parser;

#[derive(Parser)]
struct StatusCli {
    #[command(flatten)]
    status: crate::commands::StatusArgs,
}

#[test]
fn sampled_status_bounds_and_usage_conflict() {
    for seconds in ["1", "60"] {
        let args =
            StatusCli::try_parse_from(["status", "--sample", seconds, "--format", "json"]).unwrap();
        assert_eq!(args.status.sample, Some(seconds.parse().unwrap()));
        assert!(args.status.format.is_json());
    }
    for seconds in ["0", "61", "-1", "1.5", "five"] {
        assert!(StatusCli::try_parse_from(["status", "--sample", seconds]).is_err());
    }
    assert!(StatusCli::try_parse_from(["status", "--sample", "1", "--usage", "reset"]).is_err());
    let normal = StatusCli::try_parse_from(["status"]).unwrap();
    assert!(normal.status.sample.is_none());
    assert!(matches!(normal.status.format, JsonOutputFormat::Text));
}

fn observation() -> Value {
    json!({
        "elapsed_ms": 2500,
        "daemon": {"state": "running", "heartbeat_age_ms": 20},
        "refresh": {
            "request_state": "running", "trigger": "periodic", "trigger_provenance": "daemon_scheduler",
            "progress": {"whole_run_stage": "reading", "completed_records": 1234, "completed_bytes": 4096, "providers": ["codex"]},
            "scanned_routes": 2, "certified_source_count": 2, "certified_source_bytes": 8192,
            "generation_changed": false, "timings_us": {"discovery": 0}
        },
        "observed": {
            "cpu": {"status": "observed", "elapsed_ms": 2500, "cpu_ms": 250.0, "percent_one_core": 10.0},
            "scheduler": {"status": "observed", "work_cycles": 3, "no_work_cycles": 0, "work_cycles_per_second": 1.2}
        }
    })
}

#[test]
fn diagnostic_projection_preserves_missing_values_and_measured_zero() {
    let report = StatusDiagnosticReport::from_observation(2, &observation()).to_json();
    assert_eq!(report["observed"]["cpu"]["percent_one_core"], 10.0);
    assert_eq!(
        report["observed"]["scheduler"]["work_cycles_per_second"],
        1.2
    );
    assert_eq!(report["observed"]["scheduler"]["no_work_cycles"], 0);
    assert!(report["observed"]["scheduler"]["ipc_signals"].is_null());
    assert_eq!(report["refresh"]["timings_us"]["discovery"], 0);
    assert!(report["refresh"]["timings_us"]["commit"].is_null());
    let empty = StatusDiagnosticReport::from_observation(1, &Value::Null).to_json();
    assert_eq!(empty["daemon"]["state"], "unknown");
    assert!(empty["refresh"]["processed_records"].is_null());
    assert!(empty["refresh"]["providers"].is_null());
}

#[test]
fn diagnostic_build_identity_comes_only_from_the_cli_and_platform_constants() {
    let marker = "SECRET_persisted_version_16b4";
    let mut source = observation();
    for key in ["ctx_version", "version", "os", "arch"] {
        source[key] = json!(marker);
        source["daemon"][key] = json!(marker);
    }
    let unbound = StatusDiagnosticReport::from_observation(1, &source);
    assert_eq!(unbound.to_json()["ctx_version"], "unknown");
    let report = unbound.with_product_identity(ProductBuildIdentity::new("2.1.3"));
    let value = report.to_json();
    assert_eq!(value["ctx_version"], "2.1.3");
    assert_eq!(value["os"], std::env::consts::OS);
    assert_eq!(value["arch"], std::env::consts::ARCH);
    assert!(!value.to_string().contains(marker));
    for width in [32, 48, 80, 120] {
        let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width));
        let document = render_status_diagnostics(&context, &report);
        assert_fits(&document, &context);
        let plain = document.render_plain();
        let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains("ctx version 2.1.3"));
        assert!(normalized.contains(&format!("OS {}", std::env::consts::OS)));
        assert!(normalized.contains(&format!("Architecture {}", std::env::consts::ARCH)));
        assert!(!plain.contains(marker));
    }
}

#[test]
fn private_strings_and_unrecognized_enums_never_escape_the_projection() {
    let marker = "SECRET_canary_example_94d7";
    let mut source = observation();
    source["daemon"]["path"] = json!(format!("/private/{marker}"));
    source["daemon"]["process_creation_token"] = json!(marker);
    source["refresh"]["request_id"] = json!(marker);
    source["refresh"]["last_error"] = json!(marker);
    source["refresh"]["reason"] = json!(marker);
    source["refresh"]["trigger"] = json!(marker);
    source["refresh"]["trigger_provenance"] = json!(marker);
    source["refresh"]["progress"]["current_source"] = json!(marker);
    source["refresh"]["progress"]["whole_run_stage"] = json!(marker);
    source["refresh"]["progress"]["providers"] = json!(["codex", marker]);
    source["refresh"]["receipt"] = json!({"token": marker, "repo_id": marker});
    source["refresh"]["timings_us"][marker] = json!(123);
    source["observed"]["cpu"]["status"] = json!(marker);
    source["observed"]["cpu"]["identity"] = json!(marker);
    let report = StatusDiagnosticReport::from_observation(5, &source);
    let output = report.to_json();
    assert!(!output.to_string().contains(marker));
    assert!(output["refresh"]["providers"].is_null());
    assert!(output["observed"]["cpu"]["cpu_ms"].is_null());
    for width in [32, 48, 80, 120] {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Always),
        );
        let document = render_status_diagnostics(&context, &report);
        assert_fits(&document, &context);
        let plain = document.render_plain();
        assert!(!plain.contains(marker));
        assert!(!plain.contains('\u{1b}'));
    }
}

#[test]
fn completed_and_stopped_records_do_not_claim_live_progress() {
    let mut source = observation();
    source["refresh"]["request_state"] = json!("published");
    let completed = StatusDiagnosticReport::from_observation(1, &source);
    assert!(!completed.refresh.active());
    assert!(completed.to_json()["refresh"]["processed_records"].is_null());
    source["refresh"]["request_state"] = json!("running");
    source["daemon"]["state"] = json!("stopped");
    let report = StatusDiagnosticReport::from_observation(1, &source);
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 120));
    assert!(render_status_diagnostics(&context, &report)
        .render_plain()
        .contains("recorded running; daemon stopped"));
}

#[test]
fn invalid_counters_and_unavailable_measurements_do_not_wrap_or_become_zero() {
    let mut source = observation();
    source["refresh"]["scanned_routes"] = json!(u64::MAX);
    source["refresh"]["certified_source_count"] = json!(-1);
    source["observed"]["cpu"]["percent_one_core"] = json!(240.0);
    let report = StatusDiagnosticReport::from_observation(1, &source).to_json();
    assert_eq!(report["observed"]["cpu"]["percent_one_core"], 240.0);
    assert!(report["refresh"]["scanned_routes"].is_null());
    assert!(report["refresh"]["certified_source_count"].is_null());
    for status in [
        "permission_denied",
        "process_exited",
        "daemon_restarted",
        "counter_reset",
        "identity_unknown",
    ] {
        source["observed"]["scheduler"]["status"] = json!(status);
        let report = StatusDiagnosticReport::from_observation(1, &source).to_json();
        assert!(report["observed"]["scheduler"]["work_cycles"].is_null());
        assert!(report["observed"]["scheduler"]["work_cycles_per_second"].is_null());
    }
}

#[test]
fn skipped_refresh_names_only_recorded_reasons() {
    let job = json!({"status": "skipped", "reason": "retry_backoff", "trigger": "periodic"});
    let activity = RefreshActivity::from_job(&job);
    assert!(!activity.active());
    assert_eq!(activity.to_json()["state"], "skipped");
    assert_eq!(activity.to_json()["reason"], "retry_backoff");
    assert_eq!(activity.work_description(), "unknown; scheduled refresh");
}

#[test]
fn participating_providers_use_the_refresh_producers_vocabulary() {
    let activity = RefreshActivity::from_job(&json!({
        "request_state": "running",
        "progress": {"providers": ["opencode", "claude", "codex", "opencode"]}
    }));
    assert_eq!(
        activity.to_json()["providers"],
        json!(["claude", "codex", "opencode"])
    );
    assert_eq!(
        activity.provider_names().as_deref(),
        Some("Claude Code, Codex, OpenCode")
    );
}

#[test]
fn producer_enums_and_published_metadata_codes_are_recognized() {
    use ctx_history_refresh::{
        RefreshRequestState as State, RefreshRequestTrigger as RequestTrigger,
        RefreshRuntimeMetadata, SourceBackedRefreshStage as WholeRunStage,
    };

    for state in [
        State::AdmissionPending,
        State::Queued,
        State::Running,
        State::Published,
        State::Failed,
    ] {
        let activity = RefreshActivity::from_job(&json!({"request_state": state.as_str()}));
        assert_eq!(activity.to_json()["state"], state.as_str());
        assert_eq!(activity.active(), state.is_active());
    }
    for trigger in [
        RequestTrigger::Setup,
        RequestTrigger::Search,
        RequestTrigger::Import,
    ] {
        let activity = RefreshActivity::from_job(&json!({"trigger": trigger.as_str()}));
        assert_eq!(activity.to_json()["trigger"], trigger.as_str());
    }
    for metadata in [
        RefreshRuntimeMetadata::default(),
        RefreshRuntimeMetadata::periodic(),
    ] {
        let activity = RefreshActivity::from_job(&json!({
            "trigger": metadata.trigger, "trigger_provenance": metadata.trigger_provenance,
        }));
        assert_eq!(activity.to_json()["trigger"], metadata.trigger);
        assert_eq!(
            activity.to_json()["trigger_provenance"],
            metadata.trigger_provenance
        );
    }
    for stage in [
        WholeRunStage::Preparing,
        WholeRunStage::Reading,
        WholeRunStage::Merging,
        WholeRunStage::Syncing,
        WholeRunStage::PhysicalVerification,
        WholeRunStage::LogicalVerification,
        WholeRunStage::Activation,
        WholeRunStage::Complete,
        WholeRunStage::Failed,
    ] {
        let activity = RefreshActivity::from_job(&json!({
            "request_state": "running", "progress": {"whole_run_stage": stage.as_str()},
        }));
        assert_eq!(activity.to_json()["stage"], stage.as_str());
        assert!(activity
            .work_description()
            .starts_with(&stage.as_str().replace('_', " ")));
    }
    // These are string-valued producer contracts, not serde enum names:
    // refresh engine admission/runtime/recovery and history-cli import metadata.
    for (provenance, cause) in [
        ("manual", "search request"),
        ("autostart", "automatic daemon start"),
        ("setup_command", "setup command"),
        ("import_command", "import command"),
        ("automatic_provider", "automatic provider refresh"),
        ("daemon_scheduler", "scheduled refresh"),
        ("explicit_source_catalog", "explicit source selection"),
        ("commit_payload", "publication recovery"),
        ("automatic_provider_refresh", "automatic provider refresh"),
        ("history_source_plugin", "history source plugin"),
    ] {
        let activity = RefreshActivity::from_job(&json!({
            "trigger": "search", "trigger_provenance": provenance,
        }));
        assert_eq!(activity.to_json()["trigger_provenance"], provenance);
        assert!(activity.work_description().ends_with(cause));
    }
    let recovered = RefreshActivity::from_job(&json!({
        "trigger": "recovery", "trigger_provenance": "commit_payload",
    }));
    assert_eq!(recovered.to_json()["trigger"], "recovery");
}

#[test]
fn captured_published_job_keeps_real_codes_and_recorded_totals() {
    // Reviewed projection of an actual published job from a synthetic Codex
    // workload. Only static vocabulary and aggregate counts/timings were kept;
    // identifiers, timestamps, paths, receipts and source contents were removed.
    let job = json!({
        "status": "completed", "request_state": "published", "trigger": "import",
        "trigger_provenance": "import_command", "scanned_routes": 3,
        "certified_source_count": 10001, "certified_source_bytes": 44407771,
        "generation_changed": false,
        "timings_us": {"commit": 133249, "discovery": 1157, "publication_probe": 33, "scan_stage": 11973772},
        "progress": {"phase": "published", "whole_run_stage": "complete", "providers": ["codex"]},
    });
    let activity = RefreshActivity::from_job(&job);
    assert_eq!(
        activity.to_json(),
        json!({
            "state": "published", "trigger": "import", "trigger_provenance": "import_command",
            "stage": "complete", "reason": "unknown", "providers": ["codex"],
            "processed_records": null, "processed_bytes": null,
            "scanned_routes": 3, "certified_source_count": 10001, "certified_source_bytes": 44407771,
            "generation_changed": false,
            "timings_us": {"commit": 133249, "discovery": 1157, "publication_probe": 33, "scan_stage": 11973772},
        })
    );
    assert!(!activity.active());
    assert_eq!(activity.work_description(), "complete; import command");

    // The same run's import report publishes different request metadata from
    // the persisted job. Do not rewrite one producer's spelling into the other.
    let import_metadata =
        json!({"trigger": "import", "trigger_provenance": "automatic_provider_refresh"});
    let activity = RefreshActivity::from_job(&import_metadata);
    assert_eq!(
        activity.to_json()["trigger_provenance"],
        "automatic_provider_refresh"
    );
    assert_eq!(
        activity.work_description(),
        "unknown; automatic provider refresh"
    );
}

#[test]
fn advisory_work_context_keeps_only_closed_recorded_progress() {
    let marker = "SECRET_ADVISORY_CONTEXT_92d1";
    let mut source = observation();
    source["daemon"]["state"] = json!("unknown");
    source["blame"] = json!({
        "writer_observed": true, "phase": "indexing", "completed_sources": 0,
        "total_sources": 4, "applied_changes": 17, "core_generation_id": marker, "error": marker,
    });
    source["semantic"] = json!({
        "status": "budget_exhausted", "source_work_remaining": true, "last_run_at_ms": 1,
        "core_generation_id": marker, "last_error": marker, "model_key": marker,
        "semantic_runtime_active": true,
    });
    let report = StatusDiagnosticReport::from_observation(1, &source);
    let value = report.to_json();
    assert_eq!(
        value["blame"],
        json!({
            "writer_observed": true, "phase": "indexing", "completed_sources": 0,
            "total_sources": 4, "applied_changes": 17,
        })
    );
    assert_eq!(
        value["semantic"],
        json!({"last_recorded_status": "budget_exhausted", "work_remaining": true})
    );
    assert_eq!(value["daemon"]["state"], "unknown");
    assert_eq!(value["observed"]["cpu"]["percent_one_core"], 10.0);
    assert!(!value.to_string().contains(marker));
    for width in [32, 48, 80, 120] {
        let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width));
        let document = render_status_diagnostics(&context, &report);
        assert_fits(&document, &context);
        let plain = document.render_plain();
        let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains("Blame writer observed true"));
        assert!(normalized.contains("Last recorded semantic job budget exhausted"));
        assert!(normalized.contains("not tied to the daemon PID"));
        assert!(normalized.contains("may be stale"));
        assert!(normalized.contains("without activity attribution"));
        assert!(!plain.contains(marker));
    }
}

#[test]
fn unavailable_or_unheld_blame_writer_discards_stale_progress() {
    let mut source = observation();
    source["blame"] = json!({"phase": "indexing", "completed_sources": 3, "total_sources": 4, "applied_changes": 0});
    for writer in [Value::Null, json!(false), json!("true")] {
        source["blame"]["writer_observed"] = writer.clone();
        let value = StatusDiagnosticReport::from_observation(1, &source).to_json();
        assert_eq!(value["blame"]["writer_observed"], json!(writer.as_bool()));
        assert_eq!(value["blame"]["phase"], "unknown");
        for key in BLAME_COUNTERS {
            assert!(value["blame"][key].is_null());
        }
    }
    source["blame"]["writer_observed"] = json!(true);
    for phase in ["snapshot_unavailable", "future_phase"] {
        source["blame"]["phase"] = json!(phase);
        let value = StatusDiagnosticReport::from_observation(1, &source).to_json();
        assert_eq!(value["blame"]["writer_observed"], true);
        for key in BLAME_COUNTERS {
            assert!(value["blame"][key].is_null());
        }
    }
    source["blame"]["phase"] = json!("indexing");
    source["blame"]["applied_changes"] = json!(u64::MAX);
    assert!(
        StatusDiagnosticReport::from_observation(1, &source).to_json()["blame"]["applied_changes"]
            .is_null()
    );
}

#[test]
fn advisory_phase_and_semantic_status_codes_match_published_vocabulary() {
    for phase in [
        "snapshot_unavailable",
        "waiting_for_writer",
        "preparing",
        "indexing",
        "publishing",
        "complete",
    ] {
        let report = StatusDiagnosticReport::from_observation(
            1,
            &json!({"blame": {"writer_observed": true, "phase": phase}}),
        )
        .to_json();
        assert_eq!(report["blame"]["phase"], phase);
    }
    for status in [
        "disabled",
        "ready",
        "skipped",
        "budget_exhausted",
        "failed",
        "resource_deferred",
    ] {
        let report = StatusDiagnosticReport::from_observation(
            1,
            &json!({"semantic": {"status": status, "source_work_remaining": false}}),
        )
        .to_json();
        assert_eq!(report["semantic"]["last_recorded_status"], status);
        assert_eq!(report["semantic"]["work_remaining"], false);
    }
    let report = StatusDiagnosticReport::from_observation(1, &json!({"semantic": {
        "status": "future_status", "source_work_remaining": "false", "semantic_runtime_active": true,
    }})).to_json();
    assert_eq!(
        report["semantic"],
        json!({"last_recorded_status": "unknown", "work_remaining": null})
    );
    let empty = StatusDiagnosticReport::from_observation(1, &Value::Null).to_json();
    assert!(empty["blame"]["writer_observed"].is_null());
    assert_eq!(empty["semantic"], report["semantic"]);
}

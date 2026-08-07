use std::time::Duration;

use clap::Parser as _;
use ctx_history_core::platform_security::restrict_private_directory;

use super::{
    classify_blame_json, pro_outcome_from_attribution, read_report, CliUsage, CompletedOperation,
    ContextCoverage, McpInvocation, ProOutcome, ResultObservationAction, SearchContextObservation,
    ValueClass,
};

mod mcp_tests;
mod migration_tests;
mod persistence_tests;
mod schema_tests;

#[test]
fn blame_usage_consumes_authoritative_outcome_for_cli_and_mcp() {
    for (attribution, expected) in [
        (
            ctx_pro_host_protocol::BlameAttribution::Proven,
            ProOutcome::Produced,
        ),
        (
            ctx_pro_host_protocol::BlameAttribution::Possible,
            ProOutcome::Possible,
        ),
        (
            ctx_pro_host_protocol::BlameAttribution::Conflicting,
            ProOutcome::Possible,
        ),
        (
            ctx_pro_host_protocol::BlameAttribution::None,
            ProOutcome::None,
        ),
    ] {
        assert_eq!(pro_outcome_from_attribution(attribution), expected);
        let structured = serde_json::json!({
            "outcome": {
                "attribution": match attribution {
                    ctx_pro_host_protocol::BlameAttribution::Proven => "proven",
                    ctx_pro_host_protocol::BlameAttribution::Possible => "possible",
                    ctx_pro_host_protocol::BlameAttribution::Conflicting => "conflicting",
                    ctx_pro_host_protocol::BlameAttribution::None => "none",
                }
            },
            "matches": []
        });
        assert_eq!(classify_blame_json(Some(&structured)), expected);
    }
}

pub(super) fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    root
}

pub(super) fn operation(name: &'static str) -> CompletedOperation {
    let mut operation = CompletedOperation::cli(name, true, Duration::from_millis(4));
    operation.delivered_output_bytes = 1;
    operation
}

#[test]
fn cli_definition_two_search_uses_only_dedicated_exact_context_observation() {
    let mut usage = CliUsage::excluded();
    usage.operation = Some("search");
    usage.set_result_observation(ResultObservationAction::Search, 3, 99, 8_000);
    usage.set_search_context_observation(SearchContextObservation::complete(320, 1_000).unwrap());
    usage.set_measured_output_bytes(701);
    let completed = usage.completed(true, Duration::from_millis(17)).unwrap();
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::ResultBearing, 3, 0)
    );
    assert_eq!(
        completed.context_metadata_for_test(),
        (ContextCoverage::Complete, 320, 1_000)
    );
    assert_eq!(completed.delivered_output_bytes, 701);
}

#[test]
fn search_without_complete_adapter_is_unavailable_and_failures_discard_counts() {
    let mut usage = CliUsage::excluded();
    usage.operation = Some("search");
    usage.set_result_observation(ResultObservationAction::Search, 2, 0, 400);
    let completed = usage.completed(true, Duration::ZERO).unwrap();
    assert_eq!(
        completed.context_metadata_for_test(),
        (ContextCoverage::Unavailable, 0, 0)
    );

    let failed = usage.completed(false, Duration::ZERO).unwrap();
    assert_eq!(
        failed.result_metadata_for_test(),
        (ValueClass::NotApplicable, 0, 0)
    );
    assert_eq!(
        failed.context_metadata_for_test(),
        (ContextCoverage::NotApplicable, 0, 0)
    );
}

#[test]
fn cli_show_operations_are_typed_and_stats_status_are_excluded() {
    for (args, expected) in [
        (vec!["ctx", "show", "session", "abc"], "show_session"),
        (vec!["ctx", "show", "event", "abc"], "show_event"),
        (vec!["ctx", "list", "events"], "show_event"),
    ] {
        let cli = crate::Cli::try_parse_from(args).unwrap();
        assert_eq!(
            CliUsage::from_command(&cli.command).operation,
            Some(expected)
        );
    }
    for args in [vec!["ctx", "status"], vec!["ctx", "stats"]] {
        let cli = crate::Cli::try_parse_from(args).unwrap();
        assert!(CliUsage::from_command(&cli.command).operation.is_none());
    }
}

#[test]
fn cli_and_mcp_use_identical_duration_bucket_boundaries() {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"structuredContent": {}}
    });
    for (duration, expected) in [
        (Duration::ZERO, "under_10_ms"),
        (Duration::from_nanos(9_999_999), "under_10_ms"),
        (Duration::from_millis(10), "10_to_49_ms"),
        (Duration::from_nanos(49_999_999), "10_to_49_ms"),
        (Duration::from_millis(50), "50_to_249_ms"),
        (Duration::from_nanos(249_999_999), "50_to_249_ms"),
        (Duration::from_millis(250), "250_to_999_ms"),
        (Duration::from_nanos(999_999_999), "250_to_999_ms"),
        (Duration::from_secs(1), "1_to_4_s"),
        (Duration::from_nanos(4_999_999_999), "1_to_4_s"),
        (Duration::from_secs(5), "5_to_29_s"),
        (Duration::from_nanos(29_999_999_999), "5_to_29_s"),
        (Duration::from_secs(30), "30_s_or_more"),
    ] {
        let cli = CompletedOperation::cli("doctor", true, duration);
        let mcp = McpInvocation::recognized("status")
            .unwrap()
            .completed(&response, duration, 1);
        assert_eq!(cli.duration_bucket_for_test(), expected);
        assert_eq!(mcp.duration_bucket_for_test(), expected);
    }
}

#[test]
fn empty_and_disabled_reports_follow_state_dependent_json_contract() {
    let root = private_tempdir();
    let disabled = serde_json::to_value(read_report(root.path(), false, true)).unwrap();
    assert_eq!(disabled["state"], "disabled");
    assert!(disabled.get("definitions").is_none());
    assert!(disabled.get("estimates").is_none());

    let empty = serde_json::to_value(read_report(root.path(), true, true)).unwrap();
    assert_eq!(empty["state"], "empty");
    assert_eq!(empty["definitions"].as_array().unwrap().len(), 0);
    assert!(empty.get("estimates").is_none());
    assert_eq!(empty["local_only"], true);
    assert_eq!(empty["read_only"], true);
}

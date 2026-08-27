use std::{cell::Cell, time::Duration};

use ctx_history_platform::platform_security::restrict_private_directory;

use super::{
    read_report, record_best_effort, CliUsage, CompletedOperation, ContextCoverage,
    LocalUsageStorageAuthority, McpCompletionFacts, McpInvocation, ResultObservationAction,
    SearchContextObservation, Surface, UsageControlSnapshot, ValueClass, DEFINITION_VERSION,
};
use crate::operation_descriptor::{LocalUsageOperation, ObservedMcpProductOperation};

mod mcp_tests;
mod migration_tests;
mod persistence_tests;
mod schema_tests;

pub(super) fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    root
}

pub(super) fn operation(name: &'static str) -> CompletedOperation {
    let operation_kind = match name {
        "doctor" => LocalUsageOperation::Doctor,
        "search" => LocalUsageOperation::Search,
        _ => panic!("unsupported test operation {name}"),
    };
    let mut operation = CompletedOperation::cli(operation_kind, true, Duration::from_millis(4));
    operation.delivered_output_bytes = 1;
    operation
}

#[test]
fn cli_definition_two_search_uses_only_dedicated_exact_context_observation() {
    let mut usage = CliUsage::excluded();
    usage.operation = Some(LocalUsageOperation::Search);
    usage.set_result_observation(ResultObservationAction::Search, 3, 8_000);
    usage.set_search_context_observation(SearchContextObservation::complete(320, 1_000).unwrap());
    usage.set_measured_output_bytes(701);
    let completed = usage.completed(true, Duration::from_millis(17)).unwrap();
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::ResultBearing, 3)
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
    usage.operation = Some(LocalUsageOperation::Search);
    usage.set_result_observation(ResultObservationAction::Search, 2, 400);
    let completed = usage.completed(true, Duration::ZERO).unwrap();
    assert_eq!(
        completed.context_metadata_for_test(),
        (ContextCoverage::Unavailable, 0, 0)
    );

    let failed = usage.completed(false, Duration::ZERO).unwrap();
    assert_eq!(
        failed.result_metadata_for_test(),
        (ValueClass::NotApplicable, 0)
    );
    assert_eq!(
        failed.context_metadata_for_test(),
        (ContextCoverage::NotApplicable, 0, 0)
    );
}

#[test]
fn cli_and_mcp_use_identical_duration_bucket_boundaries() {
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
        let cli = CompletedOperation::cli(LocalUsageOperation::Doctor, true, duration);
        let mcp = McpInvocation::from_operation(ObservedMcpProductOperation::Status).completed(
            &McpCompletionFacts {
                delivered_output_bytes: 1,
                ..McpCompletionFacts::default()
            },
            duration,
        );
        assert_eq!(cli.duration_bucket_for_test(), expected);
        assert_eq!(mcp.duration_bucket_for_test(), expected);
    }
}

#[test]
fn blame_completion_retains_only_core_observed_transport_facts() {
    let completed = CompletedOperation::blame(Surface::Mcp, true, 512, Duration::from_millis(51));
    assert_eq!(completed.definition_version_for_test(), DEFINITION_VERSION);
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::NotApplicable, 0)
    );
    assert_eq!(completed.duration_bucket_for_test(), "50_to_249_ms");

    let failed = CompletedOperation::blame(Surface::Cli, false, 0, Duration::ZERO);
    assert_eq!(
        failed.result_metadata_for_test(),
        (ValueClass::NotApplicable, 0)
    );
}

#[test]
fn empty_and_disabled_reports_follow_state_dependent_json_contract() {
    let root = private_tempdir();
    let disabled = serde_json::to_value(read_report(root.path(), false, true)).unwrap();
    assert_eq!(disabled["schema_version"], 3);
    assert_eq!(disabled["state"], "disabled");
    assert!(disabled.get("definitions").is_none());
    assert!(disabled.get("estimates").is_none());

    let empty = serde_json::to_value(read_report(root.path(), true, true)).unwrap();
    assert_eq!(empty["schema_version"], 3);
    assert_eq!(empty["state"], "empty");
    assert_eq!(empty["definitions"].as_array().unwrap().len(), 0);
    assert!(empty.get("estimates").is_none());
    assert_eq!(empty["local_only"], true);
    assert_eq!(empty["read_only"], true);
}

#[test]
fn disabled_cli_recording_never_invokes_completion_adapter_or_opens_sqlite() {
    let root = private_tempdir();
    let database = root.path().join("usage.sqlite");
    let authority = LocalUsageStorageAuthority::new(database.clone(), "1.0.0");
    let control = UsageControlSnapshot::unversioned(false);
    let adapter_called = Cell::new(false);

    record_best_effort(&authority, &control, || {
        adapter_called.set(true);
        Some(operation("doctor"))
    });

    assert!(!adapter_called.get());
    assert!(!database.exists());
}

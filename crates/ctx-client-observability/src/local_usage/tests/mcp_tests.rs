use std::{cell::Cell, time::Duration};

use super::private_tempdir;
use crate::{
    local_usage::{
        ContextCoverage, LocalUsageStorageAuthority, McpCompletionFacts, McpInvocation,
        McpUsageRecorder, SearchContextObservation, UsageControlSnapshot, ValueClass,
    },
    operation_descriptor::ObservedMcpProductOperation,
};

fn invocation(operation: ObservedMcpProductOperation) -> McpInvocation {
    McpInvocation::from_operation(operation)
}

#[test]
fn mcp_search_records_transport_bytes_and_adapter_supplied_canonical_context() {
    let mut invocation = invocation(ObservedMcpProductOperation::Search);
    invocation.bind_search_context(SearchContextObservation::complete(12, 40).unwrap());
    let completed = invocation.completed(
        &McpCompletionFacts {
            result_count: Some(1),
            delivered_output_bytes: 777,
            ..McpCompletionFacts::default()
        },
        Duration::from_millis(5),
    );
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::ResultBearing, 1)
    );
    assert_eq!(
        completed.context_metadata_for_test(),
        (ContextCoverage::Complete, 12, 40)
    );
    assert_eq!(completed.delivered_output_bytes, 777);
}

#[test]
fn recorder_counts_one_delivered_mcp_response_once() {
    let root = private_tempdir();
    let authority = LocalUsageStorageAuthority::new(root.path().join("usage.sqlite"), "1.0.0");
    let mut recorder =
        McpUsageRecorder::start(authority, || UsageControlSnapshot::unversioned(true));
    recorder.record_delivered(Duration::ZERO, || {
        Some((
            invocation(ObservedMcpProductOperation::Sources),
            McpCompletionFacts {
                result_count: Some(0),
                delivered_output_bytes: 123,
                ..McpCompletionFacts::default()
            },
        ))
    });
    let report = crate::local_usage::read_report(root.path(), true, true);
    let current = report.definitions.unwrap().remove(0);
    assert_eq!(current.ctx_versions, ["1.0.0"]);
    assert_eq!(current.summary.calls, 1);
    assert_eq!(current.summary.delivered_output_bytes, 123);
}

#[test]
fn disabled_recorder_never_invokes_the_completion_adapter_or_opens_sqlite() {
    let root = private_tempdir();
    let database = root.path().join("usage.sqlite");
    let authority = LocalUsageStorageAuthority::new(database.clone(), "1.0.0");
    let mut recorder =
        McpUsageRecorder::start(authority, || UsageControlSnapshot::unversioned(false));
    let adapter_called = Cell::new(false);

    recorder.record_delivered(Duration::ZERO, || {
        adapter_called.set(true);
        Some((
            invocation(ObservedMcpProductOperation::Search),
            McpCompletionFacts::default(),
        ))
    });

    assert!(!adapter_called.get());
    assert!(!database.exists());
}

#[test]
fn query_events_projects_to_show_event_without_raw_protocol_input() {
    let completed = invocation(ObservedMcpProductOperation::QueryEvents).completed(
        &McpCompletionFacts {
            result_count: Some(2),
            delivered_output_bytes: 321,
            ..McpCompletionFacts::default()
        },
        Duration::ZERO,
    );
    assert_eq!(
        completed.operation,
        crate::operation_descriptor::LocalUsageOperation::ShowEvent
    );
    assert_eq!(completed.result_count, 2);
}

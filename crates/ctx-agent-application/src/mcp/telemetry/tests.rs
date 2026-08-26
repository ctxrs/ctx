use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;

use super::*;
use ctx_agent_integrations::tool_backend::{
    ToolSearchBackend, ToolSearchConcentrationFacts, ToolSearchCopyClusterAvailability,
    ToolSearchDiversificationStatus, ToolSearchLiteralRootFacts, ToolSearchStopReason,
    ToolSearchTerminalFacts,
};
use ctx_client_observability::analytics::{
    SearchBackend, SearchCopyClusterAvailability, SearchDiversificationStatus,
    SearchLiteralRootFacts, SearchStopReason,
};

#[test]
fn search_execution_survives_a_response_without_structured_content() {
    let usage = ToolUsageFacts {
        search_execution: Some(ToolSearchTerminalFacts {
            output_duration: Some(Duration::from_millis(2)),
            output_served: Some(true),
            backend_requested: Some(ToolSearchBackend::Hybrid),
            backend_effective: Some(ToolSearchBackend::Lexical),
            query_executions: Some(2),
            candidate_rows: Some(7),
            concentration: Some(ToolSearchConcentrationFacts {
                candidate_sessions: 2,
                largest_session_candidate_count: 5,
                literal_roots: ToolSearchLiteralRootFacts::NotObservedDense,
                provider_copy_candidate_count: 1,
                copy_cluster_availability: ToolSearchCopyClusterAvailability::NotConstructedV1,
                diversification_status: ToolSearchDiversificationStatus::NotApplicable,
                diversification_changed_final_top_n: None,
            }),
            stop_reason: Some(ToolSearchStopReason::Exhausted),
            ..ToolSearchTerminalFacts::default()
        }),
        ..Default::default()
    };

    let metadata = result_metadata(McpToolKind::Search, &json!({"result": {}}), Some(&usage));

    let search = metadata.search.unwrap();
    assert_eq!(search.output_served, Some(true));
    assert_eq!(search.backend_requested, Some(SearchBackend::Hybrid));
    assert_eq!(search.backend_effective, Some(SearchBackend::Lexical));
    assert_eq!(search.health.query_executions, Some(2));
    assert_eq!(search.health.candidate_rows, Some(7));
    assert_eq!(search.health.stop_reason, Some(SearchStopReason::Exhausted));
    let concentration = search.health.concentration.unwrap();
    assert_eq!(concentration.candidate_sessions, 2);
    assert_eq!(
        concentration.literal_roots,
        SearchLiteralRootFacts::NotObservedDense
    );
    assert_eq!(
        concentration.copy_cluster_availability,
        SearchCopyClusterAvailability::NotConstructedV1
    );
    assert_eq!(
        concentration.diversification_status,
        SearchDiversificationStatus::NotApplicable
    );
}

#[test]
fn protocol_classification_maps_to_content_free_product_facts() {
    for (message, expected) in [
        (
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"search"}}),
            McpRequestObservation::ToolCall(McpObservedTool::Product(
                ObservedMcpProductOperation::Search,
            )),
        ),
        (
            json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"private-tool-never-retained"}}),
            McpRequestObservation::ToolCall(McpObservedTool::Unknown),
        ),
        (
            json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"blame"}}),
            McpRequestObservation::ToolCall(McpObservedTool::Unknown),
        ),
    ] {
        assert_eq!(
            request_observation(RequestDescriptor::from_message(&message)),
            expected
        );
    }
}

#[test]
fn telemetry_is_opt_in_and_never_retains_raw_error_or_cursor_values() {
    let delivered = Arc::new(Mutex::new(0));
    let disabled_delivered = delivered.clone();
    let mut disabled = McpTelemetry::start(false, move |_| {
        *disabled_delivered.lock().unwrap() += 1;
        Ok(())
    });
    disabled.record_delivered(
        RequestDescriptor::InvalidJson,
        Some(&json!({
            "error": {"code": -32700, "data": {"query": "private"}}
        })),
        None,
        Duration::ZERO,
    );
    disabled.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    assert_eq!(*delivered.lock().unwrap(), 0);

    let events = Arc::new(Mutex::new(String::new()));
    let recorded = events.clone();
    let mut telemetry = McpTelemetry::start(true, move |batch| {
        recorded.lock().unwrap().push_str(&format!("{batch:?}"));
        Ok(())
    });
    telemetry.record_delivered(
        RequestDescriptor::ToolCall {
            operation: McpToolKind::QueryEvents,
        },
        Some(&json!({"result": {"structuredContent": {
            "events": [{}, {}], "truncated": true, "next_cursor": "never-retained"
        }}})),
        None,
        Duration::ZERO,
    );
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    let serialized = events.lock().unwrap();
    assert!(!serialized.contains("never-retained"));
    assert!(!serialized.contains("private"));
}

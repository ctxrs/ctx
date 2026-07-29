use std::{fs, time::Duration};

use serde_json::json;

use super::private_tempdir;
use crate::local_usage::{
    ContextCoverage, McpInvocation, McpUsageRecorder, SearchContextObservation, ValueClass,
};

#[test]
fn mcp_search_records_transport_bytes_and_adapter_supplied_canonical_context() {
    let mut invocation = McpInvocation::recognized("search").unwrap();
    invocation.bind_search_context(SearchContextObservation::complete(12, 40).unwrap());
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "results": [{"snippet": "hello world!"}]
            }
        }
    });
    let completed = invocation.completed(&response, Duration::from_millis(5), 777);
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::ResultBearing, 1, 0)
    );
    assert_eq!(
        completed.context_metadata_for_test(),
        (ContextCoverage::Complete, 12, 40)
    );
    assert_eq!(completed.delivered_output_bytes, 777);
}

#[test]
fn mcp_blame_counts_unique_numbered_citations_only() {
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "matches": [],
                "evidence": [
                    {"number": 4},
                    {"number": 4},
                    {"number": 9},
                    {"not_a_number": 10}
                ]
            }
        }
    });
    let completed =
        McpInvocation::recognized("blame")
            .unwrap()
            .completed(&response, Duration::ZERO, 200);
    assert_eq!(completed.citation_count, 2);
}

#[test]
fn prefix_open_correlation_uses_returned_full_uuid_and_missing_id_is_unavailable() {
    let root = private_tempdir();
    let mut recorder = McpUsageRecorder::start(root.path().to_path_buf());
    let full = "11111111-2222-3333-4444-555555555555";
    let search = McpInvocation::recognized("search").unwrap();
    let search_response = json!({
        "result": {
            "structuredContent": {
                "results": [{
                    "result_scope": "session",
                    "result_type": "session_result",
                    "ctx_session_id": full
                }]
            }
        }
    });
    assert!(!recorder.correlate_delivered_for_test(&search, &search_response));

    let show = McpInvocation::recognized("show_session").unwrap();
    let show_response = json!({
        "result": {
            "structuredContent": {
                "ctx_session_id": full,
                "events": [{}]
            }
        }
    });
    assert!(recorder.correlate_delivered_for_test(&show, &show_response));

    let missing_id = json!({
        "result": {"structuredContent": {"events": [{}]}}
    });
    assert!(!recorder.correlate_delivered_for_test(&show, &missing_id));
}

#[test]
fn recorder_counts_one_delivered_mcp_response_once() {
    let root = private_tempdir();
    fs::write(root.path().join("config.toml"), "").unwrap();
    let mut recorder = McpUsageRecorder::start(root.path().to_path_buf());
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"structuredContent": {"sources": []}}
    });
    recorder.record_delivered(
        McpInvocation::recognized("sources").unwrap(),
        &response,
        Duration::ZERO,
        123,
    );
    let report = crate::local_usage::read_report(root.path(), true, true);
    let current = report.definitions.unwrap().remove(0);
    assert_eq!(current.summary.calls, 1);
    assert_eq!(current.summary.delivered_output_bytes, 123);
}

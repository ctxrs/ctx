use ctx_history_core::CaptureProvider;
use serde_json::{json, Value};

use super::{
    encoded_json_string_bytes, handle_protocol_message, request_id_is_accepted, search_request,
    tool_definitions, McpServerIdentity, RequestDescriptor, MCP_MAX_ENCODED_REQUEST_ID_BYTES,
    PROVIDER_ROOT_SELECTOR_PATTERN,
};
use crate::tool_backend::{
    ToolBackend, ToolExecutionError, ToolOperation, ToolOutcome, ToolSearchBackend,
};

struct UnusedBackend;

impl ToolBackend for UnusedBackend {
    fn execute(&self, _operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        panic!("request-ID validation must run before the backend")
    }

    fn parse_provider(&self, _value: &str) -> Option<CaptureProvider> {
        panic!("request-ID validation must run before the backend")
    }

    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

#[test]
fn encoded_request_id_boundary_is_exact() {
    let accepted = "x".repeat(MCP_MAX_ENCODED_REQUEST_ID_BYTES - 2);
    let rejected = "x".repeat(MCP_MAX_ENCODED_REQUEST_ID_BYTES - 1);
    assert_eq!(
        encoded_json_string_bytes(&accepted),
        MCP_MAX_ENCODED_REQUEST_ID_BYTES
    );
    assert_eq!(
        serde_json::to_vec(&accepted).unwrap().len(),
        MCP_MAX_ENCODED_REQUEST_ID_BYTES
    );
    assert!(request_id_is_accepted(Some(&Value::String(
        accepted.clone()
    ))));
    assert!(!request_id_is_accepted(Some(&Value::String(
        rejected.clone()
    ))));

    let escaped = "\"\\\n\u{0001}雪";
    assert_eq!(
        encoded_json_string_bytes(escaped),
        serde_json::to_vec(escaped).unwrap().len()
    );
    assert!(request_id_is_accepted(Some(&json!(u64::MAX))));
    assert!(!request_id_is_accepted(Some(&Value::Null)));

    let mut initialized = true;
    let accepted_response = handle_protocol_message(
        json!({"jsonrpc": "2.0", "id": accepted, "method": "ping"}),
        RequestDescriptor::Ping,
        &mut initialized,
        McpServerIdentity {
            name: "ctx",
            version: "test",
        },
        &UnusedBackend,
        |_| panic!("request-ID validation must run before text rendering"),
    )
    .value
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&accepted_response["id"]).unwrap().len(),
        MCP_MAX_ENCODED_REQUEST_ID_BYTES
    );

    let rejected_response = handle_protocol_message(
        json!({"jsonrpc": "2.0", "id": rejected, "method": "ping"}),
        RequestDescriptor::Ping,
        &mut initialized,
        McpServerIdentity {
            name: "ctx",
            version: "test",
        },
        &UnusedBackend,
        |_| panic!("request-ID validation must run before text rendering"),
    )
    .value
    .unwrap();
    assert_eq!(rejected_response["id"], Value::Null);
    assert_eq!(rejected_response["error"]["code"], -32600);
}

#[test]
fn manifest_registers_native_blame_and_no_commercial_tools() {
    let definitions = tool_definitions(Vec::new());
    let names = definitions
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"blame"));
    assert!(!names.contains(&"pro_status"));
    assert!(!names.contains(&"referral"));
    let blame = definitions
        .iter()
        .find(|tool| tool["name"] == "blame")
        .unwrap();
    assert_eq!(blame["annotations"]["readOnlyHint"], true);
    assert_eq!(blame["inputSchema"]["properties"]["limit"]["maximum"], 8);
}

#[test]
fn search_root_and_group_arrays_are_typed_and_forwarded_across_backends() {
    for (backend, expected_backend) in [
        ("lexical", ToolSearchBackend::Lexical),
        ("semantic", ToolSearchBackend::Semantic),
        ("hybrid", ToolSearchBackend::Hybrid),
    ] {
        let request = search_request(
            &json!({
                "query": "fixture",
                "source_roots": ["personal", "archive"],
                "source_groups": ["work"],
                "backend": backend,
            }),
            &UnusedBackend,
        )
        .unwrap();
        assert_eq!(request.source_roots, ["personal", "archive"]);
        assert_eq!(request.source_groups, ["work"]);
        assert_eq!(request.backend, Some(expected_backend));
    }

    let error = search_request(
        &json!({"query": "fixture", "source_roots": ["personal", 7]}),
        &UnusedBackend,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("source_roots entries must be strings"));

    let definitions = tool_definitions(Vec::new());
    let search = definitions
        .iter()
        .find(|tool| tool["name"] == "search")
        .unwrap();
    assert_eq!(
        search["inputSchema"]["properties"]["source_roots"]["maxItems"],
        64
    );
    assert_eq!(
        search["inputSchema"]["properties"]["source_groups"]["items"]["maxLength"],
        64
    );
    for key in ["source_roots", "source_groups"] {
        assert_eq!(
            search["inputSchema"]["properties"][key]["items"]["pattern"],
            PROVIDER_ROOT_SELECTOR_PATTERN
        );
    }
}

#[test]
fn search_root_and_group_schema_matches_the_runtime_token_grammar() {
    for value in ["a", "A0_-", &"x".repeat(64)] {
        for key in ["source_roots", "source_groups"] {
            let mut arguments = json!({"query": "fixture"});
            arguments[key] = json!([value]);
            assert!(
                search_request(&arguments, &UnusedBackend).is_ok(),
                "{key} should accept {value:?}"
            );
        }
    }

    for value in ["", "bad.root", " spaced ", "café", &"x".repeat(65)] {
        for key in ["source_roots", "source_groups"] {
            let mut arguments = json!({"query": "fixture"});
            arguments[key] = json!([value]);
            let error = search_request(&arguments, &UnusedBackend).unwrap_err();
            let rendered = error.to_string();
            assert!(
                rendered.contains("ASCII letters, digits"),
                "{key} unexpectedly accepted {value:?}: {error}"
            );
            if !value.is_empty() {
                assert!(
                    !rendered.contains(value),
                    "{key} rejection leaked selector content: {rendered}"
                );
            }
        }
    }

    let too_many = vec!["root"; 65];
    for key in ["source_roots", "source_groups"] {
        let mut arguments = json!({"query": "fixture"});
        arguments[key] = json!(&too_many);
        let error = search_request(&arguments, &UnusedBackend).unwrap_err();
        assert!(error.to_string().contains("maximum of 64 entries"));
    }
}

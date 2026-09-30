//! Exercise the production newline-delimited stdio loop, including discovery,
//! parsing, backend dispatch and the final encoded JSON-RPC response.

use std::{io::Cursor, sync::Mutex};

use ctx_agent_integrations::{
    mcp::HistoryToolSurface,
    tool_backend::{
        ToolBackend, ToolExecutionError, ToolOperation, ToolOutcome, ToolTranscriptMode,
    },
};
use ctx_history_core::CaptureProvider;
use ctx_history_server::{Citation, CitationKind};
use serde_json::{json, Value};

use super::{
    render,
    tests::{fixture, remote_backend, search_hit, COLLECTION},
};

fn exchange(backend: &impl ToolBackend, messages: Vec<Value>) -> Vec<Value> {
    let mut input = vec![
        json!({
            "jsonrpc":"2.0", "id":"init", "method":"initialize",
            "params":{"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"synthetic-test", "version":"1"}}
        }),
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
    ];
    input.extend(messages);
    let mut bytes = Vec::new();
    for message in input {
        serde_json::to_writer(&mut bytes, &message).unwrap();
        bytes.push(b'\n');
    }
    let mut output = Vec::new();
    crate::mcp::serve_remote_stdio(&mut Cursor::new(bytes), &mut output, backend).unwrap();
    let encoded = String::from_utf8(output).unwrap();
    assert!(encoded.ends_with('\n'));
    let mut responses: Vec<Value> = encoded
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let initialized = responses.remove(0);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    let instructions = initialized["result"]["instructions"].as_str().unwrap();
    match backend.history_tool_surface() {
        HistoryToolSurface::Local => {
            assert!(instructions.starts_with("Local access to ctx search, cited Blame"));
            assert!(instructions.contains("read-only graph, and pure output tools"));
            assert!(!instructions.contains("shared ctx history collection"));
        }
        HistoryToolSurface::RemoteLog => {
            assert!(instructions.contains("selected shared ctx history collection"));
            assert!(instructions
                .contains("status, lexical search, show_event, and paginated show_session logs"));
            assert!(instructions.contains("bounded snippets"));
            assert!(instructions.contains("citations unchanged"));
            for unavailable in ["Local access", "Blame", "graph", "pure output"] {
                assert!(!instructions.contains(unavailable), "{instructions}");
            }
        }
    }
    assert!(instructions.contains("MCP hosts may log or forward it"));
    responses
}

fn call(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":name, "method":"tools/call", "params":{"name":name, "arguments":arguments}})
}

fn list() -> Value {
    json!({"jsonrpc":"2.0", "id":"list", "method":"tools/list"})
}

fn schema<'a>(response: &'a Value, name: &str) -> &'a Value {
    &response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap()["inputSchema"]
}

#[test]
fn remote_stdio_advertises_only_its_supported_tools_and_arguments() {
    // Use the actual adapter for discovery: no HTTP request is needed.
    let responses = exchange(&remote_backend(), vec![list()]);
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["status", "search", "show_session", "show_event"]
    );
    let session = schema(&responses[0], "show_session");
    assert_eq!(session["properties"]["mode"]["enum"], json!(["log"]));
    assert_eq!(session["properties"]["mode"]["default"], "log");
    assert_eq!(session["properties"]["limit"]["default"], 50);
    assert_eq!(session["properties"]["limit"]["maximum"], 100);
    assert_eq!(session["properties"]["cursor"]["maxLength"], 8192);
    assert_eq!(session["required"], json!(["ctx_session_id"]));
    let search = schema(&responses[0], "search");
    assert_eq!(search["required"], json!(["query"]));
    assert_eq!(search["properties"]["backend"]["enum"], json!(["lexical"]));
    assert_eq!(search["properties"]["limit"]["maximum"], 100);
    assert!(search["properties"].get("workspace").is_none());
    assert_eq!(
        schema(&responses[0], "show_event")["properties"]["window"]["maximum"],
        0
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
}

struct RemoteFixture {
    calls: Mutex<Vec<ToolOperation>>,
    hit: Value,
    event: Value,
    cursor: String,
}

impl ToolBackend for RemoteFixture {
    fn history_tool_surface(&self) -> HistoryToolSurface {
        HistoryToolSurface::RemoteLog
    }
    fn parse_provider(&self, _: &str) -> Option<CaptureProvider> {
        None
    }
    fn provider_names(&self) -> Vec<&'static str> {
        vec![]
    }
    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        let value = match &operation {
            ToolOperation::Search(_) => {
                json!({"results":[self.hit], "complete":true, "exhaustive":true})
            }
            ToolOperation::ShowEvent(_) => self.event.clone(),
            ToolOperation::ShowSession(request) if request.cursor.is_none() => {
                json!({"events":[self.event], "next_cursor":self.cursor})
            }
            ToolOperation::ShowSession(_) => json!({"events":[], "next_cursor":null}),
            _ => panic!("unexpected operation {operation:?}"),
        };
        self.calls.lock().unwrap().push(operation);
        let text = render::tool_text(&value).unwrap();
        let mut outcome = ToolOutcome::plain(value);
        outcome.text = Some(text);
        Ok(outcome)
    }
}

#[test]
fn stdio_search_citations_open_exact_evidence_and_page_with_remote_defaults() {
    let mut event = fixture();
    event.record.content.normalized_body = Some("exact\u{202e}evidence\nsecond line".into());
    event.record.content.structured_content =
        Some(json!({"exact":"structured\u{2066}evidence", "number":42}));
    let backend = RemoteFixture {
        calls: Mutex::new(vec![]),
        hit: serde_json::to_value(search_hit(&event, "exact\u{202e}evidence", true)).unwrap(),
        event: serde_json::to_value(&event).unwrap(),
        cursor: "x".repeat(5000),
    };
    let responses = exchange(&backend, vec![call("search", json!({"query":"evidence"}))]);
    let hit = &responses[0]["result"]["structuredContent"]["results"][0];
    assert_eq!(hit, &backend.hit);
    assert!(hit.get("record").is_none());
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("exact\u{202e}evidence"));
    assert!(text.contains("Snippet truncated"));
    assert!(text.contains(&event.citation));
    assert!(!text.contains("second line"));
    assert!(!text.contains("structured\u{2066}evidence"));
    let responses = exchange(
        &backend,
        vec![
            call("show_event", json!({"ctx_event_id":hit["citation"]})),
            call(
                "show_session",
                json!({"ctx_session_id":hit["session_citation"]}),
            ),
        ],
    );
    assert_eq!(responses[0]["result"]["structuredContent"], backend.event);
    assert!(responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("exact\u{202e}evidence\nsecond line"));
    assert_eq!(
        responses[1]["result"]["structuredContent"]["events"][0],
        backend.event
    );
    let cursor = &responses[1]["result"]["structuredContent"]["next_cursor"];
    assert_eq!(cursor.as_str(), Some(backend.cursor.as_str()));
    let responses = exchange(
        &backend,
        vec![call(
            "show_session",
            json!({
                "ctx_session_id":hit["session_citation"], "cursor":cursor, "limit":100, "mode":"log"
            }),
        )],
    );
    assert_eq!(
        responses[0]["result"]["structuredContent"],
        json!({"events":[], "next_cursor":null})
    );
    let calls = backend.calls.lock().unwrap();
    let ToolOperation::ShowEvent(opened) = &calls[1] else {
        panic!("show event")
    };
    assert_eq!(opened.selector, event.citation);
    let ToolOperation::ShowSession(first) = &calls[2] else {
        panic!("show session")
    };
    assert_eq!(first.selector, event.session_citation);
    assert_eq!(first.mode, ToolTranscriptMode::Log);
    assert_eq!(first.limit, 50);
    assert_eq!(first.cursor, None);
    let ToolOperation::ShowSession(next) = &calls[3] else {
        panic!("next page")
    };
    assert_eq!(next.selector, event.session_citation);
    assert_eq!(next.mode, ToolTranscriptMode::Log);
    assert_eq!(next.limit, 100);
    assert_eq!(next.cursor.as_deref(), Some(backend.cursor.as_str()));
}

#[test]
fn stdio_text_only_clients_receive_structured_only_evidence_and_policy() {
    let mut event = fixture();
    event.record.content.normalized_body = None;
    event.record.content.structured_content = Some(json!({"only":"retained\u{202e}structure"}));
    let backend = RemoteFixture {
        calls: Mutex::new(vec![]),
        hit: serde_json::to_value(search_hit(&event, "retained structure", false)).unwrap(),
        event: serde_json::to_value(&event).unwrap(),
        cursor: "page-two".into(),
    };
    let responses = exchange(
        &backend,
        vec![call("show_event", json!({"ctx_event_id":event.citation}))],
    );
    let result = &responses[0]["result"];
    assert_eq!(result["structuredContent"], backend.event);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Content policy: \"selected\""));
    assert!(text.contains("Structured content: {\"only\":\"retained\u{202e}structure\"}"));
    assert!(text.contains(&event.citation));
    assert!(text.contains(&event.session_citation));
}

#[test]
fn remote_stdio_rejects_unsupported_modes_filters_and_invalid_citations_before_transport() {
    let event = fixture();
    let mut other_collection = Citation::parse(&event.citation).unwrap();
    other_collection.collection = "00000000-0000-4000-8000-000000000002".into();
    let responses = exchange(
        &remote_backend(),
        vec![
            call("show_event", json!({"ctx_event_id":"deadbeef"})),
            call("show_event", json!({"ctx_event_id":event.session_citation})),
            call(
                "show_event",
                json!({"ctx_event_id":other_collection.encode().unwrap()}),
            ),
            call("show_session", json!({"ctx_session_id":event.citation})),
            call(
                "show_session",
                json!({"ctx_session_id":event.session_citation, "mode":"lite"}),
            ),
            call(
                "show_session",
                json!({"ctx_session_id":event.session_citation, "mode":"full"}),
            ),
            call(
                "show_session",
                json!({"ctx_session_id":event.session_citation, "limit":101}),
            ),
            call(
                "show_session",
                json!({"ctx_session_id":event.session_citation, "cursor":"x".repeat(8193)}),
            ),
            call(
                "show_event",
                json!({"ctx_event_id":event.citation, "window":1}),
            ),
            call(
                "search",
                json!({"query":"evidence", "workspace":"filtered-workspace"}),
            ),
            call("search", json!({"query":"evidence", "limit":101})),
            call("search", json!({"query":"evidence", "backend":"semantic"})),
        ],
    );
    for response in responses {
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["error_code"],
            "invalid_request"
        );
        assert!(!response.to_string().contains("synthetic-test-token"));
    }
    assert_eq!(
        Citation::parse(&event.citation).unwrap().kind,
        CitationKind::Event
    );
    assert_eq!(
        Citation::parse(&event.session_citation).unwrap().collection,
        COLLECTION
    );
    for response in exchange(
        &remote_backend(),
        ["sources", "query_events", "blame"]
            .into_iter()
            .map(|name| call(name, json!({})))
            .collect(),
    ) {
        assert_eq!(response["error"]["code"], -32602);
    }
}

#[derive(Default)]
struct LocalProbe(Mutex<Vec<ToolOperation>>);

// Deliberately inherits the optional capability's local default.
impl ToolBackend for LocalProbe {
    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        self.0.lock().unwrap().push(operation);
        Ok(ToolOutcome::plain(json!({"local":true})))
    }
    fn parse_provider(&self, _: &str) -> Option<CaptureProvider> {
        None
    }
    fn provider_names(&self) -> Vec<&'static str> {
        vec![]
    }
}

#[test]
fn stdio_local_schema_defaults_prefixes_modes_windows_and_filters_are_unchanged() {
    let backend = LocalProbe::default();
    let responses = exchange(
        &backend,
        vec![
            list(),
            call("show_session", json!({"ctx_session_id":"deadbeef"})),
            call(
                "show_session",
                json!({"ctx_session_id":COLLECTION, "mode":"full", "limit":4096, "cursor":"x".repeat(4096)}),
            ),
            call(
                "show_session",
                json!({"ctx_session_id":"deadbeef", "mode":"log"}),
            ),
            call(
                "show_event",
                json!({"ctx_event_id":"deadbeef", "before":1, "after":2, "window":3}),
            ),
            call(
                "search",
                json!({"query":"evidence", "workspace":"local-workspace", "limit":200}),
            ),
            call("sources", json!({})),
            call("show_event", json!({"ctx_event_id":fixture().citation})),
        ],
    );
    let session = schema(&responses[0], "show_session");
    assert_eq!(
        session["properties"]["mode"]["enum"],
        json!(["full", "lite", "log"])
    );
    assert_eq!(session["properties"]["mode"]["default"], "lite");
    assert_eq!(session["properties"]["limit"]["default"], 200);
    assert_eq!(session["properties"]["limit"]["maximum"], 4096);
    assert_eq!(
        schema(&responses[0], "show_event")["properties"]["window"]["maximum"],
        50
    );
    assert_eq!(
        schema(&responses[0], "search")["properties"]["limit"]["maximum"],
        200
    );
    for response in &responses[1..7] {
        assert_eq!(
            response["result"]["structuredContent"],
            json!({"local":true})
        );
    }
    assert_eq!(responses[7]["result"]["isError"], true);
    let calls = backend.0.lock().unwrap();
    assert_eq!(calls.len(), 6);
    let ToolOperation::ShowSession(default) = &calls[0] else {
        panic!("session")
    };
    assert_eq!(default.mode, ToolTranscriptMode::Lite);
    assert_eq!(default.limit, 200);
    assert_eq!(default.selector, "deadbeef");
    let ToolOperation::ShowSession(full) = &calls[1] else {
        panic!("session")
    };
    assert_eq!(full.mode, ToolTranscriptMode::Full);
    assert_eq!(full.limit, 4096);
    let ToolOperation::ShowSession(log) = &calls[2] else {
        panic!("session")
    };
    assert_eq!(log.mode, ToolTranscriptMode::Log);
    let ToolOperation::ShowEvent(event) = &calls[3] else {
        panic!("event")
    };
    assert_eq!((event.before, event.after, event.window), (1, 2, Some(3)));
    let ToolOperation::Search(search) = &calls[4] else {
        panic!("search")
    };
    assert_eq!(search.workspace.as_deref(), Some("local-workspace"));
    assert_eq!(search.limit, 200);
    assert_eq!(calls[5], ToolOperation::Sources);
}

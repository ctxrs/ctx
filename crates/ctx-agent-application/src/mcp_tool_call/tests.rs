use std::sync::Mutex;

use ctx_agent_integrations::tool_backend::{
    QueryEventFilters, QueryEventsRequest, ShowEventRequest, ShowSessionRequest, ToolBackendError,
    ToolEventContent, ToolOperation, ToolSearchBackend, ToolSearchContentScope, ToolSearchRequest,
    ToolSearchUsageFacts, ToolTranscriptMode,
};
use serde_json::json;

use super::*;

#[derive(Default)]
struct Ports {
    calls: Mutex<Vec<&'static str>>,
}

impl Ports {
    fn called(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
}

impl HistoryReadPort for Ports {
    fn status(&self) -> Result<serde_json::Value, ToolBackendError> {
        self.called("status");
        Ok(json!({"payload_type": "status"}))
    }

    fn show_session(
        &self,
        request: ShowSessionRequest,
    ) -> Result<HistoryReadOutcome, ToolBackendError> {
        self.called("show_session");
        Ok(HistoryReadOutcome {
            structured: json!({"selector": request.selector}),
            compact: json!({"compact": "session"}),
        })
    }

    fn show_event(
        &self,
        request: ShowEventRequest,
    ) -> Result<HistoryReadOutcome, ToolBackendError> {
        self.called("show_event");
        Ok(HistoryReadOutcome {
            structured: json!({"selector": request.selector}),
            compact: json!({"compact": "event"}),
        })
    }

    fn query_events(
        &self,
        _request: QueryEventsRequest,
    ) -> Result<serde_json::Value, ToolBackendError> {
        self.called("query_events");
        Ok(json!({"payload_type": "event_range_page"}))
    }
}

impl SearchReadinessPort for Ports {
    fn search_ready(
        &self,
        request: ToolSearchRequest,
    ) -> Result<SearchReadOutcome, ToolBackendError> {
        self.called("search");
        Ok(SearchReadOutcome {
            structured: json!({"query": request.query}),
            compact: json!({"compact": "search"}),
            usage: ToolSearchUsageFacts::complete(11, 29),
        })
    }
}

impl SourceCatalogPort for Ports {
    fn source_catalog(&self) -> Result<SourceCatalog, ToolBackendError> {
        self.called("sources");
        Ok(SourceCatalog {
            automatic_discovery: false,
            sources: vec![json!({"provider": "fixture"})],
            issues: Vec::new(),
            issues_truncated: false,
        })
    }
}

fn search_request() -> ToolSearchRequest {
    ToolSearchRequest {
        query: "workspace context".to_owned(),
        limit: 8,
        provider: None,
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        source_roots: Vec::new(),
        source_groups: Vec::new(),
        workspace: Some("/workspace".to_owned()),
        since: None,
        primary_only: false,
        content_scope: ToolSearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        events: false,
        include_current_session: false,
        backend: Some(ToolSearchBackend::Lexical),
        semantic_weight: 0.35,
    }
}

#[test]
fn coarse_ports_are_called_once_and_results_are_converted_without_round_trips() {
    let ports = Ports::default();
    let status = invoke_mcp_tool_call(ToolOperation::Status, &ports, &ports, &ports).unwrap();
    assert_eq!(status.structured["payload_type"], "status");

    let search = invoke_mcp_tool_call(
        ToolOperation::Search(search_request()),
        &ports,
        &ports,
        &ports,
    )
    .unwrap();
    assert_eq!(search.structured["query"], "workspace context");
    assert_eq!(search.compact.unwrap()["compact"], "search");
    assert_eq!(
        search.usage.search,
        Some(ToolSearchUsageFacts::complete(11, 29))
    );

    let sources = invoke_mcp_tool_call(ToolOperation::Sources, &ports, &ports, &ports).unwrap();
    assert_eq!(sources.structured["schema_version"], 1);
    assert_eq!(sources.structured["automatic_discovery"], false);
    assert_eq!(sources.structured["sources"][0]["provider"], "fixture");
    assert_eq!(sources.structured["read_only"], true);
    assert_eq!(
        *ports.calls.lock().unwrap(),
        ["status", "search", "sources"]
    );
}

#[test]
fn history_operations_preserve_cursor_requests_and_compact_pairing() {
    let ports = Ports::default();
    let session = invoke_mcp_tool_call(
        ToolOperation::ShowSession(ShowSessionRequest {
            selector: "ctx-session".to_owned(),
            mode: ToolTranscriptMode::Lite,
            limit: 12,
            cursor: Some("cursor".to_owned()),
            output_limit_bytes: 1024,
        }),
        &ports,
        &ports,
        &ports,
    )
    .unwrap();
    assert_eq!(session.structured["selector"], "ctx-session");
    assert_eq!(session.compact.unwrap()["compact"], "session");

    let event = invoke_mcp_tool_call(
        ToolOperation::ShowEvent(ShowEventRequest {
            selector: "ctx-event".to_owned(),
            before: 1,
            after: 2,
            window: Some(3),
            output_limit_bytes: 1024,
        }),
        &ports,
        &ports,
        &ports,
    )
    .unwrap();
    assert_eq!(event.structured["selector"], "ctx-event");

    invoke_mcp_tool_call(
        ToolOperation::QueryEvents(QueryEventsRequest {
            since: None,
            until: None,
            filters: QueryEventFilters::default(),
            cursor: Some("cursor".to_owned()),
            content: ToolEventContent::None,
            limit: 7,
            output_limit_bytes: 1024,
        }),
        &ports,
        &ports,
        &ports,
    )
    .unwrap();
    assert_eq!(
        *ports.calls.lock().unwrap(),
        ["show_session", "show_event", "query_events"]
    );
}

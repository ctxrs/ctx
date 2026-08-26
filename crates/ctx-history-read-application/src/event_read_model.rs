use ctx_history_core::CoreContentPolicyStatus;
use ctx_history_index_query::{
    CoreEventRangeDirection, CoreEventRangeDomain, CoreEventRangeScope, CoreEventRangeSelection,
    CoreEventRecord, VerifiedIndex,
};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::json::{event_copy_json, timestamp_json};

pub const EVENT_QUERY_SCHEMA_VERSION: u8 = 1;
pub const EVENT_QUERY_PAGE_ITEMS: usize = 100;
pub const EVENT_QUERY_PAGE_BYTES: usize = 1024 * 1024;
/// JSON string escaping can expand each admitted Core byte to six wire bytes.
/// Keep a fixed envelope allowance while retaining a deterministic upper bound.
pub const MAX_EVENT_QUERY_WIRE_RECORD_BYTES: usize =
    ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES * 6 + 1024 * 1024;

/// Stored Core is already JSON escaped. Reserving seven eighths of the MCP
/// envelope covers event projection, receipt fields, and JSON-RPC framing
/// before a record is materialized.
pub const fn mcp_event_query_core_record_bytes(response_cap: usize) -> usize {
    response_cap / 8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventContentProjection {
    Full,
    Text,
    None,
}

impl EventContentProjection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Text => "text",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventQueryWireRequest {
    pub domain: Value,
    pub filters: Value,
    pub direction: &'static str,
    pub content: EventContentProjection,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct EventQueryReceipt<'a> {
    pub schema_version: u8,
    pub generation_id: &'a str,
    pub domain: &'a Value,
    pub filters: &'a Value,
    pub direction: &'static str,
    pub content: &'static str,
    pub limit: usize,
    pub terminal: bool,
    pub truncated: bool,
    pub next_cursor: Option<&'a str>,
    pub freshness: EventQueryFreshness,
    pub frontier: EventQueryFrontier<'a>,
}

#[derive(Serialize)]
pub struct EventQueryFreshness {
    pub mode: &'static str,
    pub status: &'static str,
    pub source_count: usize,
    pub read_only: bool,
}

#[derive(Serialize)]
pub struct EventQueryFrontier<'a> {
    pub generation_id: &'a str,
    pub cursor: Option<&'a str>,
    pub status: &'static str,
    pub certified_sources: usize,
    pub sources_with_frontier: usize,
    pub certified_bytes: u64,
}

#[derive(Serialize)]
pub struct EventQueryPageUsage {
    pub items: usize,
    pub pages: usize,
    pub bytes: usize,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub oversized_singleton: bool,
}

#[derive(Serialize)]
pub struct EventQueryPageReadModel<'a> {
    #[serde(flatten)]
    pub receipt: EventQueryReceipt<'a>,
    pub payload_type: &'static str,
    pub events: &'a [Value],
    pub usage: EventQueryPageUsage,
}

#[derive(Serialize)]
pub struct EventQueryCompletionUsage {
    pub items: usize,
    pub pages: usize,
    pub bytes: usize,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub oversized_singleton_pages: usize,
}

#[derive(Serialize)]
pub struct EventQueryCompletionReadModel<'a> {
    #[serde(flatten)]
    pub receipt: EventQueryReceipt<'a>,
    pub record_type: &'static str,
    pub usage: EventQueryCompletionUsage,
}

impl EventQueryWireRequest {
    pub fn from_selection(
        selection: &CoreEventRangeSelection,
        content: EventContentProjection,
        limit: usize,
    ) -> Self {
        event_query_wire_request(selection, content, limit)
    }

    pub fn new(
        domain: Value,
        filters: Value,
        direction: CoreEventRangeDirection,
        content: EventContentProjection,
        limit: usize,
    ) -> Self {
        Self {
            domain,
            filters,
            direction: direction_name(direction),
            content,
            limit,
        }
    }

    pub fn page_items(&self) -> usize {
        self.limit.min(EVENT_QUERY_PAGE_ITEMS)
    }
}

pub fn event_query_wire_request(
    selection: &CoreEventRangeSelection,
    content: EventContentProjection,
    limit: usize,
) -> EventQueryWireRequest {
    let selected = selection.filters();
    let mut filters = Map::new();
    if !selected.providers.is_empty() {
        filters.insert("providers".to_owned(), json!(selected.providers));
    }
    let source_identity = selected.source_identity.map(|value| value.to_string());
    let session_id = selected.session_id.map(|value| value.to_string());
    let parent_session_id = selected.parent_session_id.map(|value| value.to_string());
    let root_session_id = selected.root_session_id.map(|value| value.to_string());
    for (key, value) in [
        ("source", source_identity.as_deref()),
        ("history_source", selected.history_source.as_deref()),
        ("provider_key", selected.provider_key.as_deref()),
        ("source_id", selected.source_id.as_deref()),
        ("source_format", selected.source_format.as_deref()),
        (
            "provider_session_id",
            selected.provider_session_id.as_deref(),
        ),
        ("session", session_id.as_deref()),
        ("parent_session", parent_session_id.as_deref()),
        ("root_session", root_session_id.as_deref()),
        ("branch", selected.branch.as_deref()),
        ("workspace", selected.workspace.as_deref()),
        ("event_type", selected.event_type.as_deref()),
        ("role", selected.role.as_deref()),
        ("file", selected.file.as_deref()),
    ] {
        if let Some(value) = value {
            filters.insert(key.to_owned(), json!(value));
        }
    }
    if selected.scope != CoreEventRangeScope::All {
        filters.insert(
            "scope".to_owned(),
            json!(match selected.scope {
                CoreEventRangeScope::All => "all",
                CoreEventRangeScope::Primary => "primary",
                CoreEventRangeScope::Subagent => "subagent",
            }),
        );
    }
    let domain = match selection.domain() {
        CoreEventRangeDomain::All => json!({ "kind": "all" }),
        CoreEventRangeDomain::Timestamped {
            since_unix_ms,
            until_unix_ms,
        } => json!({
            "kind": "range",
            "range": {
                "since": timestamp_json(Some(since_unix_ms)),
                "until": timestamp_json(Some(until_unix_ms)),
            },
        }),
    };
    EventQueryWireRequest {
        domain,
        filters: Value::Object(filters),
        direction: direction_name(selected.direction),
        content,
        limit,
    }
}

pub fn event_query_receipt<'a>(
    index: &VerifiedIndex,
    request: &'a EventQueryWireRequest,
    generation_id: &'a str,
    next_cursor: Option<&'a str>,
    terminal: bool,
    truncated: bool,
) -> EventQueryReceipt<'a> {
    let sources = &index.manifest().sources;
    let sources_with_frontier = sources
        .iter()
        .filter(|source| source.frontier().is_some())
        .count();
    let frontier_status = if sources_with_frontier == 0 {
        "unavailable"
    } else if sources_with_frontier == sources.len() {
        "available"
    } else {
        "partial"
    };
    EventQueryReceipt {
        schema_version: EVENT_QUERY_SCHEMA_VERSION,
        generation_id,
        domain: &request.domain,
        filters: &request.filters,
        direction: request.direction,
        content: request.content.as_str(),
        limit: request.limit,
        terminal,
        truncated,
        next_cursor,
        freshness: EventQueryFreshness {
            mode: "pinned",
            status: "not_checked",
            source_count: sources.len(),
            read_only: true,
        },
        frontier: EventQueryFrontier {
            generation_id,
            cursor: next_cursor,
            status: frontier_status,
            certified_sources: sources.len(),
            sources_with_frontier,
            certified_bytes: index.manifest().certified_source_bytes,
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub fn event_query_page_read_model<'a>(
    index: &VerifiedIndex,
    request: &'a EventQueryWireRequest,
    generation_id: &'a str,
    events: &'a [Value],
    next_cursor: Option<&'a str>,
    terminal: bool,
    truncated: bool,
    usage: EventQueryPageUsage,
) -> EventQueryPageReadModel<'a> {
    EventQueryPageReadModel {
        receipt: event_query_receipt(
            index,
            request,
            generation_id,
            next_cursor,
            terminal,
            truncated,
        ),
        payload_type: "event_range_page",
        events,
        usage,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn event_query_completion_read_model<'a>(
    index: &VerifiedIndex,
    request: &'a EventQueryWireRequest,
    generation_id: &'a str,
    next_cursor: Option<&'a str>,
    terminal: bool,
    truncated: bool,
    usage: EventQueryCompletionUsage,
) -> EventQueryCompletionReadModel<'a> {
    EventQueryCompletionReadModel {
        receipt: event_query_receipt(
            index,
            request,
            generation_id,
            next_cursor,
            terminal,
            truncated,
        ),
        record_type: "event_range_completion",
        usage,
    }
}

pub fn event_query_event_read_model(generation_id: &str, ordinal: usize, event: Value) -> Value {
    json!({
        "schema_version": EVENT_QUERY_SCHEMA_VERSION,
        "record_type": "event_range_event",
        "generation_id": generation_id,
        "ordinal": ordinal,
        "event": event,
    })
}

const fn direction_name(direction: CoreEventRangeDirection) -> &'static str {
    match direction {
        CoreEventRangeDirection::Ascending => "ascending",
        CoreEventRangeDirection::Descending => "descending",
    }
}

pub fn render_event_read_model(
    event: &CoreEventRecord,
    projection: EventContentProjection,
) -> serde_json::Result<Value> {
    let record = &event.core_record;
    let content = &record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    let text = (projection != EventContentProjection::None)
        .then_some(content.normalized_body.as_ref())
        .flatten();
    let structured_content = (projection == EventContentProjection::Full)
        .then_some(content.structured_content.as_ref())
        .flatten();
    let activity = (projection == EventContentProjection::Full)
        .then_some(content.activity.as_ref())
        .flatten();
    let (provider_key, source_id) = event
        .custom_source_identity()
        .map_or((None, None), |(provider_key, source_id)| {
            (Some(provider_key), Some(source_id))
        });
    let occurred_at = timestamp_json(event.occurred_at_unix_ms);
    let mut rendered = json!({
        "schema_version": EVENT_QUERY_SCHEMA_VERSION,
        "record_version": record.record_version,
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_source_id": event.source.identity().as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.map(|id| id.as_uuid()),
        "session_relationship": event.session_relationship,
        "event_copy": event_copy_json(event.event_copy.as_ref()),
        "occurred_at": occurred_at,
        "occurred_at_ms": event.occurred_at_unix_ms,
        "sequence": event.event_sequence,
        "provider": event.provider,
        "provider_key": provider_key,
        "source_id": source_id,
        "source_format": event.source_format,
        "source": event.source,
        "provider_session_id": event.provider_session_id,
        "native_event_id": event.native_event_id,
        "agent_scope": event.agent_scope,
        "event_type": event.event_type,
        "role": event.role,
        "parser_revision": record.parser_revision,
        "normalization_revision": record.normalization_revision,
        "text": text,
        "structured_content": structured_content,
        "content": {
            "complete": complete,
            "policy_revision": content.policy_revision,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
        "citations": [{
            "target_type": "event",
            "ctx_event_id": event.event_id.as_uuid(),
            "ctx_session_id": event.session_id.as_uuid(),
            "label": event.event_type,
            "time": occurred_at,
            "provider": event.provider,
            "session_id": event.provider_session_id,
            "event_seq": event.event_sequence,
        }],
        "content_projection": projection.as_str(),
    });
    if let Some(activity) = activity {
        rendered["activity"] = serde_json::to_value(activity)?;
    }
    Ok(rendered)
}

use std::path::Path;

use anyhow::Result;
use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRangeDirection, CoreEventRangeFilters, CoreEventRangeScope,
};
use serde_json::Value;
use uuid::Uuid;

use super::{invalid_tool_request, optional_string, optional_usize};
use crate::commands::list::events::{
    decode_cursor, event_range_page_value, mcp_event_query_core_record_bytes, selection,
    validated_limit, EventContentProjection, EventQueryWireRequest, DEFAULT_EVENT_QUERY_LIMIT,
};
use crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES;

pub(super) fn tool_query_events(arguments: &Value, data_root: &Path) -> Result<Value> {
    let providers = optional_strings(arguments, "providers")?;
    let source = optional_string(arguments, "source")?;
    let history_source = optional_string(arguments, "history_source")?;
    let provider_key = optional_string(arguments, "provider_key")?;
    let source_id = optional_string(arguments, "source_id")?;
    let source_format = optional_string(arguments, "source_format")?;
    let provider_session = optional_string(arguments, "provider_session")?;
    let session = optional_string(arguments, "session")?;
    let parent_session = optional_string(arguments, "parent_session")?;
    let root_session = optional_string(arguments, "root_session")?;
    let branch = optional_string(arguments, "branch")?;
    let workspace = optional_string(arguments, "workspace")?;
    let event_type = optional_string(arguments, "event_type")?;
    let role = optional_string(arguments, "role")?;
    let agent_type = optional_string(arguments, "agent_type")?;
    let scope = optional_scope(arguments)?;
    let file = optional_string(arguments, "file")?;
    let direction = optional_direction(arguments)?;
    let filters = CoreEventRangeFilters {
        providers,
        source_identity: parse_optional_uuid("source", source.as_deref())?,
        history_source,
        provider_key,
        source_id,
        source_format,
        provider_session_id: provider_session,
        session_id: parse_optional_uuid("session", session.as_deref())?,
        parent_session_id: parse_optional_uuid("parent_session", parent_session.as_deref())?,
        root_session_id: parse_optional_uuid("root_session", root_session.as_deref())?,
        branch,
        workspace,
        event_type,
        role,
        agent_type,
        scope,
        file,
        direction,
    };
    let since = optional_string(arguments, "since")?;
    let until = optional_string(arguments, "until")?;
    let selection = selection(since.as_deref(), until.as_deref(), filters)?;
    let cursor = optional_string(arguments, "cursor")?
        .as_deref()
        .map(decode_cursor)
        .transpose()?;
    let limit = optional_usize(arguments, "limit")?
        .map(usize_to_u64)
        .transpose()?
        .unwrap_or(DEFAULT_EVENT_QUERY_LIMIT);
    let limit = validated_limit(limit)?;
    let content = optional_content_projection(arguments)?;
    let request = EventQueryWireRequest::from_selection(&selection, content, limit);
    let record_bytes = mcp_event_query_core_record_bytes(MCP_PRESENTATION_MAX_OUTPUT_BYTES);
    let strict_budget =
        CoreEventPageBudget::new(record_bytes, record_bytes.min(MAX_CORE_CONTENT_BYTES));
    event_range_page_value(
        data_root,
        &selection,
        cursor.as_ref(),
        &request,
        Some(strict_budget),
    )
    .map_err(Into::into)
}

fn usize_to_u64(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid_tool_request("numeric argument is too large"))
}

fn optional_strings(arguments: &Value, key: &str) -> Result<Vec<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| invalid_tool_request(format!("{key} entries must be strings")))
            })
            .collect(),
        Some(_) => Err(invalid_tool_request(format!("{key} must be an array"))),
    }
}

fn parse_optional_uuid(key: &'static str, value: Option<&str>) -> Result<Option<Uuid>> {
    value
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| invalid_tool_request(format!("{key} must be a full UUID")))
        })
        .transpose()
}

fn optional_scope(arguments: &Value) -> Result<CoreEventRangeScope> {
    match optional_string(arguments, "scope")?.as_deref() {
        None | Some("all") => Ok(CoreEventRangeScope::All),
        Some("primary") => Ok(CoreEventRangeScope::Primary),
        Some("subagent") => Ok(CoreEventRangeScope::Subagent),
        Some(_) => Err(invalid_tool_request(
            "scope must be one of all, primary, subagent",
        )),
    }
}

fn optional_direction(arguments: &Value) -> Result<CoreEventRangeDirection> {
    match optional_string(arguments, "direction")?.as_deref() {
        None | Some("ascending") => Ok(CoreEventRangeDirection::Ascending),
        Some("descending") => Ok(CoreEventRangeDirection::Descending),
        Some(_) => Err(invalid_tool_request(
            "direction must be one of ascending, descending",
        )),
    }
}

fn optional_content_projection(arguments: &Value) -> Result<EventContentProjection> {
    match optional_string(arguments, "content")?.as_deref() {
        None | Some("full") => Ok(EventContentProjection::Full),
        Some("text") => Ok(EventContentProjection::Text),
        Some("none") => Ok(EventContentProjection::None),
        Some(_) => Err(invalid_tool_request(
            "content must be one of full, text, none",
        )),
    }
}

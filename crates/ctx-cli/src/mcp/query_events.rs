use std::path::Path;

use anyhow::Result;
use ctx_history_index::{CoreEventRangeDirection, CoreEventRangeFilters, CoreEventRangeScope};
use serde_json::Value;
use uuid::Uuid;

use super::{invalid_tool_request, optional_string, optional_usize};
use crate::commands::show::events::{
    decode_cursor, event_range_page_value, selection, validated_limits, wire_domain,
    EventContentProjection, EventQueryWireRequest, DEFAULT_EVENT_QUERY_BYTE_BUDGET,
    DEFAULT_EVENT_QUERY_LIMIT, DEFAULT_EVENT_QUERY_PAGE_ITEMS,
};

pub(super) fn tool_query_events(arguments: &Value, data_root: &Path) -> Result<Value> {
    let providers = optional_strings(arguments, "providers")?;
    let source = optional_string(arguments, "source")?;
    let history_source = optional_string(arguments, "history_source")?;
    let provider_key = optional_string(arguments, "provider_key")?;
    let source_id = optional_string(arguments, "source_id")?;
    let source_format = optional_string(arguments, "source_format")?;
    let provider_session = optional_string(arguments, "provider_session")?;
    let session = optional_string(arguments, "session")?;
    let parent_session = optional_alias_string(arguments, "parent_session", "parent")?;
    let root_session = optional_alias_string(arguments, "root_session", "root")?;
    let branch = optional_string(arguments, "branch")?;
    let workspace = optional_string(arguments, "workspace")?;
    let event_type = optional_string(arguments, "event_type")?;
    let role = optional_string(arguments, "role")?;
    let agent_type = optional_string(arguments, "agent_type")?;
    let scope = optional_scope(arguments)?;
    let file = optional_string(arguments, "file")?;
    let direction = optional_direction(arguments)?;
    let filters = CoreEventRangeFilters {
        providers: providers.clone(),
        source_identity: parse_optional_uuid("source", source.as_deref())?,
        history_source: history_source.clone(),
        provider_key: provider_key.clone(),
        source_id: source_id.clone(),
        source_format: source_format.clone(),
        provider_session_id: provider_session.clone(),
        session_id: parse_optional_uuid("session", session.as_deref())?,
        parent_session_id: parse_optional_uuid("parent_session", parent_session.as_deref())?,
        root_session_id: parse_optional_uuid("root_session", root_session.as_deref())?,
        branch: branch.clone(),
        workspace: workspace.clone(),
        event_type: event_type.clone(),
        role: role.clone(),
        agent_type: agent_type.clone(),
        scope,
        file: file.clone(),
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
    let max_items = optional_alias_usize(arguments, "max_items", "page_items")?
        .map(usize_to_u64)
        .transpose()?
        .unwrap_or(DEFAULT_EVENT_QUERY_PAGE_ITEMS);
    let max_bytes = optional_alias_usize(arguments, "max_bytes", "byte_budget")?
        .map(usize_to_u64)
        .transpose()?
        .unwrap_or(DEFAULT_EVENT_QUERY_BYTE_BUDGET);
    let limits = validated_limits(limit, max_items, max_bytes)?;
    let content = optional_content_projection(arguments)?;
    let wire_filters = compact_filters(
        &providers,
        [
            ("source", source.as_deref()),
            ("history_source", history_source.as_deref()),
            ("provider_key", provider_key.as_deref()),
            ("source_id", source_id.as_deref()),
            ("source_format", source_format.as_deref()),
            ("provider_session_id", provider_session.as_deref()),
            ("session", session.as_deref()),
            ("parent_session", parent_session.as_deref()),
            ("root_session", root_session.as_deref()),
            ("branch", branch.as_deref()),
            ("workspace", workspace.as_deref()),
            ("event_type", event_type.as_deref()),
            ("role", role.as_deref()),
            ("agent_type", agent_type.as_deref()),
            ("file", file.as_deref()),
        ],
        scope,
    );
    let request = EventQueryWireRequest::new(
        wire_domain(since.as_deref(), until.as_deref()),
        wire_filters,
        direction,
        content,
        limits,
    );
    event_range_page_value(data_root, &selection, cursor.as_ref(), &request).map_err(Into::into)
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

fn optional_alias_string(
    arguments: &Value,
    canonical: &str,
    alias: &str,
) -> Result<Option<String>> {
    let canonical_value = optional_string(arguments, canonical)?;
    let alias_value = optional_string(arguments, alias)?;
    match (canonical_value, alias_value) {
        (Some(_), Some(_)) => Err(invalid_tool_request(format!(
            "{canonical} and its alias {alias} cannot be supplied together"
        ))),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn optional_alias_usize(arguments: &Value, canonical: &str, alias: &str) -> Result<Option<usize>> {
    let canonical_value = optional_usize(arguments, canonical)?;
    let alias_value = optional_usize(arguments, alias)?;
    match (canonical_value, alias_value) {
        (Some(_), Some(_)) => Err(invalid_tool_request(format!(
            "{canonical} and its alias {alias} cannot be supplied together"
        ))),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn compact_filters<const N: usize>(
    providers: &[String],
    values: [(&str, Option<&str>); N],
    scope: CoreEventRangeScope,
) -> Value {
    let mut filters = serde_json::Map::new();
    if !providers.is_empty() {
        filters.insert("providers".to_owned(), serde_json::json!(providers));
    }
    for (key, value) in values {
        if let Some(value) = value {
            filters.insert(key.to_owned(), serde_json::json!(value));
        }
    }
    if scope != CoreEventRangeScope::All {
        filters.insert(
            "scope".to_owned(),
            serde_json::json!(match scope {
                CoreEventRangeScope::All => "all",
                CoreEventRangeScope::Primary => "primary",
                CoreEventRangeScope::Subagent => "subagent",
            }),
        );
    }
    Value::Object(filters)
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

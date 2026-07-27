use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchHit {
    pub event_id: Uuid,
    pub history_record_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub session_parent_session_id: Option<Uuid>,
    pub session_root_session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub seq: u64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    pub preview: String,
    pub score: f64,
    pub provider: Option<CaptureProvider>,
    pub session_external_session_id: Option<String>,
    pub history_source: Option<String>,
    pub history_source_plugin: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub agent_type: Option<AgentType>,
    pub session_is_primary: Option<bool>,
    pub cwd: Option<String>,
    pub raw_source_path: Option<String>,
    pub cursor: Option<String>,
    pub record_title: Option<String>,
    pub record_kind: Option<String>,
    pub record_workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEmbeddingDocument {
    pub event_id: Uuid,
    pub history_record_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub seq: u64,
    /// Activity time used to order semantic documents, including a paired assistant reply.
    pub occurred_at_ms: i64,
    /// User-anchor time used with `seq` to advance the Store pagination frontier.
    pub anchor_occurred_at_ms: i64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub rank_bucket: String,
    pub provider: Option<CaptureProvider>,
    pub source_format: Option<String>,
    pub agent_type: Option<AgentType>,
    pub session_is_primary: Option<bool>,
    pub cwd: Option<String>,
    pub raw_source_path: Option<String>,
    pub record_title: Option<String>,
    pub record_kind: Option<String>,
    pub record_workspace: Option<String>,
    pub text: String,
}

#[derive(Default)]
pub(super) struct EventSearchSourceIdentity {
    pub(super) history_source: Option<String>,
    pub(super) history_source_plugin: Option<String>,
    pub(super) provider_key: Option<String>,
    pub(super) source_id: Option<String>,
    pub(super) source_format: Option<String>,
}

pub(super) fn event_search_source_identity(
    source_metadata_json: Option<&str>,
) -> rusqlite::Result<EventSearchSourceIdentity> {
    let Some(source_metadata_json) = source_metadata_json else {
        return Ok(EventSearchSourceIdentity::default());
    };
    let metadata: serde_json::Value = serde_json::from_str(source_metadata_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let source_metadata = metadata
        .get("source_metadata")
        .and_then(serde_json::Value::as_object);
    let plugin = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_plugin"))
        .or_else(|| metadata.get("ctx_history_plugin"))
        .and_then(serde_json::Value::as_object);
    let custom = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_jsonl_v1"))
        .or_else(|| metadata.get("ctx_history_jsonl_v1"))
        .and_then(serde_json::Value::as_object);
    let plugin_name = plugin
        .and_then(|plugin| plugin.get("plugin_name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let plugin_source_id = plugin
        .and_then(|plugin| plugin.get("plugin_source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let history_source = plugin
        .and_then(|plugin| plugin.get("history_source"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            plugin_name
                .as_deref()
                .zip(plugin_source_id.as_deref())
                .map(|(plugin_name, source_id)| format!("{plugin_name}/{source_id}"))
        });
    let provider_key = custom
        .and_then(|custom| custom.get("provider_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_id = custom
        .and_then(|custom| custom.get("source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_format = custom
        .and_then(|custom| custom.get("source_format"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            source_metadata
                .and_then(|metadata| metadata.get("source_format"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            metadata
                .get("source_format")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned);
    Ok(EventSearchSourceIdentity {
        history_source,
        history_source_plugin: plugin_name,
        provider_key,
        source_id,
        source_format,
    })
}

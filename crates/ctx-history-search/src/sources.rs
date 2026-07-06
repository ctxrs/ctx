use crate::*;

pub(crate) fn source_id_matches_history_source_filter(
    source_id: Option<Uuid>,
    context: &RecordContext,
    filters: &SearchFilters,
) -> bool {
    if !has_history_source_filter(filters) {
        return true;
    }
    source_id
        .and_then(|id| context.sources.get(&id))
        .is_some_and(|source| source_matches_history_source_filter(source, filters))
}

pub(crate) fn associated_session(
    session_id: Option<Uuid>,
    source_id: Option<Uuid>,
    context: &RecordContext,
) -> Option<&Session> {
    session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
        .or_else(|| source_id.and_then(|id| associated_session_for_source(id, context)))
}

pub(crate) fn associated_session_for_source(
    source_id: Uuid,
    context: &RecordContext,
) -> Option<&Session> {
    context
        .sessions
        .iter()
        .find(|session| session.capture_source_id == Some(source_id))
        .or_else(|| {
            let source = context.sources.get(&source_id)?;
            context.sessions.iter().find(|session| {
                session.provider == source.descriptor.provider
                    && session.external_session_id == source.descriptor.external_session_id
            })
        })
}

pub(crate) fn source_for_id(
    source_id: Option<Uuid>,
    context: &RecordContext,
) -> Option<&ctx_history_core::CaptureSource> {
    source_id.and_then(|id| context.sources.get(&id))
}

pub(crate) fn source_cursor(source: &ctx_history_core::CaptureSource) -> Option<String> {
    source
        .sync
        .metadata
        .get("cursor")
        .and_then(|cursor| cursor.get("after"))
        .and_then(|after| after.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SourceHistoryIdentity {
    pub(crate) history_source: Option<String>,
    pub(crate) history_source_plugin: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
}

pub(crate) fn source_history_identity(
    source: &ctx_history_core::CaptureSource,
) -> SourceHistoryIdentity {
    let metadata = &source.sync.metadata;
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
    SourceHistoryIdentity {
        history_source,
        history_source_plugin: plugin_name,
        provider_key,
        source_id,
        source_format,
    }
}

pub(crate) fn has_history_source_filter(filters: &SearchFilters) -> bool {
    filters
        .history_source
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || filters
            .provider_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters
            .source_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters
            .source_format
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

pub(crate) fn source_matches_history_source_filter(
    source: &ctx_history_core::CaptureSource,
    filters: &SearchFilters,
) -> bool {
    let identity = source_history_identity(source);
    source_identity_matches_history_source_filter(&identity, filters)
}

pub(crate) fn hit_matches_history_source_filter(
    hit: &HitMetadata,
    filters: &SearchFilters,
) -> bool {
    if !has_history_source_filter(filters) {
        return true;
    }
    source_identity_matches_history_source_filter(
        &SourceHistoryIdentity {
            history_source: hit.history_source.clone(),
            history_source_plugin: hit.history_source_plugin.clone(),
            provider_key: hit.provider_key.clone(),
            source_id: hit.source_id.clone(),
            source_format: hit.source_format.clone(),
        },
        filters,
    )
}

pub(crate) fn source_identity_matches_history_source_filter(
    identity: &SourceHistoryIdentity,
    filters: &SearchFilters,
) -> bool {
    if let Some(selector) = filters
        .history_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let plugin_match = identity.history_source.as_deref() == Some(selector);
        let provider_source_match = identity
            .provider_key
            .as_deref()
            .zip(identity.source_id.as_deref())
            .is_some_and(|(provider_key, source_id)| {
                selector == format!("{provider_key}/{source_id}")
            });
        if !plugin_match && !provider_source_match {
            return false;
        }
    }
    if let Some(provider_key) = filters
        .provider_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if identity.provider_key.as_deref() != Some(provider_key) {
            return false;
        }
    }
    if let Some(source_id) = filters
        .source_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if identity.source_id.as_deref() != Some(source_id) {
            return false;
        }
    }
    if let Some(source_format) = filters
        .source_format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if identity.source_format.as_deref() != Some(source_format) {
            return false;
        }
    }
    true
}

pub(crate) fn event_cursor(event: &Event) -> Option<String> {
    event
        .payload
        .get("cursor")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            event
                .sync
                .metadata
                .get("cursor")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
}

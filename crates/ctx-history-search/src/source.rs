use std::path::Path;

use chrono::Utc;
use ctx_history_core::{
    Artifact, ContextCitation, ContextCitationType, Event, FileTouched, Run, Session,
};
use uuid::Uuid;

use crate::filters::{hit_matches_history_source_filter, session_matches_agent_scope};
use crate::model::{HitMetadata, RecordContext};
use crate::query::SearchFilters;
use crate::snippets::joined;

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
                session.capture_source_id.is_none()
                    && session.provider == source.descriptor.provider
                    && session.external_session_id == source.descriptor.external_session_id
            })
        })
}

pub(crate) fn record_context_display_hit(
    context: &RecordContext,
    filters: &SearchFilters,
    time: chrono::DateTime<Utc>,
) -> HitMetadata {
    context
        .sessions
        .iter()
        .find(|session| {
            session_matches_agent_scope(session, filters)
                && filters
                    .provider
                    .map_or(true, |provider| session.provider == provider)
                && filters.session.map_or(true, |id| session.id == id)
                && hit_matches_history_source_filter(&session_hit(session, context), filters)
        })
        .or_else(|| {
            context
                .sessions
                .iter()
                .find(|session| session_matches_agent_scope(session, filters))
        })
        .map(|session| session_hit(session, context))
        .unwrap_or_else(|| empty_hit(time))
}

pub(crate) fn file_touched_search_text(file: &FileTouched) -> String {
    let path = file.path.as_str();
    let old_path = file.old_path.as_deref().unwrap_or_default();
    joined([
        path,
        old_path,
        file.change_kind
            .map(|kind| kind.as_str())
            .unwrap_or_default(),
    ])
}

pub(crate) fn citation(
    citation_type: ContextCitationType,
    id: Uuid,
    label: &str,
    time: chrono::DateTime<Utc>,
) -> ContextCitation {
    ContextCitation {
        citation_type,
        id,
        label: label.to_owned(),
        time,
        provider: None,
        session_id: None,
        event_seq: None,
        raw_source_path: None,
        raw_source_exists: None,
        cursor: None,
    }
}

pub(crate) fn empty_hit(time: chrono::DateTime<Utc>) -> HitMetadata {
    HitMetadata {
        time,
        provider: None,
        provider_session_id: None,
        history_source: None,
        history_source_plugin: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        session_id: None,
        parent_session_id: None,
        root_session_id: None,
        event_id: None,
        event_seq: None,
        cwd: None,
        raw_source_path: None,
        raw_source_exists: None,
        cursor: None,
    }
}

pub(crate) fn session_hit(session: &Session, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(session.capture_source_id, session.started_at, context);
    hit.provider = Some(session.provider);
    hit.provider_session_id = session.external_session_id.clone();
    hit.session_id = Some(session.id);
    hit.parent_session_id = session.parent_session_id;
    hit.root_session_id = session.root_session_id;
    if hit.cwd.is_none() {
        hit.cwd = source_for_id(session.capture_source_id, context)
            .and_then(|source| source.descriptor.cwd.clone());
    }
    hit
}

pub(crate) fn run_hit(run: &Run, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(run.source_id, run.started_at, context);
    hit.session_id = run.session_id;
    if let Some(session) = run
        .session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
    {
        if hit.provider.is_none() {
            hit.provider = Some(session.provider);
        }
        if hit.provider_session_id.is_none() {
            hit.provider_session_id = session.external_session_id.clone();
        }
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    if hit.cwd.is_none() {
        hit.cwd = run.cwd.clone();
    }
    hit
}

pub(crate) fn event_hit(event: &Event, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(event.capture_source_id, event.occurred_at, context);
    hit.session_id = event.session_id;
    hit.event_id = Some(event.id);
    hit.event_seq = Some(event.seq);
    hit.cursor = event_cursor(event).or(hit.cursor);
    if hit.provider.is_none() {
        if let Some(session) = event
            .session_id
            .and_then(|id| context.sessions.iter().find(|session| session.id == id))
        {
            hit.provider = Some(session.provider);
            if hit.provider_session_id.is_none() {
                hit.provider_session_id = session.external_session_id.clone();
            }
            hit.parent_session_id = session.parent_session_id;
            hit.root_session_id = session.root_session_id;
        }
    }
    hit
}

pub(crate) fn artifact_hit(artifact: &Artifact, context: &RecordContext) -> HitMetadata {
    source_hit(artifact.source_id, artifact.timestamps.updated_at, context)
}

pub(crate) fn file_hit(file: &FileTouched, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(file.source_id, file.timestamps.updated_at, context);
    hit.event_id = file.event_id;
    hit.session_id = file.event_id.and_then(|id| {
        context
            .events
            .iter()
            .find(|event| event.id == id)
            .and_then(|event| event.session_id)
    });
    if let Some(session) = hit
        .session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
    {
        hit.provider = Some(session.provider);
        hit.provider_session_id = session.external_session_id.clone();
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    hit
}

pub(crate) fn source_hit(
    source_id: Option<Uuid>,
    time: chrono::DateTime<Utc>,
    context: &RecordContext,
) -> HitMetadata {
    let Some(source) = source_for_id(source_id, context) else {
        return empty_hit(time);
    };
    let raw_source_path = source.descriptor.raw_source_path.clone();
    let identity = source_history_identity(source);
    let mut hit = HitMetadata {
        time,
        provider: Some(source.descriptor.provider),
        provider_session_id: source.descriptor.external_session_id.clone(),
        history_source: identity.history_source,
        history_source_plugin: identity.history_source_plugin,
        provider_key: identity.provider_key,
        source_id: identity.source_id,
        source_format: identity.source_format,
        session_id: None,
        parent_session_id: None,
        root_session_id: None,
        event_id: None,
        event_seq: None,
        cwd: source.descriptor.cwd.clone(),
        raw_source_exists: raw_source_path
            .as_deref()
            .map(|path| Path::new(path).exists()),
        raw_source_path,
        cursor: source_cursor(source),
    };
    if let Some(session) = associated_session_for_source(source.id, context) {
        hit.provider = Some(session.provider);
        hit.provider_session_id = session.external_session_id.clone();
        hit.session_id = Some(session.id);
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    hit
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

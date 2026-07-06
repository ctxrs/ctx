use ctx_history_core::{HistoryRecord, Session};
use ctx_history_store::{EventSearchHit, FileTouchScope, Store};
use uuid::Uuid;

use crate::model::{HitMetadata, RecordContext};
use crate::query::{ProviderSessionFilter, Result, SearchFilters};
use crate::source::{associated_session, source_history_identity, SourceHistoryIdentity};

pub(crate) fn event_hit_matches_filters(
    hit: &EventSearchHit,
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> bool {
    if let Some(session_id) = filters.session {
        if hit.session_id != Some(session_id) {
            return false;
        }
    }
    if event_hit_matches_excluded_provider_session(hit, filters) {
        return false;
    }
    if let Some(provider) = filters.provider {
        if hit.provider != Some(provider) {
            return false;
        }
    }
    if let Some(since) = filters.since {
        if hit.occurred_at < since {
            return false;
        }
    }
    if !event_hit_matches_agent_scope(hit, filters) {
        return false;
    }
    if let Some(event_type) = filters.event_type {
        if hit.event_type != event_type {
            return false;
        }
    }
    if let Some(repo) = filters
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let repo = repo.to_lowercase();
        let matches_repo = [
            hit.cwd.as_deref(),
            hit.raw_source_path.as_deref(),
            hit.record_workspace.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&repo));
        if !matches_repo {
            return false;
        }
    }
    if let Some(scope) = file_scope {
        if !file_scope_matches_hit(scope, hit) {
            return false;
        }
    }
    true
}

pub(crate) fn event_hit_matches_excluded_provider_session(
    hit: &EventSearchHit,
    filters: &SearchFilters,
) -> bool {
    filters
        .exclude_provider_session
        .as_ref()
        .is_some_and(|excluded| {
            (hit.provider == Some(excluded.provider)
                && hit.session_external_session_id.as_deref()
                    == Some(excluded.provider_session_id.as_str()))
                || excluded_session_tree_matches(
                    excluded,
                    hit.session_id,
                    hit.session_parent_session_id,
                    hit.session_root_session_id,
                )
        })
}

pub(crate) fn hit_matches_excluded_provider_session(
    hit: &HitMetadata,
    filters: &SearchFilters,
) -> bool {
    filters
        .exclude_provider_session
        .as_ref()
        .is_some_and(|excluded| {
            (hit.provider == Some(excluded.provider)
                && hit.provider_session_id.as_deref()
                    == Some(excluded.provider_session_id.as_str()))
                || excluded_session_tree_matches(
                    excluded,
                    hit.session_id,
                    hit.parent_session_id,
                    hit.root_session_id,
                )
        })
}

pub(crate) fn context_has_excluded_provider_session(
    context: &RecordContext,
    filters: &SearchFilters,
) -> bool {
    filters
        .exclude_provider_session
        .as_ref()
        .is_some_and(|excluded| {
            context.sessions.iter().any(|session| {
                (session.provider == excluded.provider
                    && session.external_session_id.as_deref()
                        == Some(excluded.provider_session_id.as_str()))
                    || excluded_session_tree_matches(
                        excluded,
                        Some(session.id),
                        session.parent_session_id,
                        session.root_session_id,
                    )
            })
        })
}

pub(crate) fn excluded_session_tree_matches(
    excluded: &ProviderSessionFilter,
    session_id: Option<Uuid>,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
) -> bool {
    excluded.session_id.is_some_and(|excluded_session_id| {
        session_id == Some(excluded_session_id)
            || parent_session_id == Some(excluded_session_id)
            || root_session_id == Some(excluded_session_id)
    })
}

pub(crate) fn file_filter_scope(
    store: &Store,
    filters: &SearchFilters,
) -> Result<Option<FileTouchScope>> {
    let Some(file) = filters
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(store.file_touch_scope(file)?))
}

pub(crate) fn file_scope_matches_hit(scope: &FileTouchScope, hit: &EventSearchHit) -> bool {
    scope.event_ids.contains(&hit.event_id)
        || hit
            .run_id
            .is_some_and(|run_id| scope.run_ids.contains(&run_id))
        || hit
            .session_id
            .is_some_and(|session_id| scope.session_ids.contains(&session_id))
        || hit
            .history_record_id
            .is_some_and(|record_id| scope.history_record_ids.contains(&record_id))
}

pub(crate) fn is_agent_history_bookkeeping_record(record: &HistoryRecord) -> bool {
    record.kind == "agent_history"
        || record.tags.iter().any(|tag| tag == "agent-history")
        || record
            .body
            .trim_start()
            .starts_with("Indexed local agent history from ")
        || record
            .body
            .trim_start()
            .starts_with("Indexed custom agent history from ")
}

pub(crate) fn session_matches_agent_scope(session: &Session, filters: &SearchFilters) -> bool {
    if filters.session == Some(session.id) {
        return true;
    }
    if filters.include_subagents && !filters.primary_only {
        return true;
    }
    session_is_primary(session)
        || (!filters.primary_only
            && session.agent_type == ctx_history_core::AgentType::Unknown
            && session.parent_session_id.is_none())
}

pub(crate) fn session_is_primary(session: &Session) -> bool {
    session.is_primary || session.agent_type == ctx_history_core::AgentType::Primary
}

pub(crate) fn event_hit_matches_agent_scope(hit: &EventSearchHit, filters: &SearchFilters) -> bool {
    if filters.session.is_some() && filters.session == hit.session_id {
        return true;
    }
    if filters.include_subagents && !filters.primary_only {
        return true;
    }
    if hit.session_is_primary == Some(true)
        || hit.agent_type == Some(ctx_history_core::AgentType::Primary)
    {
        return true;
    }
    if filters.primary_only {
        return false;
    }
    hit.session_is_primary.is_none() && hit.agent_type.is_none()
}

pub(crate) fn record_text_matches_agent_scope(
    context: &RecordContext,
    filters: &SearchFilters,
) -> bool {
    if has_history_source_filter(filters) {
        return false;
    }
    context
        .sessions
        .iter()
        .all(|session| session_matches_agent_scope(session, filters))
}

pub(crate) fn item_matches_agent_scope(
    session_id: Option<Uuid>,
    source_id: Option<Uuid>,
    context: &RecordContext,
    filters: &SearchFilters,
) -> bool {
    let item_source_id = source_id.or_else(|| {
        session_id
            .and_then(|id| context.sessions.iter().find(|session| session.id == id))
            .and_then(|session| session.capture_source_id)
    });
    if !source_id_matches_history_source_filter(item_source_id, context, filters) {
        return false;
    }
    associated_session(session_id, source_id, context)
        .map(|session| session_matches_agent_scope(session, filters))
        .unwrap_or(true)
}

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

pub(crate) fn has_filters(filters: &SearchFilters) -> bool {
    filters.session.is_some()
        || filters.provider.is_some()
        || filters
            .repo
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.since.is_some()
        || filters.primary_only
        || !filters.include_subagents
        || filters.event_type.is_some()
        || filters
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.exclude_provider_session.is_some()
        || has_history_source_filter(filters)
}

pub(crate) fn record_matches_filters(
    record: &HistoryRecord,
    context: &RecordContext,
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> bool {
    if let Some(session_id) = filters.session {
        if !context
            .sessions
            .iter()
            .any(|session| session.id == session_id)
            && !context
                .events
                .iter()
                .any(|event| event.session_id == Some(session_id))
            && !context
                .runs
                .iter()
                .any(|run| run.session_id == Some(session_id))
        {
            return false;
        }
    }

    if let Some(excluded) = &filters.exclude_provider_session {
        let matched_sessions = context
            .sessions
            .iter()
            .filter(|session| {
                (session.provider == excluded.provider
                    && session.external_session_id.as_deref()
                        == Some(excluded.provider_session_id.as_str()))
                    || excluded_session_tree_matches(
                        excluded,
                        Some(session.id),
                        session.parent_session_id,
                        session.root_session_id,
                    )
            })
            .count();
        if matched_sessions > 0 && matched_sessions == context.sessions.len() {
            return false;
        }
    }

    if let Some(provider) = filters.provider {
        let session_match = context
            .sessions
            .iter()
            .any(|session| session.provider == provider);
        let source_match = context
            .sources
            .values()
            .any(|source| source.descriptor.provider == provider);
        if !session_match && !source_match {
            return false;
        }
    }

    if has_history_source_filter(filters)
        && !context
            .sources
            .values()
            .any(|source| source_matches_history_source_filter(source, filters))
    {
        return false;
    }

    if let Some(since) = filters.since {
        let has_recent_event = context
            .events
            .iter()
            .any(|event| event.occurred_at >= since);
        let has_recent_session = context.sessions.iter().any(|session| {
            session.started_at >= since || session.ended_at.is_some_and(|ended| ended >= since)
        });
        if record.updated_at < since && !has_recent_event && !has_recent_session {
            return false;
        }
    }

    if (filters.primary_only || !filters.include_subagents)
        && !context.sessions.is_empty()
        && !context
            .sessions
            .iter()
            .any(|session| session_matches_agent_scope(session, filters))
    {
        return false;
    }

    if let Some(event_type) = filters.event_type {
        if !context
            .events
            .iter()
            .any(|event| event.event_type == event_type)
        {
            return false;
        }
    }

    if let Some(repo) = filters
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let repo = repo.to_lowercase();
        let matches_record = record
            .workspace
            .as_deref()
            .is_some_and(|workspace| workspace.to_lowercase().contains(&repo));
        let matches_session = context.sessions.iter().any(|session| {
            session
                .sync
                .metadata
                .get("metadata")
                .and_then(|value| value.as_object())
                .is_some_and(|metadata| {
                    metadata
                        .values()
                        .any(|value| value.to_string().to_lowercase().contains(&repo))
                })
        });
        let matches_source = context.sources.values().any(|source| {
            source
                .descriptor
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.to_lowercase().contains(&repo))
        });
        if !matches_record && !matches_session && !matches_source {
            return false;
        }
    }

    if let Some(file) = filters
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(scope) = file_scope {
            if !record_context_matches_file_scope(scope, record, context) {
                return false;
            }
        } else if !context.files_touched.iter().any(|touched| {
            touched.path == file
                || touched.path.ends_with(file)
                || touched.old_path.as_deref() == Some(file)
        }) {
            return false;
        }
    }

    true
}

pub(crate) fn record_context_matches_file_scope(
    scope: &FileTouchScope,
    record: &HistoryRecord,
    context: &RecordContext,
) -> bool {
    scope.history_record_ids.contains(&record.id)
        || context.sessions.iter().any(|session| {
            scope.session_ids.contains(&session.id)
                || session
                    .capture_source_id
                    .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
        || context.runs.iter().any(|run| {
            scope.run_ids.contains(&run.id)
                || run
                    .session_id
                    .is_some_and(|session_id| scope.session_ids.contains(&session_id))
                || run
                    .source_id
                    .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
        || context.events.iter().any(|event| {
            scope.event_ids.contains(&event.id)
                || event
                    .session_id
                    .is_some_and(|session_id| scope.session_ids.contains(&session_id))
                || event
                    .run_id
                    .is_some_and(|run_id| scope.run_ids.contains(&run_id))
                || event
                    .capture_source_id
                    .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
        || context.files_touched.iter().any(|file| {
            file.source_id
                .is_some_and(|source_id| scope.source_ids.contains(&source_id))
        })
}

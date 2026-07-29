use ctx_history_index::{AgentScope, EventRecord, EventSearchFilters};

pub(super) fn source_event_matches_filters(
    event: &EventRecord,
    filters: &EventSearchFilters,
) -> bool {
    if !filters.matches_source_identity(event) {
        return false;
    }
    if filters
        .session_id
        .is_some_and(|id| event.session_id.as_uuid() != id)
        || filters
            .parent_session_id
            .is_some_and(|id| event.parent_session_id.map(|value| value.as_uuid()) != Some(id))
        || filters
            .root_session_id
            .is_some_and(|id| event.root_session_id.as_uuid() != id)
        || filters
            .provider
            .as_deref()
            .is_some_and(|value| event.provider != value)
        || filters
            .source_format
            .as_deref()
            .is_some_and(|value| event.source_format != value)
        || filters
            .provider_session_id
            .as_deref()
            .is_some_and(|value| event.provider_session_id.as_deref() != Some(value))
        || filters
            .branch
            .as_deref()
            .is_some_and(|value| event.branch.as_deref() != Some(value))
        || filters
            .event_type
            .as_deref()
            .is_some_and(|value| event.event_type != value)
        || filters
            .role
            .as_deref()
            .is_some_and(|value| event.role.as_deref() != Some(value))
        || filters
            .agent_type
            .as_deref()
            .is_some_and(|value| event.agent_type != value)
        || filters
            .since_unix_ms
            .is_some_and(|since| event.occurred_at_unix_ms.is_none_or(|value| value < since))
    {
        return false;
    }
    if filters.agent_scope == AgentScope::Primary
        && filters.session_id.is_none()
        && !event.is_primary
        && event.agent_type != "primary"
    {
        return false;
    }
    if filters.workspace.as_deref().is_some_and(|needle| {
        !event
            .workspace
            .as_deref()
            .is_some_and(|value| metadata_contains(value, needle))
    }) {
        return false;
    }
    if filters.file.as_deref().is_some_and(|needle| {
        !event
            .touched_files
            .iter()
            .any(|value| metadata_contains(value, needle))
    }) {
        return false;
    }
    !filters
        .exclude_session_tree
        .as_ref()
        .is_some_and(|excluded| {
            let provider_thread = event.provider == excluded.provider
                && event.provider_session_id.as_deref()
                    == Some(excluded.provider_session_id.as_str());
            provider_thread
                || excluded.session_id.is_some_and(|session_id| {
                    event.session_id.as_uuid() == session_id
                        || event.parent_session_id.map(|id| id.as_uuid()) == Some(session_id)
                        || event.root_session_id.as_uuid() == session_id
                })
        })
}

fn metadata_contains(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.trim().to_lowercase())
}

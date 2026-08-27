use super::{
    content_scope_event_types, source_token, AsciiFoldSubstring, CompiledSearchFilter,
    CoreAgentScope, CoreEventRecord, LiteralFactKind, Result, SearchAgentScope, StableEntityId,
};

impl CompiledSearchFilter {
    /// Reapplies the canonical logical filter to one final decoded winner.
    /// This is deliberately the same authority used to derive lexical and
    /// semantic backend adapters, so hydration cannot silently widen a query.
    pub fn matches_core(&self, record: &CoreEventRecord) -> Result<bool> {
        let filters = self.filters();
        let event = &record.event;
        let core = &record.core_record;

        if core.content.discovery_exclusion.is_some()
            || filters.allowed_source_keys.as_ref().is_some_and(|allowed| {
                !allowed
                    .iter()
                    .any(|candidate| candidate == &source_token(&event.source))
            })
            || filters
                .session_id
                .is_some_and(|expected| event.session_id.as_uuid() != expected)
            || filters
                .excluded_session_ids
                .iter()
                .chain(
                    filters
                        .exclude_session_tree
                        .iter()
                        .flat_map(|tree| tree.session_ids.iter()),
                )
                .any(|excluded| event.session_id.as_uuid() == *excluded)
            || filters.parent_session_id.is_some_and(|expected| {
                event.parent_session_id.map(StableEntityId::as_uuid) != Some(expected)
            })
            || filters.root_session_id.is_some_and(|expected| {
                event.root_session_id.map(StableEntityId::as_uuid) != Some(expected)
            })
            || filters
                .provider
                .as_deref()
                .is_some_and(|expected| event.provider != expected.trim())
            || filters
                .source_format
                .as_deref()
                .is_some_and(|expected| event.source_format != expected.trim())
            || filters
                .provider_session_id
                .as_deref()
                .is_some_and(|expected| {
                    event.provider_session_id.as_deref() != Some(expected.trim())
                })
            || filters
                .event_type
                .as_deref()
                .is_some_and(|expected| event.event_type != expected.trim())
            || content_scope_event_types(filters.content_scope)
                .is_some_and(|eligible| !eligible.contains(&event.event_type.as_str()))
            || filters
                .role
                .as_deref()
                .is_some_and(|expected| event.role.as_deref() != Some(expected.trim()))
            || filters.since_unix_ms.is_some_and(|since| {
                event
                    .occurred_at_unix_ms
                    .is_none_or(|occurred_at| occurred_at < since)
            })
            || match filters.agent_scope {
                SearchAgentScope::All => false,
                SearchAgentScope::Primary => event.agent_scope != Some(CoreAgentScope::Primary),
                SearchAgentScope::Subagent => event.agent_scope != Some(CoreAgentScope::Subagent),
            }
            || !self.matches_source_identity(event)
        {
            return Ok(false);
        }

        let facts = core
            .content
            .activity
            .iter()
            .flat_map(|activity| activity.facts.iter());
        let facts = facts.collect::<Vec<_>>();
        if filters.branch.as_deref().is_some_and(|branch| {
            !facts
                .iter()
                .any(|fact| fact.kind == LiteralFactKind::Branch && fact.value == branch.trim())
        }) {
            return Ok(false);
        }
        for (needle, kinds) in [
            (
                filters.workspace.as_deref(),
                &[
                    LiteralFactKind::Workspace,
                    LiteralFactKind::SessionCwd,
                    LiteralFactKind::ToolWorkdir,
                    LiteralFactKind::Project,
                ][..],
            ),
            (filters.file.as_deref(), &[LiteralFactKind::File][..]),
        ] {
            let Some(needle) = needle else {
                continue;
            };
            let matcher = AsciiFoldSubstring::new(needle.trim())?;
            if !facts
                .iter()
                .any(|fact| kinds.contains(&fact.kind) && matcher.matches(fact.value.as_bytes()))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

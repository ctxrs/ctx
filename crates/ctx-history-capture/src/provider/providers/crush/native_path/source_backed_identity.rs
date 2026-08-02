use super::*;

pub(super) fn crush_source_key(project_key: TypedKey) -> CrushSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(CRUSH_SOURCE_ANCHOR_NAMESPACE, project_key)?;
    Ok(SourceKey::derive(
        CaptureProvider::Crush.as_str(),
        CRUSH_SQLITE_SOURCE_FORMAT,
        CRUSH_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionLineage {
    pub(super) parent_session_id: Option<StableEntityId>,
    pub(super) root_session_id: StableEntityId,
    pub(super) agent_type: AgentType,
    pub(super) is_primary: bool,
}

pub(super) fn session_lineage(
    source: &OpenedSource,
    session_parents: &HashMap<String, Option<String>>,
    session: &CrushSessionRow,
    session_id: StableEntityId,
) -> CrushSourceBackedResultV0<SessionLineage> {
    let Some(parent_provider_session_id) = session.parent_session_id.as_deref() else {
        return Ok(SessionLineage {
            parent_session_id: None,
            root_session_id: session_id,
            agent_type: AgentType::Primary,
            is_primary: true,
        });
    };
    let parent_session_id =
        crush_session_id(&source.database.source_key, parent_provider_session_id)?;
    let mut seen = HashSet::from([session.id.clone()]);
    let mut root_provider_session_id = parent_provider_session_id.to_owned();
    for depth in 0..MAX_CRUSH_SESSION_LINEAGE_DEPTH {
        if !seen.insert(root_provider_session_id.clone()) {
            return Err(CrushSourceBackedErrorV0::SessionLineageCycle(
                root_provider_session_id,
            ));
        }
        let next_parent = session_parents
            .get(&root_provider_session_id)
            .cloned()
            .flatten();
        let Some(next_parent) = next_parent else {
            let root_session_id =
                crush_session_id(&source.database.source_key, &root_provider_session_id)?;
            return Ok(SessionLineage {
                parent_session_id: Some(parent_session_id),
                root_session_id,
                agent_type: AgentType::Subagent,
                is_primary: false,
            });
        };
        root_provider_session_id = next_parent;
        if depth + 1 == MAX_CRUSH_SESSION_LINEAGE_DEPTH {
            return Err(CrushSourceBackedErrorV0::SessionLineageTooDeep);
        }
    }
    Err(CrushSourceBackedErrorV0::SessionLineageTooDeep)
}

pub(super) fn crush_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> CrushSourceBackedResultV0<StableEntityId> {
    let session_key = NativeSessionKey::native_id(
        CRUSH_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CRUSH_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?)
}

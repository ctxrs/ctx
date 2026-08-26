use super::*;

pub fn crush_source_key(project_key: TypedKey) -> CrushSourceBackedResultV0<SourceKey> {
    crush_source_key_scoped(project_key, SourceAnchorScope::Unqualified)
}

pub(super) fn crush_source_key_scoped(
    project_key: TypedKey,
    source_scope: SourceAnchorScope,
) -> CrushSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(CRUSH_SOURCE_ANCHOR_NAMESPACE, project_key)?;
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Crush.as_str(),
        CRUSH_SQLITE_SOURCE_FORMAT,
        CRUSH_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionLineage {
    pub(super) parent_session_id: Option<StableEntityId>,
}

pub(super) fn session_lineage(
    source: &OpenedSource,
    session: &CrushSessionRow,
) -> CrushSourceBackedResultV0<SessionLineage> {
    let Some(parent_provider_session_id) = session.parent_session_id.as_deref() else {
        return Ok(SessionLineage {
            parent_session_id: None,
        });
    };
    let parent_session_id =
        crush_session_id(&source.database.source_key, parent_provider_session_id)?;
    Ok(SessionLineage {
        parent_session_id: Some(parent_session_id),
    })
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

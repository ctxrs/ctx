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

pub(super) fn crush_source_revision(
    evidence: &SqliteSourceEvidence,
    schema_fingerprint: &str,
) -> String {
    format!(
        "crush-sqlite-snapshot-v1:capture={CRUSH_CAPTURE_REVISION};policy={CRUSH_POLICY_REVISION};schema={schema_fingerprint};identity={};length={};revision={}",
        hex_bytes(evidence.identity()),
        evidence.length(),
        hex_bytes(evidence.revision()),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MessageAddress {
    pub(super) rowid: i64,
    pub(super) native_record_id: String,
    pub(super) parent_rowid: i64,
    pub(super) provider_session_id: String,
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

pub(super) fn validate_message_locator(
    locator: &SourceRecordLocator,
) -> CrushSourceBackedResultV0<MessageAddress> {
    if locator.source().provider() != CaptureProvider::Crush.as_str()
        || locator.source().source_format() != CRUSH_SQLITE_SOURCE_FORMAT
        || locator.source().schema_variant() != CRUSH_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, .. } = locator.source().anchor() else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    if namespace != CRUSH_SOURCE_ANCHOR_NAMESPACE {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != CRUSH_MESSAGE_RELATION
        || row_version.as_ref() != Some(&TypedKey::Bytes(locator.record_digest().to_vec()))
    {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    let TypedKey::Composite(parts) = primary_key else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::I64(rowid), TypedKey::Utf8(native_record_id), TypedKey::I64(parent_rowid), TypedKey::Utf8(provider_session_id)] =
        parts.as_slice()
    else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    if *rowid <= 0
        || *parent_rowid <= 0
        || native_record_id.is_empty()
        || provider_session_id.is_empty()
    {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    Ok(MessageAddress {
        rowid: *rowid,
        native_record_id: native_record_id.clone(),
        parent_rowid: *parent_rowid,
        provider_session_id: provider_session_id.clone(),
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

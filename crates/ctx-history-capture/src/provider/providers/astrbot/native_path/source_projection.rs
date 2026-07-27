use super::*;

pub(super) fn capture_source(
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    fact: &SessionFact,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::AstrBot,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_format: Some(ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(authority.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(fact.provider_session_id.clone()),
        },
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": authority.user_version,
                "schema_fingerprint": authority.schema_fingerprint,
                "support_level": "supported",
                "provider_session_id": fact.provider_session_id,
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": authority.source_root,
                "source_display_path": authority.display_source_path,
                "source_revision": authority.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::AstrBot,
                    &fact.provider_session_id,
                    ASTRBOT_SQLITE_SOURCE_FORMAT,
                    Some(&authority.raw_source_path),
                ),
                "nativepath_publication": CURSOR_STREAM_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn session(
    committed_store: &Store,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    fact: &SessionFact,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::AstrBot,
        &fact.provider_session_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    if fact.preserve_existing {
        if let Ok(existing) = committed_store.get_session(id) {
            return Ok(existing);
        }
    }
    let mut session_metadata = fact.metadata.clone();
    if let Some(metadata) = session_metadata.as_object_mut() {
        metadata.insert(
            "selected_conversation".to_owned(),
            authority
                .selected_conversation
                .clone()
                .map_or(Value::Null, Value::String),
        );
    }
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::AstrBot,
        external_session_id: Some(fact.provider_session_id.clone()),
        external_agent_id: fact.external_agent_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some(fact.role_hint.to_owned()),
        is_primary: true,
        status: if fact.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": fact.provider_session_id,
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::AstrBot.as_str(),
                    fact.provider_session_id
                ),
                "metadata": session_metadata,
                "nativepath_publication": CURSOR_STREAM_REVISION,
                "source_revision": authority.source_revision,
            }),
        ),
    })
}

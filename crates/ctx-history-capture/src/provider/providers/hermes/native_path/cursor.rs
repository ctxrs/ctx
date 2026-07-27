use super::*;

#[cfg(test)]
std::thread_local! {
    static BEFORE_CURSOR_PUBLICATION_REVALIDATION_HOOK:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {
            std::cell::RefCell::new(None)
        };
}

#[cfg(test)]
pub(in crate::provider::providers::hermes) struct CursorPublicationRevalidationHookGuard;

#[cfg(test)]
impl Drop for CursorPublicationRevalidationHookGuard {
    fn drop(&mut self) {
        BEFORE_CURSOR_PUBLICATION_REVALIDATION_HOOK.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(in crate::provider::providers::hermes) fn install_before_cursor_publication_revalidation_hook(
    hook: impl FnOnce() + 'static,
) -> CursorPublicationRevalidationHookGuard {
    BEFORE_CURSOR_PUBLICATION_REVALIDATION_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(
            slot.is_none(),
            "Hermes cursor-publication revalidation hook is already installed"
        );
        *slot = Some(Box::new(hook));
    });
    CursorPublicationRevalidationHookGuard
}

pub(super) fn core_plan(
    stored: Option<SyncCursor>,
    proposed_source_identity: &str,
    locator_identity: &str,
    source_revision: &str,
) -> Result<CorePlan> {
    let Some(stored) = stored else {
        return Ok(CorePlan {
            expected: None,
            cursor: HermesStoreCursor {
                version: HERMES_CURSOR_VERSION,
                canonical_source_identity: proposed_source_identity.to_owned(),
                locator_identity: locator_identity.to_owned(),
                source_revision: source_revision.to_owned(),
                frontier: HermesFrontier::initial(),
                terminal: false,
                generation: 0,
                rejected_records: 0,
                retired: false,
            },
            migration: false,
        });
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let mut cursor: HermesStoreCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "Hermes NativePath committed cursor payload is malformed".to_owned(),
                )
            })?;
        validate_cursor(&cursor)?;
        let same_source = cursor.source_revision == source_revision && !cursor.retired;
        if !same_source {
            cursor.frontier = HermesFrontier::initial();
            cursor.terminal = false;
            cursor.retired = false;
            cursor.generation = cursor.generation.checked_add(1).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Hermes NativePath cursor generation overflowed".to_owned(),
                )
            })?;
            cursor.rejected_records = 0;
            cursor.source_revision = source_revision.to_owned();
        }
        cursor.locator_identity = locator_identity.to_owned();
        return Ok(CorePlan {
            expected: Some(stored),
            cursor,
            migration: false,
        });
    }
    let legacy =
        CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Hermes cursor is neither NativePath nor a released migration cursor".to_owned(),
            )
        })?;
    validate_released_cursor(&legacy)?;
    Ok(CorePlan {
        expected: Some(stored),
        cursor: HermesStoreCursor {
            version: HERMES_CURSOR_VERSION,
            canonical_source_identity: proposed_source_identity.to_owned(),
            locator_identity: locator_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            frontier: HermesFrontier::initial(),
            terminal: false,
            generation: 1,
            rejected_records: legacy.rejected_records(),
            retired: false,
        },
        migration: true,
    })
}

pub(super) fn validate_released_cursor(cursor: &CertifiedProviderCursor) -> Result<()> {
    let position = cursor.native_position();
    let valid_position = position.value() == [0]
        || (position.value().len() == 17 && matches!(position.value()[0], 1 | 2));
    let _: () = cursor.parser_checkpoint().deserialize()?;
    if cursor.parser_revision() != RELEASED_HERMES_CAPTURE_REVISION
        || cursor.policy_revision() != RELEASED_HERMES_POLICY_REVISION
        || position.kind() != RELEASED_HERMES_POSITION_KIND
        || !valid_position
    {
        return Err(CaptureError::InvalidPayload(
            "Hermes cursor is not the released SQLite cursor shape".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_cursor(cursor: &HermesStoreCursor) -> Result<()> {
    if cursor.version != HERMES_CURSOR_VERSION
        || cursor.canonical_source_identity.is_empty()
        || cursor.locator_identity.is_empty()
        || cursor.source_revision.is_empty()
        || (cursor.retired && !cursor.terminal)
    {
        return Err(CaptureError::InvalidPayload(
            "Hermes NativePath cursor authority is invalid".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn output_plan(
    profile: &ImportProfile,
    machine_id: &str,
    core: &HermesStoreCursor,
    source_revision: &str,
) -> Result<OutputPlan> {
    let Some(sink) = profile.sink() else {
        return Ok(OutputPlan {
            source: OutputSourceIdentity {
                provider: CaptureProvider::Hermes.as_str().to_owned(),
                namespace_id: machine_id.to_owned(),
                source_id: core.canonical_source_identity.clone(),
            },
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            scan_frontier: HermesFrontier::initial(),
            disposition: ProOutputSourceDisposition::NewSource,
            terminal: false,
            enabled: false,
            initially_behind: false,
        });
    };
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Hermes.as_str().to_owned(),
        namespace_id: machine_id.to_owned(),
        source_id: core.canonical_source_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(OutputPlan {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_frontier: None,
                scan_frontier: HermesFrontier::initial(),
                disposition: ProOutputSourceDisposition::NewSource,
                terminal: false,
                enabled: false,
                initially_behind: true,
            });
        }
    };
    output_plan_from_progress(
        source,
        progress,
        core,
        source_revision,
        sink.materializer_revision(),
    )
}

pub(super) fn output_plan_from_progress(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    core: &HermesStoreCursor,
    source_revision: &str,
    materializer_revision: &str,
) -> Result<OutputPlan> {
    let Some(progress) = progress else {
        return Ok(OutputPlan {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            scan_frontier: HermesFrontier::initial(),
            disposition: ProOutputSourceDisposition::NewSource,
            terminal: false,
            enabled: true,
            initially_behind: false,
        });
    };
    let progress_frontier = progress.cursor.as_ref().map(output_frontier).transpose()?;
    if progress.observed_revision == source_revision
        && progress.parser_revision == HERMES_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
    {
        let frontier = progress_frontier.ok_or_else(|| {
            CaptureError::InvalidPayload("Hermes output progress has no native cursor".to_owned())
        })?;
        if frontier.next_ordinal > core.frontier.next_ordinal
            || (frontier.next_ordinal == core.frontier.next_ordinal && frontier != core.frontier)
            || (progress.terminal && !core.terminal)
        {
            return Err(CaptureError::InvalidPayload(
                "Hermes output progress is ahead of certified Core".to_owned(),
            ));
        }
        return Ok(OutputPlan {
            source,
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_frontier: progress
                .cursor
                .as_ref()
                .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
                .transpose()
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            scan_frontier: frontier,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            terminal: progress.terminal,
            enabled: !(progress.terminal && core.terminal && frontier == core.frontier),
            initially_behind: false,
        });
    }
    Ok(OutputPlan {
        source,
        source_epoch: progress.source_epoch.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("Hermes output source epoch overflowed".to_owned())
        })?,
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier: progress
            .cursor
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        scan_frontier: HermesFrontier::initial(),
        disposition: ProOutputSourceDisposition::Rewrite,
        terminal: false,
        enabled: true,
        initially_behind: false,
    })
}

pub(super) fn output_frontier(cursor: &OutputNativeCursor) -> Result<HermesFrontier> {
    if cursor.version != HERMES_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Hermes output cursor version is unsupported".to_owned(),
        ));
    }
    HermesFrontier::decode(&cursor.payload)
}

pub(super) fn safe_frontier(frontier: HermesFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(HERMES_FRONTIER_VERSION, frontier.encode())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn sync_cursor(
    context: &PublicationContext<'_>,
    cursor: &HermesStoreCursor,
) -> Result<SyncCursor> {
    Ok(SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Hermes.as_str(),
                context.adapter.machine_id,
                context.cursor_stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.adapter.machine_id.clone(),
        stream: context.cursor_stream.to_owned(),
        cursor: serde_json::to_string(cursor)?,
        last_synced_at: Some(context.adapter.imported_at),
        timestamps: timestamps(context.adapter.imported_at),
    })
}

pub(super) fn publication_id(
    transition: &NativePathCursorTransition,
    cursor: &HermesStoreCursor,
) -> String {
    let mut digest = Sha256::new();
    digest.update(HERMES_PUBLICATION_DOMAIN);
    digest.update(transition.key().stream().as_bytes());
    digest.update(cursor.version.to_be_bytes());
    digest.update((cursor.canonical_source_identity.len() as u64).to_be_bytes());
    digest.update(cursor.canonical_source_identity.as_bytes());
    digest.update((cursor.locator_identity.len() as u64).to_be_bytes());
    digest.update(cursor.locator_identity.as_bytes());
    digest.update(cursor.generation.to_be_bytes());
    digest.update(cursor.frontier.encode());
    digest.update([u8::from(cursor.terminal)]);
    digest.update(cursor.rejected_records.to_be_bytes());
    digest.update([u8::from(cursor.retired)]);
    digest.update(cursor.source_revision.as_bytes());
    format!("hermes-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
    inventory_token: Option<&str>,
) -> String {
    let revision = format!(
        "hermes-nativepath-snapshot-v1:capture={HERMES_CAPTURE_REVISION};policy={HERMES_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    );
    let Some(token) = inventory_token else {
        return revision;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-hermes-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
}

pub(super) fn source_metadata(context: &PublicationContext<'_>) -> Value {
    json!({
        "adapter": HERMES_SQLITE_SOURCE_FORMAT,
        "sqlite_user_version": context.sqlite_user_version,
        "schema_fingerprint": context.schema_fingerprint,
        "upstream_schema_version_at_research": 17,
        "capture_policy": "provider_owned_nativepath_v1",
    })
}

pub(super) fn revalidate_source(context: &PublicationContext<'_>) -> Result<()> {
    if context.source_snapshot.revalidate(context.canonical_path)? {
        Ok(())
    } else {
        Err(CaptureError::SourceChangedDuringCapture)
    }
}

pub(super) fn revalidate_source_before_cursor_publication(
    context: &PublicationContext<'_>,
) -> Result<()> {
    #[cfg(test)]
    BEFORE_CURSOR_PUBLICATION_REVALIDATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
    revalidate_source(context)
}

pub(super) fn session_metadata(row: &HermesSessionRow) -> Value {
    json!({
        "source_format": HERMES_SQLITE_SOURCE_FORMAT,
        "source": row.source,
        "title": row.title,
        "model": row.model,
        "model_config": row.model_config.as_deref().map(
            crate::provider::normalization::provider_json_text
        ),
        "end_reason": row.end_reason,
        "message_count": row.message_count,
        "tool_call_count": row.tool_call_count,
        "tokens": {
            "input": row.input_tokens,
            "output": row.output_tokens,
            "cache_read": row.cache_read_tokens,
            "cache_write": row.cache_write_tokens,
            "reasoning": row.reasoning_tokens,
        },
        "git": {
            "branch": row.git_branch,
            "repo_root": row.git_repo_root,
        },
        "billing": {
            "provider": row.billing_provider,
            "base_url": row.billing_base_url,
            "mode": row.billing_mode,
            "estimated_cost_usd": row.estimated_cost_usd,
            "actual_cost_usd": row.actual_cost_usd,
        },
        "archived": row.archived != 0,
    })
}

pub(super) fn relationship_placeholder(
    context: &PublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Hermes,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.adapter.imported_at,
        ended_at: None,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

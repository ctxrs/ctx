use super::*;

#[derive(Debug)]
pub(super) struct SourceProjectionIdentity {
    pub(super) canonical_source_key: String,
    pub(super) proposed_source_namespace: String,
    pub(super) root_namespace: String,
    pub(super) cursor_stream: String,
}

pub(super) fn source_projection_identity(
    source: &CodexCatalogSource,
) -> VerticalResult<SourceProjectionIdentity> {
    let raw_source_path = source.source_path.display().to_string();
    let native_session_id = source
        .catalog_native_session_id
        .as_deref()
        .ok_or(CodexNativeVerticalError::MissingOwner)?;
    let root_namespace = provider_source_identity(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        Some(&source.source_root),
        None,
        None,
        &Value::Null,
    )
    .ok_or(CodexNativeVerticalError::CorruptCursor(
        "canonical root namespace is unavailable",
    ))?;
    let proposed_source_namespace =
        canonical_source_namespace(&source.source_root, native_session_id)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &raw_source_path,
    );
    let canonical_source_key = proposed_source_namespace.clone();
    Ok(SourceProjectionIdentity {
        canonical_source_key,
        proposed_source_namespace,
        root_namespace,
        cursor_stream,
    })
}

pub(super) fn canonical_source_namespace(
    source_root: &str,
    native_session_id: &str,
) -> VerticalResult<String> {
    let root_namespace = provider_source_identity(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        Some(source_root),
        None,
        None,
        &Value::Null,
    )
    .ok_or(CodexNativeVerticalError::CorruptCursor(
        "canonical root namespace is unavailable",
    ))?;
    Ok(format!(
        "codex-nativepath:{}",
        stable_capture_uuid(
            &format!("{root_namespace}:{native_session_id}"),
            "canonical-source"
        )
    ))
}

pub(super) fn source_locator_identity(
    cursor_stream: &str,
    canonical_source_identity: &str,
) -> String {
    format!("{cursor_stream}#{canonical_source_identity}")
}

pub(super) fn source_generation_key(
    context: &CodexPublicationContext,
    canonical_source_identity: &str,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: context.options.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        locator_identity: source_locator_identity(
            &context.cursor_stream,
            &context.proposed_source_namespace,
        ),
        cursor_stream: context.cursor_stream.clone(),
        source_revision: context.source_revision.clone(),
        generation_id: format!("codex-nativepath-generation-v1:{}", context.generation),
    }
}

pub(super) fn load_committed_source(
    store: &Store,
    source: &CodexCatalogSource,
    options: &CodexNativeStoreOptions,
    identity: &SourceProjectionIdentity,
) -> VerticalResult<Option<CodexCommittedSource>> {
    let Some(cursor) = store.get_sync_cursor(None, &options.machine_id, &identity.cursor_stream)?
    else {
        return Ok(None);
    };
    let committed = match decode_native_path_committed_cursor(&cursor.cursor) {
        Ok(committed) => committed,
        Err(_) => return migration_committed_source(cursor, None, None),
    };
    let canonical_journal_frontier = committed.journal_checkpoint().cloned();
    let certified = CertifiedProviderCursor::decode(committed.provider_cursor())
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("provider cursor is malformed"))?;
    if certified.parser_revision() != capture_revision()
        || certified.policy_revision() != policy_revision()
        || certified.native_position().kind() != CODEX_NATIVE_POSITION_KIND
    {
        return migration_committed_source(cursor, Some(&certified), canonical_journal_frontier);
    }
    let wire: CodexNativeStoreCursorWire = certified
        .parser_checkpoint()
        .deserialize()
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("checkpoint envelope is malformed"))?;
    if !(CODEX_NATIVE_CURSOR_MIN_READ_VERSION..=CODEX_NATIVE_CURSOR_VERSION).contains(&wire.version)
    {
        return Err(CodexNativeVerticalError::CorruptCursor(
            "checkpoint identity/version mismatch",
        ));
    }
    let encoded_checkpoint = wire.checkpoint.encode().map_err(CaptureError::from)?;
    let checkpoint = CodexNativeCheckpoint::decode(&encoded_checkpoint)
        .map_err(|_| CodexNativeVerticalError::CorruptCursor("checkpoint authority is invalid"))?;
    let frontier = decode_frontier(certified.native_position().value())?;
    let terminal_frontier = frontier_from_checkpoint(&checkpoint);
    let certified_observation = wire.certified_observation.clone().or_else(|| {
        (wire.version < CODEX_NATIVE_CURSOR_VERSION
            && checkpoint.terminal()
            && checkpoint.observation.len == checkpoint.complete_prefix_end())
        .then(|| checkpoint.observation.clone())
    });
    if wire.version == CODEX_NATIVE_CURSOR_VERSION && certified_observation.is_none() {
        return Err(CodexNativeVerticalError::CorruptCursor(
            "certified source observation is missing",
        ));
    }
    let persisted_observation_revision = certified_observation
        .as_ref()
        .map(source_observation_revision);
    if (persisted_observation_revision
        .as_deref()
        .is_some_and(|revision| certified.source_revision() != revision)
        && certified.source_revision() != source_revision(&checkpoint.full_revision_sha256))
        || frontier.complete_prefix_end > terminal_frontier.complete_prefix_end
        || frontier.next_raw_ordinal > terminal_frontier.next_raw_ordinal
    {
        return Err(CodexNativeVerticalError::CorruptCursor(
            "certified source revision/frontier mismatch",
        ));
    }
    let source_identity = CodexSourceIdentity::new(
        identity.canonical_source_key.clone(),
        source.source_root.clone(),
        source.source_path.clone(),
    )?;
    let proof_observation_is_safe = certified_observation.as_ref().is_some_and(|observation| {
        source.catalog_observation == *observation
            || (wire.phase != CodexNativeCursorPhase::Rebuilding
                && checkpoint.observation == *observation
                && source.catalog_observation.len > observation.len)
    });
    Ok(Some(CodexCommittedSource {
        expected_store_cursor: cursor,
        proof: (wire.version >= 2
            && wire.canonical_source_key == identity.canonical_source_key
            && frontier == terminal_frontier
            && proof_observation_is_safe)
            .then(|| {
                CodexAppendProof::new(
                    source_identity,
                    CodexCheckpointGeneration::new(wire.generation),
                    checkpoint.clone(),
                )
            }),
        generation: wire.generation,
        frontier: if wire.version >= 2 {
            frontier
        } else {
            initial_codex_frontier()
        },
        source_revision: certified.source_revision().to_owned(),
        rejected_records: certified.rejected_records(),
        canonical_journal_frontier,
        retained_events: wire.retained_events,
        skipped_events: wire.skipped_events,
        certified_observation,
        phase: wire.phase,
    }))
}

pub(super) fn migration_committed_source(
    cursor: SyncCursor,
    certified: Option<&CertifiedProviderCursor>,
    canonical_journal_frontier: Option<JournalCheckpoint>,
) -> VerticalResult<Option<CodexCommittedSource>> {
    Ok(Some(CodexCommittedSource {
        expected_store_cursor: cursor,
        proof: None,
        generation: 0,
        frontier: initial_codex_frontier(),
        source_revision: certified
            .map(|cursor| cursor.source_revision().to_owned())
            .unwrap_or_default(),
        rejected_records: certified
            .map(CertifiedProviderCursor::rejected_records)
            .unwrap_or_default(),
        canonical_journal_frontier,
        retained_events: 0,
        skipped_events: 0,
        certified_observation: None,
        phase: CodexNativeCursorPhase::Complete,
    }))
}

// These arguments are the distinct certified cursor authorities. Keeping them
// explicit avoids constructing another aggregate on every prepared page.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_next_store_cursor(
    options: &CodexNativeStoreOptions,
    identity: &SourceProjectionIdentity,
    generation: u64,
    source_revision: &str,
    checkpoint: &CodexNativeCheckpoint,
    certified_observation: &CodexFileObservation,
    phase: CodexNativeCursorPhase,
    rejected_records: u64,
    retained_events: u64,
    skipped_events: u64,
) -> VerticalResult<SyncCursor> {
    let frontier = frontier_from_checkpoint(checkpoint);
    let parser_checkpoint =
        BoundedParserCheckpoint::from_serializable(&CodexNativeStoreCursorWire {
            version: CODEX_NATIVE_CURSOR_VERSION,
            canonical_source_key: identity.canonical_source_key.clone(),
            generation,
            checkpoint: checkpoint.clone(),
            certified_observation: Some(certified_observation.clone()),
            phase,
            retained_events,
            skipped_events,
        })?;
    let certified = CertifiedProviderCursor::new(
        source_revision,
        capture_revision(),
        policy_revision(),
        NativePosition::new(CODEX_NATIVE_POSITION_KIND, encode_frontier(&frontier)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        parser_checkpoint,
    )?
    .with_rejected_records(rejected_records);
    Ok(certified_provider_sync_cursor(
        CaptureProvider::Codex,
        &options.machine_id,
        identity.cursor_stream.clone(),
        &certified,
        options.imported_at,
    )?)
}

pub(super) fn build_context_store_cursor(
    context: &CodexPublicationContext,
    frontier: &CodexNativeFrontier,
    phase: CodexNativeCursorPhase,
) -> VerticalResult<SyncCursor> {
    let parser_checkpoint =
        BoundedParserCheckpoint::from_serializable(&CodexNativeStoreCursorWire {
            version: CODEX_NATIVE_CURSOR_VERSION,
            canonical_source_key: context.canonical_source_key.clone(),
            generation: context.generation,
            checkpoint: context.checkpoint.clone(),
            certified_observation: Some(context.certified_observation.clone()),
            phase,
            retained_events: context.retained_events,
            skipped_events: context.skipped_events,
        })?;
    let certified = CertifiedProviderCursor::new(
        context.source_revision.clone(),
        capture_revision(),
        policy_revision(),
        NativePosition::new(CODEX_NATIVE_POSITION_KIND, encode_frontier(frontier)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        parser_checkpoint,
    )?
    .with_rejected_records(context.rejected_records);
    Ok(certified_provider_sync_cursor(
        CaptureProvider::Codex,
        &context.options.machine_id,
        context.cursor_stream.clone(),
        &certified,
        context.options.imported_at,
    )?)
}

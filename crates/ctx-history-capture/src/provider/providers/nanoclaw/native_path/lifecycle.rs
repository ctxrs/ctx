use super::*;

pub(super) fn retire_missing_project(
    store: &mut Store,
    requested_root: &Path,
    context: &ProviderAdapterContext,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let cursor_path_identity = provider_path_identity(requested_root)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &cursor_path_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested_root.to_path_buf(),
            reason: "NanoClaw project root or data/v2.db does not exist",
        });
    };
    let committed = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => committed,
        Err(_) => {
            if !is_released_nanoclaw_legacy_cursor(&stored.cursor)? {
                return Err(CaptureError::InvalidPayload(
                    "NanoClaw cursor is neither a released legacy cursor nor a NativePath cursor"
                        .to_owned(),
                ));
            }
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    };
    let anchor_source_id = project_anchor_source_id(&cursor_path_identity);
    let cursor = NanoClawNativeCursor::decode(committed.provider_cursor(), anchor_source_id)?;
    let anchor = store.get_capture_source(cursor.anchor_source_id)?;
    let canonical_source_identity =
        anchor
            .descriptor
            .source_identity
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw project anchor has no canonical source identity",
            ))?;
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            &cursor_stream,
            cursor.encode()?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: cursor_path_identity,
        cursor_stream,
        expected_canonical_source_identity: canonical_source_identity,
        expected_source_revision: cursor.source_revision,
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement, &transition);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    ensure_active_journal(store)?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, transition.next().cursor.len())?,
        )?;
        let disposition = if matches!(
            group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
            NativePathCursorSetClassification::AllNextSameGroup { .. }
        ) {
            ProviderSourceRouteRetirementDisposition::AlreadyRetired
        } else {
            let disposition = group.retire_provider_source_route(&retirement)?;
            group.prepare_journal_checkpoint()?;
            group.publish_cursor_set()?;
            disposition
        };
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(
            if disposition == ProviderSourceRouteRetirementDisposition::Retired {
                ProviderImportWorkResult::Changed
            } else {
                ProviderImportWorkResult::NoOp
            },
        );
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    live: &NanoClawLiveProject,
    snapshot: &NanoClawProjectSnapshot,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(store, live, snapshot, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "nanoclaw_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

pub(super) fn replay_outputs(
    store: &Store,
    live: &NanoClawLiveProject,
    snapshot: &NanoClawProjectSnapshot,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    if !snapshot.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let Some(stored) = store.get_sync_cursor(None, &live.machine_id, &live.cursor_stream)? else {
        return Ok(());
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = NanoClawNativeCursor::decode(committed.provider_cursor(), live.anchor_source_id)?;
    if !cursor.terminal || cursor.source_revision != live.source_revision {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw output replay requires the terminal current Core frontier".to_owned(),
        ));
    }
    let anchor = store.get_capture_source(cursor.anchor_source_id)?;
    let canonical_source_identity =
        anchor
            .descriptor
            .source_identity
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw project anchor has no canonical source identity",
            ))?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::NanoClaw.as_str().to_owned(),
        namespace_id: live.source_root.clone(),
        source_id: canonical_source_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let final_frontier = output_frontier(&cursor)?;
    if progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == cursor.source_revision
            && progress.parser_revision == NANOCLAW_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.terminal
            && progress.cursor.as_ref().is_some_and(|prior| {
                prior.version == final_frontier.version && prior.payload == final_frontier.bytes
            })
    }) {
        return Ok(());
    }
    let state = output_state(progress, &cursor, sink.materializer_revision())?;
    let expected_frontier = NativeSafeFrontier::new(
        NANOCLAW_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&NanoClawFrontier::initial())?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: output_source,
        source_epoch: state.source_epoch,
        observed_revision: cursor.source_revision.clone(),
        parser_revision: NANOCLAW_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_frontier,
        observations: Vec::new(),
    };
    let page = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(
            CaptureProvider::NanoClaw.as_str(),
            canonical_source_identity,
        ),
        expected_frontier,
        final_frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: NANOCLAW_OUTPUT_PAGE_BYTES,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(page, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "nanoclaw_nativepath_output_replay",
            format!("{:?}", failure.output_error),
        ));
    }
    Ok(())
}

pub(super) struct NanoClawOutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

pub(super) fn output_state(
    progress: Option<ProOutputProgress>,
    cursor: &NanoClawNativeCursor,
    materializer_revision: &str,
) -> Result<NanoClawOutputState> {
    let Some(progress) = progress else {
        return Ok(NanoClawOutputState {
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let expected_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let rewrite = progress.observed_revision != cursor.source_revision
        || progress.parser_revision != NANOCLAW_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != materializer_revision
        || progress.source_epoch != cursor.generation;
    Ok(NanoClawOutputState {
        source_epoch: if rewrite {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "NanoClaw output source epoch exhausted",
                ))?
        } else {
            progress.source_epoch
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
    })
}

pub(super) fn output_frontier(cursor: &NanoClawNativeCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        NANOCLAW_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&json!({
            "generation": cursor.generation,
            "source_revision": cursor.source_revision,
            "frontier": cursor.frontier,
            "prefix_digest": cursor.prefix_digest,
            "terminal": cursor.terminal,
        }))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn live_project(
    root: &Path,
    central_path: &Path,
    source_revision: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
) -> Result<NanoClawLiveProject> {
    let cursor_path_identity = provider_path_identity(root)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &cursor_path_identity,
    );
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(root)
        .display()
        .to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "NanoClaw project has no canonical source identity",
    ))?;
    Ok(NanoClawLiveProject {
        root: root.to_path_buf(),
        central_path: central_path.to_path_buf(),
        machine_id: context.machine_id.clone(),
        locator_identity: cursor_path_identity.clone(),
        cursor_stream,
        proposed_source_identity,
        raw_source_path,
        source_root,
        source_revision: source_revision.to_owned(),
        user_version,
        schema_fingerprint: schema_fingerprint.to_owned(),
        anchor_source_id: project_anchor_source_id(&cursor_path_identity),
    })
}

pub(super) fn requested_project_root(path: &Path) -> Result<PathBuf> {
    let root = if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        path.parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw data/v2.db has no project root",
            })?
    } else {
        path.to_path_buf()
    };
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()?.join(root))
    }
}

pub(super) fn decode_prior_cursor(
    stored: Option<SyncCursor>,
    anchor_source_id: Uuid,
) -> Result<PriorCursor> {
    let Some(stored) = stored else {
        return Ok(PriorCursor::None);
    };
    match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => {
            let retired = committed
                .publication_id()
                .starts_with("nanoclaw-nativepath-retire:");
            Ok(PriorCursor::Native {
                cursor: NanoClawNativeCursor::decode(
                    committed.provider_cursor(),
                    anchor_source_id,
                )?,
                stored,
                retired,
            })
        }
        Err(_) => {
            if is_released_nanoclaw_legacy_cursor(&stored.cursor)? {
                Ok(PriorCursor::Legacy(stored))
            } else {
                Err(CaptureError::InvalidPayload(
                    "NanoClaw cursor is neither a released legacy cursor nor a NativePath cursor"
                        .to_owned(),
                ))
            }
        }
    }
}

pub(super) fn is_released_nanoclaw_legacy_cursor(encoded: &str) -> Result<bool> {
    let Some(cursor) = CertifiedProviderCursor::decode_if_certified(encoded)? else {
        return Ok(false);
    };
    if cursor.parser_revision() != NANOCLAW_CAPTURE_REVISION
        || cursor.policy_revision() != NANOCLAW_POLICY_REVISION
        || cursor.native_position().kind() != NANOCLAW_LEGACY_POSITION_KIND
    {
        return Ok(false);
    }
    let _: () = cursor.parser_checkpoint().deserialize()?;
    let value = cursor.native_position().value();
    if value == [0] {
        return Ok(true);
    }
    if value.len() != 27 || value[0] != 1 || !matches!(value[9], 1 | 2) || value[18] > 2 {
        return Ok(false);
    }
    let session_rowid = decode_legacy_ordered_i64(&value[10..18])?;
    let message_rowid = decode_legacy_ordered_i64(&value[19..27])?;
    let valid = session_rowid > 0
        && !(value[9] == 1 && (value[18] != 0 || message_rowid != 0))
        && !(value[18] == 0 && message_rowid != 0);
    Ok(valid)
}

pub(super) fn decode_legacy_ordered_i64(bytes: &[u8]) -> Result<i64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload(
            "NanoClaw released cursor integer has an invalid width".to_owned(),
        )
    })?;
    Ok((u64::from_be_bytes(bytes) ^ (1_u64 << 63)) as i64)
}

pub(super) fn provider_sync_cursor(
    machine_id: &str,
    stream: &str,
    cursor: String,
    at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!("provider-cursor:{machine_id}:{stream}"),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(at),
        timestamps: timestamps(at),
    }
}

pub(super) fn nanoclaw_locator_observation(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
) -> ProviderSourceLocatorObservation {
    ProviderSourceLocatorObservation {
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: live.locator_identity.clone(),
        cursor_stream: live.cursor_stream.clone(),
        proposed_source_identity: live.proposed_source_identity.clone(),
        raw_source_path: Some(live.root.display().to_string()),
        source_revision: live.source_revision.clone(),
        observed_at_ms: context.imported_at.timestamp_millis(),
    }
}

pub(super) fn canonical_source_identity(store: &Store, anchor_source_id: Uuid) -> Result<String> {
    store
        .get_capture_source(anchor_source_id)?
        .descriptor
        .source_identity
        .ok_or(CaptureError::SystemInvariant(
            "NanoClaw project anchor has no canonical source identity",
        ))
}

pub(super) fn nanoclaw_capture_source_ids(
    store: &Store,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
) -> Result<Vec<Uuid>> {
    Ok(store
        .list_capture_sources()?
        .into_iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::NanoClaw
                && source.descriptor.machine_id == context.machine_id
                && source.descriptor.source_format.as_deref() == Some(NANOCLAW_SOURCE_FORMAT)
                && source.descriptor.source_identity.as_deref() == Some(canonical_source_identity)
                && source.sync.deleted_at.is_none()
        })
        .map(|source| source.id)
        .collect())
}

pub(super) fn generation_key(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
    generation: u64,
) -> NativePathSourceGenerationKey {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_GENERATION_DOMAIN);
    hash_field(&mut digest, live.locator_identity.as_bytes());
    hash_field(&mut digest, live.source_revision.as_bytes());
    digest.update(generation.to_be_bytes());
    NativePathSourceGenerationKey {
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        locator_identity: live.locator_identity.clone(),
        cursor_stream: live.cursor_stream.clone(),
        source_revision: live.source_revision.clone(),
        generation_id: format!("nanoclaw-generation-v1:{}", hex(&digest.finalize())),
    }
}

pub(super) fn deduplicate_retained_entities(retained: &mut NativePathRetainedSourceEntities) {
    retained.capture_source_ids.sort_unstable();
    retained.capture_source_ids.dedup();
    retained.session_ids.sort_unstable();
    retained.session_ids.dedup();
    retained.event_ids.sort_unstable();
    retained.event_ids.dedup();
}

pub(super) fn source_stage_publication_id(
    live: &NanoClawLiveProject,
    prior: &NanoClawNativeCursor,
    next: &NanoClawNativeCursor,
    source_ids: &[Uuid],
) -> String {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_SOURCE_STAGE_DOMAIN);
    hash_field(&mut digest, live.cursor_stream.as_bytes());
    hash_field(&mut digest, live.source_revision.as_bytes());
    digest.update(prior.generation.to_be_bytes());
    if let Ok(encoded) = serde_json::to_vec(&prior.source_stage) {
        hash_field(&mut digest, &encoded);
    }
    if let Ok(encoded) = serde_json::to_vec(&next.source_stage) {
        hash_field(&mut digest, &encoded);
    }
    for source_id in source_ids {
        hash_field(&mut digest, source_id.as_bytes());
    }
    format!("nanoclaw-source-stage-v1:{}", hex(&digest.finalize()))
}

pub(super) fn omission_publication_id(
    live: &NanoClawLiveProject,
    prior: &NanoClawNativeCursor,
    next: &NanoClawNativeCursor,
    page: &ctx_history_store::NativePathSourceRetirementPage,
) -> String {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_OMISSION_DOMAIN);
    hash_field(&mut digest, live.cursor_stream.as_bytes());
    hash_field(&mut digest, live.source_revision.as_bytes());
    digest.update(prior.generation.to_be_bytes());
    if let Ok(encoded) = serde_json::to_vec(&prior.retirement) {
        hash_field(&mut digest, &encoded);
    }
    if let Ok(encoded) = serde_json::to_vec(&next.retirement) {
        hash_field(&mut digest, &encoded);
    }
    if let Some(frontier) = &page.next_after {
        hash_field(&mut digest, frontier.kind.as_str().as_bytes());
        hash_field(&mut digest, frontier.id.as_bytes());
    }
    digest.update([u8::from(page.done)]);
    digest.update((page.inspected as u64).to_be_bytes());
    digest.update((page.retired as u64).to_be_bytes());
    format!("nanoclaw-omission-v1:{}", hex(&digest.finalize()))
}

pub(super) fn publication_id(
    live: &NanoClawLiveProject,
    page: &NanoClawNativePage,
    cursor: &NanoClawNativeCursor,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_PUBLICATION_DOMAIN);
    hash_field(&mut digest, live.cursor_stream.as_bytes());
    hash_field(&mut digest, live.source_revision.as_bytes());
    hash_field(&mut digest, &serde_json::to_vec(&page.expected_frontier)?);
    hash_field(&mut digest, &serde_json::to_vec(&page.next_frontier)?);
    hash_field(&mut digest, cursor.prefix_digest.as_bytes());
    digest.update([u8::from(page.terminal)]);
    for unit in &page.units {
        hash_field(&mut digest, &serde_json::to_vec(unit)?);
    }
    Ok(format!("nanoclaw-nativepath:{}", hex(&digest.finalize())))
}

pub(super) fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(NANOCLAW_NATIVE_RETIREMENT_DOMAIN);
    hash_field(&mut digest, retirement.machine_id.as_bytes());
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("nanoclaw-nativepath-retire:{}", hex(&digest.finalize()))
}

pub(super) fn project_anchor_source_id(cursor_path_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!("nanoclaw-nativepath-project:{cursor_path_identity}"),
        "source",
    )
}

pub(super) fn provider_session_id(session: &NanoClawSessionRow) -> String {
    format!("{}/{}", session.agent_group_id, session.id)
}

pub(super) fn initial_prefix_digest() -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-nanoclaw-nativepath-prefix-v1\0");
    hex(&digest.finalize())
}

pub(super) fn line_number(ordinal: u64) -> usize {
    ordinal.min(usize::MAX as u64) as usize
}

pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

pub(super) fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(super) fn ensure_active_journal(store: &Store) -> Result<()> {
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn mark_output_behind(sink: Option<&dyn ProOutputSink>, message: &str) {
    if let Some(sink) = sink {
        sink.mark_behind(ProOutputSinkError::new(
            "nanoclaw_nativepath_output_replay",
            message,
        ));
    }
}

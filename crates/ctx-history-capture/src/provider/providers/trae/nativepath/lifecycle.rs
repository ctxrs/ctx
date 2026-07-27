use super::*;

pub(super) fn root_cursor_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Trae,
        TRAE_ROOT_SOURCE_FORMAT,
        &identity,
    ))
}

pub(super) fn load_root_manifest(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<TraeRootManifest>> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: TraeRootManifest = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Trae root manifest is corrupt".into()))?;
    if manifest.version != TRAE_ROOT_CURSOR_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Trae root manifest version is unsupported".into(),
        ));
    }
    Ok(Some(manifest))
}

pub(super) fn publish_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    stream: &str,
    manifest: &TraeRootManifest,
) -> Result<bool> {
    let encoded = serde_json::to_string(manifest)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    if let Some(stored) = &stored {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        if committed.provider_cursor() == encoded {
            return Ok(false);
        }
    }
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encoded,
            context.imported_at,
        ),
    );
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-nativepath-root-v1\0");
    digest.update(transition.next().cursor.as_bytes());
    let publication_id = format!("trae-nativepath-root-v1:{:x}", digest.finalize());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len().max(1))?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let changed = matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllExpected
    );
    if changed {
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    Ok(changed)
}

pub(super) fn retire_missing_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &TraeRouteState,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &route.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Trae root manifest references a missing source cursor".into(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = TraeNativeCursor::decode(committed.provider_cursor())?;
    if cursor.locator_identity != route.locator_identity
        || cursor.canonical_source_identity != route.canonical_source_identity
    {
        return Err(CaptureError::InvalidPayload(
            "Trae retirement route no longer matches its committed cursor".into(),
        ));
    }
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Trae,
        source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor_stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            route.cursor_stream.clone(),
            committed.provider_cursor().to_owned(),
            context.imported_at,
        ),
    );
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-nativepath-retirement-v1\0");
    digest.update(route.locator_identity.as_bytes());
    digest.update(route.canonical_source_identity.as_bytes());
    digest.update(route.source_revision.as_bytes());
    digest.update(format!("{:?}", reason).as_bytes());
    let publication_id = format!("trae-nativepath-retirement-v1:{:x}", digest.finalize());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
        };
    group.commit()?;
    Ok(changed)
}

pub(super) fn replay_source_outputs_or_mark_behind(
    path: &Path,
    source_root: &Path,
    context: &ProviderAdapterContext,
    store: &Store,
    sink: &dyn ProOutputSink,
) {
    if let Err(error) = replay_source_outputs(path, source_root, context, store, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "trae_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

pub(super) fn replay_source_outputs(
    path: &Path,
    source_root: &Path,
    context: &ProviderAdapterContext,
    store: &Store,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let (authority, conn) = acquire_source(path, source_root, context.imported_at)?;
    let stored = load_source_cursor(store, &context.machine_id, &authority.cursor_stream)?;
    let StoredTraeCursor::Native { cursor, .. } = stored else {
        return Err(CaptureError::InvalidPayload(
            "Trae output replay requires committed NativePath Core".into(),
        ));
    };
    if !cursor.terminal
        || cursor.source_revision != authority.source_revision
        || cursor.canonical_source_identity != authority.proposed_source_identity
    {
        return Err(CaptureError::InvalidPayload(
            "Trae output replay source does not match committed Core authority".into(),
        ));
    }
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Trae.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: authority.proposed_source_identity.clone(),
    };
    let progress = sink.observe_source(&output_source).map_err(|error| {
        CaptureError::InvalidPayload(format!("Trae output sink observation failed: {error}"))
    })?;
    let (start, mut state) = TraeOutputState::new(
        output_source,
        progress,
        &authority,
        sink.materializer_revision(),
    )?;
    if state.terminal_noop {
        return Ok(());
    }
    let mut scanner = TraeScanner::new(&conn, &authority, start);
    while let Some(page) = scanner.next_page(false, true)? {
        if !authority.snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_frontier = output_frontier(page.expected)?;
        let next_frontier = output_frontier(page.next)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|row| output_observation(&authority, row))
            .collect::<Result<Vec<_>>>()?;
        let accounting = NativePageAccounting {
            logical_units: page.logical_units.max(1),
            conservative_serialized_bytes: page.estimated_bytes.max(1),
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: authority.source_revision.clone(),
            parser_revision: TRAE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::Trae.as_str(),
                &authority.proposed_source_identity,
            ),
            expected_frontier,
            next_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if let Err(error) = process_pro_replay_only(replay, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "trae_nativepath_output_page",
                format!("{:?}", error.output_error),
            ));
            break;
        }
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next_frontier);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

struct TraeOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    terminal_noop: bool,
}

impl TraeOutputState {
    pub(super) fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        authority: &TraeSourceAuthority,
        materializer_revision: &str,
    ) -> Result<(TraeFrontier, Self)> {
        let Some(progress) = progress else {
            return Ok((
                TraeFrontier::default(),
                Self {
                    source,
                    source_epoch: 0,
                    expected_source_epoch: None,
                    expected_sink_frontier: None,
                    disposition: ProOutputSourceDisposition::NewSource,
                    terminal_noop: false,
                },
            ));
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let decoded = progress
            .cursor
            .as_ref()
            .filter(|cursor| cursor.version == TRAE_OUTPUT_FRONTIER_VERSION)
            .and_then(|cursor| serde_json::from_slice::<TraeFrontier>(&cursor.payload).ok());
        let can_resume = progress.parser_revision == TRAE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == authority.source_revision
            && decoded.is_some();
        let terminal_noop =
            can_resume && progress.terminal && decoded.is_some_and(TraeFrontier::is_terminal);
        let rewrite = !can_resume;
        let source_epoch = if rewrite {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae output source epoch exhausted",
                ))?
        } else {
            progress.source_epoch
        };
        Ok((
            if can_resume {
                decoded.unwrap_or_default()
            } else {
                TraeFrontier::default()
            },
            Self {
                source,
                source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: prior_frontier,
                disposition: if rewrite {
                    ProOutputSourceDisposition::Rewrite
                } else {
                    ProOutputSourceDisposition::AppendOrResume
                },
                terminal_noop,
            },
        ))
    }
}

pub(super) fn output_frontier(frontier: TraeFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(TRAE_OUTPUT_FRONTIER_VERSION, serde_json::to_vec(&frontier)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn output_observation(
    authority: &TraeSourceAuthority,
    row: TraeOutputRow,
) -> Result<ProOutputObservation> {
    let native_sequence = packed_native_index(row.key_index, row.session_index, row.message_index)?;
    Ok(ProOutputObservation {
        kind: row
            .command
            .as_ref()
            .map_or(OutputObservationKind::Tool, |_| {
                OutputObservationKind::Command
            }),
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "{}:{}:{}",
                row.key_index, row.session_index, row.message_index
            ),
            native_sequence,
            native_record_id: Some(row.native_message_id),
            source_record_ordinal: Some(u64::from(row.key_index)),
            source_record_subrecord_index: Some(row.message_index),
            byte_start: Some(u64::try_from(row.byte_range.start).unwrap_or(u64::MAX)),
            byte_end_exclusive: Some(u64::try_from(row.byte_range.end).unwrap_or(u64::MAX)),
        },
        occurred_at_unix_ms: Some(row.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: row.provider_session_id.clone(),
            root_session_id: row.provider_session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(row.provider_session_id),
            agent_id: None,
            repository: None,
        },
        call_id: row.call_id,
        command: row.command,
        outcome: row.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "trae-itemtable-message-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": authority.path,
                "source_revision": authority.source_revision,
                "key_index": row.key_index,
                "session_index": row.session_index,
                "message_index": row.message_index,
                "byte_start": row.byte_range.start,
                "byte_end_exclusive": row.byte_range.end,
            }))?,
        },
        content: row.content,
    })
}

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &RovoDevSessionSource,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_identity: &str,
    source_revision: &str,
    locator_identity: &str,
    source_id: Uuid,
    stream: &str,
    document: &PreparedDocument,
    summary: &mut ProviderImportSummary,
) -> Result<Option<ResolvedSource>> {
    let raw_source_path = source.context_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        Some(source_identity),
        &json!({"native_source_id": source_identity}),
    )
    .ok_or(CaptureError::SystemInvariant(
        "RovoDev NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::RovoDev,
            source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    group.upsert_capture_source(&capture_source(
        source_id,
        source,
        context,
        configured_source_root,
        source_revision,
        &resolution.canonical_source_identity,
        document,
    ))?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session = canonical_session(
        committed_store,
        source_id,
        &resolution.canonical_source_identity,
        context,
        options,
        document,
    )?;
    if let Some(parent_id) = session.parent_session_id {
        if committed_store.get_session(parent_id).is_err() {
            group.insert_session_if_absent(&relationship_placeholder(
                parent_id,
                source_id,
                context,
                options,
                document
                    .parent_provider_session_id
                    .as_deref()
                    .unwrap_or_default(),
            ))?;
        }
    }
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = session.parent_session_id {
        let actor = canonical_actor(&session);
        group.upsert_projection_neutral_session_edge(
            &actor,
            &relationship_edge(
                source_id,
                &resolution.canonical_source_identity,
                context,
                &session,
                parent_id,
            ),
        )?;
        summary.imported_edges = summary.imported_edges.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(Some(ResolvedSource { source_id, session }))
}

pub(super) fn classify_cursor(
    stored: Option<&SyncCursor>,
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    document: Option<&PreparedDocument>,
) -> Result<CursorPlan> {
    let Some(stored) = stored else {
        return Ok(CursorPlan::Publish {
            expected: None,
            prior: None,
            generation: 0,
            start: 0,
            replacement: false,
        });
    };
    let committed = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => committed,
        Err(_) => {
            // Released pre-NativePath cursors are decode-only migration input.
            // No new cursor is ever emitted in that format.
            if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "RovoDev cursor is neither NativePath nor a released migration cursor"
                        .to_owned(),
                ));
            }
            return Ok(CursorPlan::Publish {
                expected: Some(stored.cursor.clone()),
                prior: None,
                generation: 0,
                start: 0,
                replacement: false,
            });
        }
    };
    let prior = RovoDevNativeCursor::decode(committed.provider_cursor())?;
    if prior.source_identity != source_identity {
        return Err(CaptureError::InvalidPayload(
            "RovoDev NativePath cursor belongs to another source".to_owned(),
        ));
    }
    if prior.source_revision == source_revision && !prior.missing {
        if prior.terminal {
            return Ok(CursorPlan::AlreadyCommitted(prior));
        }
        return Ok(CursorPlan::Publish {
            expected: Some(stored.cursor.clone()),
            start: usize::try_from(prior.frontier.next_message_index).map_err(|_| {
                CaptureError::InvalidPayload("RovoDev cursor frontier exceeds usize".to_owned())
            })?,
            generation: prior.generation,
            prior: Some(prior),
            replacement: false,
        });
    }

    let append = prior.physical_identity == physical_identity
        && document.is_some_and(|document| {
            usize::try_from(prior.frontier.next_message_index)
                .ok()
                .filter(|count| *count <= document.messages.len())
                .is_some_and(|count| {
                    frontier(&document.messages, count)
                        .ok()
                        .is_some_and(|frontier| {
                            frontier.prefix_sha256 == prior.frontier.prefix_sha256
                        })
                })
        })
        && !prior.missing;
    if append {
        let start = usize::try_from(prior.frontier.next_message_index).map_err(|_| {
            CaptureError::InvalidPayload("RovoDev cursor frontier exceeds usize".to_owned())
        })?;
        Ok(CursorPlan::Publish {
            expected: Some(stored.cursor.clone()),
            generation: prior.generation,
            prior: Some(prior),
            start,
            replacement: false,
        })
    } else {
        let generation = prior.generation.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("RovoDev source generation is exhausted".to_owned())
        })?;
        Ok(CursorPlan::Publish {
            expected: Some(stored.cursor.clone()),
            prior: Some(prior),
            generation,
            start: 0,
            replacement: true,
        })
    }
}

// Cursor construction keeps all persisted identity components explicit.
#[allow(clippy::too_many_arguments)]
pub(super) fn next_cursor(
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    locator_identity: &str,
    source_id: Uuid,
    prior: Option<&RovoDevNativeCursor>,
    generation: u64,
    page: &PreparedPage,
    document: &PreparedDocument,
) -> Result<RovoDevNativeCursor> {
    let same_generation = prior.filter(|prior| prior.generation == generation && !prior.missing);
    let accepted_events = page
        .messages
        .iter()
        .filter(|message| message.event.is_some())
        .count();
    let accepted_file_touches = page
        .messages
        .iter()
        .map(|message| message.touches.len())
        .sum::<usize>();
    let page_rejections = page
        .messages
        .iter()
        .filter_map(|message| message.rejection.clone())
        .collect::<Vec<_>>();
    let mut failures = same_generation
        .map(|prior| prior.failures.clone())
        .unwrap_or_default();
    if page.expected_frontier.next_message_index == 0 {
        failures.extend(document.initial_failures.iter().cloned());
    }
    failures.extend(page_rejections.iter().cloned());
    failures.truncate(ROVODEV_MAX_FAILURES);
    Ok(RovoDevNativeCursor {
        version: ROVODEV_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::RovoDev.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: source_revision.to_owned(),
        physical_identity: physical_identity.to_owned(),
        locator_identity: locator_identity.to_owned(),
        source_id: Some(source_id),
        frontier: page.next_frontier.clone(),
        terminal: page.terminal,
        missing: false,
        generation,
        accepted_sessions: 1,
        accepted_events: same_generation
            .map_or(0, |prior| prior.accepted_events)
            .saturating_add(u64::try_from(accepted_events).unwrap_or(u64::MAX)),
        accepted_file_touches: same_generation
            .map_or(0, |prior| prior.accepted_file_touches)
            .saturating_add(u64::try_from(accepted_file_touches).unwrap_or(u64::MAX)),
        rejected_records: same_generation
            .map_or(0, |prior| prior.rejected_records)
            .saturating_add(u64::try_from(page_rejections.len()).unwrap_or(u64::MAX))
            .saturating_add(if page.expected_frontier.next_message_index == 0 {
                u64::try_from(document.initial_failures.len()).unwrap_or(u64::MAX)
            } else {
                0
            }),
        failures,
    })
}

pub(super) fn replay_cursor_summary(
    cursor: &RovoDevNativeCursor,
    summary: &mut ProviderImportSummary,
) {
    summary.skipped_sessions = usize::try_from(cursor.accepted_sessions).unwrap_or(usize::MAX);
    summary.skipped_events = usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events);
    for failure in &cursor.failures {
        summary.record_failure(ProviderImportFailure {
            line: failure.line,
            error: failure.error.clone(),
        });
    }
    let rejected = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
    summary.failed = summary.failed.max(rejected);
    summary.set_work_result(ProviderImportWorkResult::NoOp);
}

pub(super) fn replay_outputs(
    source: &RovoDevSessionSource,
    document: &PreparedDocument,
    source_identity: &str,
    cursor: &RovoDevNativeCursor,
    sink: &dyn ProOutputSink,
) -> std::result::Result<(), ProOutputSinkError> {
    let source_revision = &cursor.source_revision;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::RovoDev.as_str().to_owned(),
        namespace_id: source_identity.to_owned(),
        source_id: document.provider_session_id.clone(),
    };
    let progress = sink.observe_source(&output_source)?;
    let mut state = output_state(
        output_source,
        progress,
        source_revision,
        sink.materializer_revision(),
        document,
        cursor.generation,
        &cursor.physical_identity,
    )?;
    let mut index = state.source_start;
    if index > document.messages.len() {
        return Err(ProOutputSinkError::new(
            "rovodev_output_cursor",
            "frontier exceeds current source",
        ));
    }
    while index < document.messages.len() || state.requires_checkpoint {
        let end = index
            .saturating_add(ROVODEV_PAGE_MAX_UNITS)
            .min(document.messages.len());
        let expected = frontier(&document.messages, index).map_err(|error| {
            ProOutputSinkError::new("rovodev_output_frontier", error.to_string())
        })?;
        let next = frontier(&document.messages, end).map_err(|error| {
            ProOutputSinkError::new("rovodev_output_frontier", error.to_string())
        })?;
        let mut observations = Vec::new();
        for message_index in index..end {
            let message = &document.messages[message_index];
            let role = message
                .get("role")
                .or_else(|| message.get("kind"))
                .or_else(|| message.get("type"))
                .and_then(Value::as_str);
            if rovodev_event_type(message, role) != EventType::ToolOutput {
                continue;
            }
            observations.push(output_observation(
                source,
                document,
                source_identity,
                source_revision,
                message,
                message_index,
            )?);
        }
        let terminal = end == document.messages.len();
        let expected_safe =
            output_safe_frontier(&expected, cursor.generation, &cursor.physical_identity).map_err(
                |error| ProOutputSinkError::new("rovodev_output_frontier", error.to_string()),
            )?;
        let next_safe = output_safe_frontier(&next, cursor.generation, &cursor.physical_identity)
            .map_err(|error| {
            ProOutputSinkError::new("rovodev_output_frontier", error.to_string())
        })?;
        let accounting = NativePageAccounting {
            logical_units: observations.len(),
            conservative_serialized_bytes: observations
                .iter()
                .map(|output| estimated_output_bytes(output).saturating_add(4 * 1024))
                .sum::<usize>()
                .saturating_add(4 * 1024),
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: source_revision.to_owned(),
            parser_revision: ROVODEV_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_frontier.clone(),
            observations,
        };
        let page = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::RovoDev.as_str(), source_identity),
            expected_safe,
            next_safe.clone(),
            terminal,
            accounting,
            output,
        )
        .map_err(|error| ProOutputSinkError::new("rovodev_output_page", error.to_string()))?;
        if let Err(failure) = process_pro_replay_only(page, sink) {
            return Err(match failure.output_error {
                NativeOutputProFailure::Sink(error) => error,
                NativeOutputProFailure::ReceiptMismatch { .. } => ProOutputSinkError::new(
                    "rovodev_output_receipt",
                    "output sink acknowledgement did not match the requested cursor",
                ),
            });
        }
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_frontier = Some(next_safe);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        state.requires_checkpoint = false;
        if terminal {
            break;
        }
        index = end;
    }
    Ok(())
}

pub(super) fn output_state(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    source_revision: &str,
    materializer_revision: &str,
    document: &PreparedDocument,
    generation: u64,
    physical_identity: &str,
) -> std::result::Result<OutputState, ProOutputSinkError> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            source_start: 0,
            disposition: ProOutputSourceDisposition::NewSource,
            requires_checkpoint: true,
        });
    };
    let prior_safe_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| {
            NativeSafeFrontier::new(cursor.version, cursor.payload.clone()).map_err(|error| {
                ProOutputSinkError::new("rovodev_output_cursor", error.to_string())
            })
        })
        .transpose()?;
    let prior_frontier = prior_safe_frontier
        .as_ref()
        .map(output_frontier)
        .transpose()?;
    let append = prior_frontier.as_ref().is_some_and(|prior| {
        prior.generation == generation
            && prior.physical_identity == physical_identity
            && usize::try_from(prior.next_message_index)
                .ok()
                .filter(|count| *count <= document.messages.len())
                .is_some_and(|count| {
                    frontier(&document.messages, count)
                        .ok()
                        .is_some_and(|current| current.prefix_sha256 == prior.prefix_sha256)
                })
    });
    let rewrite = progress.parser_revision != ROVODEV_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != materializer_revision
        || !append;
    let requires_checkpoint = rewrite || progress.observed_revision != source_revision;
    let source_start = if rewrite {
        0
    } else {
        prior_frontier.as_ref().map_or(0, |frontier| {
            usize::try_from(frontier.next_message_index).unwrap_or(usize::MAX)
        })
    };
    Ok(OutputState {
        source,
        source_epoch: if rewrite {
            progress.source_epoch.checked_add(1).ok_or_else(|| {
                ProOutputSinkError::new("rovodev_output_epoch", "source epoch is exhausted")
            })?
        } else {
            progress.source_epoch
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier: prior_safe_frontier,
        source_start,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
        requires_checkpoint,
    })
}

pub(super) fn output_observation(
    _source: &RovoDevSessionSource,
    document: &PreparedDocument,
    source_identity: &str,
    source_revision: &str,
    message: &Value,
    index: usize,
) -> std::result::Result<ProOutputObservation, ProOutputSinkError> {
    let event_index = u64::try_from(index)
        .map_err(|_| ProOutputSinkError::new("rovodev_output_index", "index exceeds u64"))?;
    let metadata = output_metadata(message, event_index, document.cwd.as_deref());
    let content = super::rovodev_result_content(message).unwrap_or_default();
    let locator_payload = serde_json::to_vec(&json!({
        "source_identity": source_identity,
        "source_revision": source_revision,
        "message_index": event_index,
    }))
    .map_err(|error| ProOutputSinkError::new("rovodev_output_locator", error.to_string()))?;
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    Ok(ProOutputObservation {
        kind: metadata.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: metadata.native_record_id.clone(),
            native_sequence: event_index,
            native_record_id: Some(metadata.native_record_id),
            source_record_ordinal: Some(0),
            source_record_subrecord_index: u32::try_from(index).ok(),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: document.provider_session_id.clone(),
            root_session_id: document
                .parent_provider_session_id
                .clone()
                .unwrap_or_else(|| document.provider_session_id.clone()),
            parent_session_id: document.parent_provider_session_id.clone(),
            provider_session_id: Some(document.provider_session_id.clone()),
            agent_id: provider_string_field(&document.metadata, &["agent_id", "agentId"]),
            repository: None,
        },
        call_id: metadata.call_id,
        command: metadata.command,
        outcome: metadata.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: ROVODEV_NATIVE_LOCATOR_KIND.to_owned(),
            payload: locator_payload,
        },
        content: content.into_bytes(),
    })
}

pub(super) fn estimated_output_bytes(output: &ProOutputObservation) -> usize {
    output
        .content
        .len()
        .saturating_add(output.locator.payload.len())
        .saturating_add(output.coordinate.unit_key.len())
        .saturating_add(512)
}

pub(super) fn retire_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    root_stream: &str,
    manifest: &RovoDevRootManifest,
    entry: &RovoDevManifestEntry,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<(RovoDevRootManifest, usize, ProviderImportSummary)> {
    let source_cursor = store
        .get_sync_cursor(None, &context.machine_id, &entry.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "RovoDev root manifest references a missing source cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&source_cursor.cursor)?;
    let prior = RovoDevNativeCursor::decode(committed.provider_cursor())?;
    let cursor_canonical = prior
        .source_id
        .map(|source_id| {
            store
                .get_capture_source(source_id)
                .map(|source| source.descriptor.source_identity)
        })
        .transpose()?
        .flatten();
    if prior.locator_identity != entry.locator_identity
        || prior.source_revision != entry.source_revision
        || entry.canonical_source_identity.is_some()
            && entry.canonical_source_identity != cursor_canonical
    {
        return Err(CaptureError::InvalidPayload(
            "RovoDev root/source cursor authority diverged".to_owned(),
        ));
    }
    let mut next_manifest = manifest.clone();
    next_manifest
        .sources
        .retain(|source| source.source_identity != entry.source_identity);
    let root_stored = store.get_sync_cursor(None, &context.machine_id, root_stream)?;
    let root_next = sync_cursor(
        context,
        root_stream,
        serde_json::to_string(&next_manifest)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        CaptureProvider::RovoDev,
    );
    let root_transition = NativePathCursorTransition::new(
        root_stored.as_ref().map(|cursor| cursor.cursor.clone()),
        root_next,
    );
    let missing_cursor = RovoDevNativeCursor {
        source_revision: prior.source_revision.clone(),
        terminal: true,
        missing: true,
        generation: prior.generation.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("RovoDev source generation is exhausted".to_owned())
        })?,
        ..prior.clone()
    };
    let source_transition = NativePathCursorTransition::new(
        Some(source_cursor.cursor),
        sync_cursor(
            context,
            &entry.cursor_stream,
            missing_cursor.encode()?,
            CaptureProvider::RovoDev,
        ),
    );
    let transitions = vec![source_transition, root_transition];
    let publication_id = retirement_publication_id(entry, &transitions);
    let retained_bytes = transitions
        .iter()
        .map(|transition| transition.next().cursor.len())
        .sum();
    let accounting = NativePathGroupAccounting::new(0, 2, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllExpected
    ) {
        if let Some(canonical) = entry
            .canonical_source_identity
            .as_deref()
            .or(cursor_canonical.as_deref())
        {
            let disposition =
                group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                    provider: CaptureProvider::RovoDev,
                    source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    locator_identity: entry.locator_identity.clone(),
                    cursor_stream: entry.cursor_stream.clone(),
                    expected_canonical_source_identity: canonical.to_owned(),
                    expected_source_revision: entry.source_revision.clone(),
                    retired_at_ms: context.imported_at.timestamp_millis(),
                    reason,
                })?;
            if disposition == ProviderSourceRouteRetirementDisposition::AlreadyRetired {
                return Err(CaptureError::InvalidPayload(
                    "RovoDev live root manifest retained an already-retired route".to_owned(),
                ));
            }
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok((next_manifest, 1, summary))
}

pub(super) fn source_id_for_generation(
    source: &RovoDevSessionSource,
    source_identity: &str,
    locator_identity: &str,
    provider_session_id: &str,
    generation: u64,
) -> Uuid {
    if generation == 0 {
        let raw_source_path = source.context_path.display().to_string();
        return provider_scoped_source_uuid(
            CaptureProvider::RovoDev,
            provider_session_id,
            ROVODEV_SOURCE_FORMAT,
            Some(&raw_source_path),
        );
    }
    stable_capture_uuid(
        &format!(
            "rovodev-native-source:{source_identity}:{locator_identity}:{provider_session_id}"
        ),
        "source",
    )
}

pub(super) fn sync_cursor(
    context: &ProviderAdapterContext,
    stream: &str,
    cursor: String,
    provider: CaptureProvider,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                provider.as_str(),
                context.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    }
}

pub(super) fn frontier(messages: &[Value], count: usize) -> Result<RovoDevFrontier> {
    if count > messages.len() {
        return Err(CaptureError::InvalidPayload(
            "RovoDev frontier exceeds the message history".to_owned(),
        ));
    }
    Ok(RovoDevFrontier {
        version: ROVODEV_NATIVE_FRONTIER_VERSION,
        next_message_index: u64::try_from(count).map_err(|_| {
            CaptureError::InvalidPayload("RovoDev message count exceeds u64".to_owned())
        })?,
        prefix_sha256: prefix_sha256(&messages[..count]),
    })
}

pub(super) fn output_safe_frontier(
    frontier: &RovoDevFrontier,
    generation: u64,
    physical_identity: &str,
) -> std::result::Result<NativeSafeFrontier, NativeIngestionPageError> {
    let output_frontier = RovoDevOutputFrontier {
        version: ROVODEV_NATIVE_FRONTIER_VERSION,
        generation,
        physical_identity: physical_identity.to_owned(),
        next_message_index: frontier.next_message_index,
        prefix_sha256: frontier.prefix_sha256,
    };
    let bytes = serde_json::to_vec(&output_frontier)
        .map_err(|_| NativeIngestionPageError::FrontierTooLarge { bytes: usize::MAX })?;
    NativeSafeFrontier::new(ROVODEV_NATIVE_FRONTIER_VERSION, bytes)
}

pub(super) fn output_frontier(
    frontier: &NativeSafeFrontier,
) -> std::result::Result<RovoDevOutputFrontier, ProOutputSinkError> {
    if frontier.version != ROVODEV_NATIVE_FRONTIER_VERSION {
        return Err(ProOutputSinkError::new(
            "rovodev_output_cursor",
            "unsupported frontier version",
        ));
    }
    let decoded: RovoDevOutputFrontier = serde_json::from_slice(&frontier.bytes)
        .map_err(|error| ProOutputSinkError::new("rovodev_output_cursor", error.to_string()))?;
    if decoded.version != ROVODEV_NATIVE_FRONTIER_VERSION || decoded.physical_identity.is_empty() {
        return Err(ProOutputSinkError::new(
            "rovodev_output_cursor",
            "inconsistent frontier version",
        ));
    }
    Ok(decoded)
}

pub(super) fn prefix_sha256(messages: &[Value]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_PREFIX_DOMAIN);
    for message in messages {
        let bytes = serde_json::to_vec(message).unwrap_or_default();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest.finalize().into()
}

pub(super) fn publication_id(
    source_identity: &str,
    source_revision: &str,
    page: &PreparedPage,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_PUBLICATION_DOMAIN);
    digest.update(source_identity.as_bytes());
    digest.update(source_revision.as_bytes());
    digest.update(page.expected_frontier.prefix_sha256);
    digest.update(page.next_frontier.prefix_sha256);
    digest.update([u8::from(page.terminal)]);
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("rovodev-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn rejection_publication_id(
    source_identity: &str,
    source_revision: &str,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_PUBLICATION_DOMAIN);
    digest.update(b"rejection\0");
    digest.update(source_identity.as_bytes());
    digest.update(source_revision.as_bytes());
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("rovodev-nativepath-rejection-v1:{:x}", digest.finalize())
}

pub(super) fn root_publication_id(
    manifest: &RovoDevRootManifest,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_ROOT_PUBLICATION_DOMAIN);
    digest.update(manifest.root_identity.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("rovodev-nativepath-root-v1:{:x}", digest.finalize())
}

pub(super) fn retirement_publication_id(
    entry: &RovoDevManifestEntry,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_RETIREMENT_PUBLICATION_DOMAIN);
    digest.update(entry.source_identity.as_bytes());
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("rovodev-nativepath-retire-v1:{:x}", digest.finalize())
}

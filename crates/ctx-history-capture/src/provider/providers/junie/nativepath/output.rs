use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JunieOutputCursor {
    pub(super) version: u32,
    pub(super) provider: String,
    pub(super) source_identity: String,
    pub(super) source_revision: String,
    pub(super) observed_length: u64,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
    pub(super) generation: u64,
    pub(super) terminal: bool,
    pub(super) frontier: Frontier,
}

impl JunieOutputCursor {
    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie output replay cursor exceeds its provider-local bound".to_owned(),
            ));
        }
        Ok(encoded)
    }

    pub(super) fn decode(encoded: &[u8]) -> Result<Self> {
        let cursor: Self = serde_json::from_slice(encoded)?;
        if cursor.version != OUTPUT_FRONTIER_VERSION
            || cursor.provider != CaptureProvider::Junie.as_str()
            || cursor.source_identity.is_empty()
            || cursor.frontier.offset > cursor.observed_length
            || (cursor.terminal
                && (cursor.frontier.pending.is_some()
                    || cursor.frontier.offset != cursor.observed_length))
            || cursor.frontier.pending.as_ref().is_some_and(|pending| {
                pending.start_offset != cursor.frontier.offset
                    || pending.start_ordinal != cursor.frontier.next_ordinal
                    || pending.base_event_index != cursor.frontier.next_event_index
                    || pending.next_event_index < pending.base_event_index
                    || pending.start_offset >= pending.end_offset
                    || pending.next_row > pending.row_count
            })
        {
            return Err(CaptureError::InvalidPayload(
                "Junie output replay cursor is malformed or inconsistent".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

pub(super) struct OutputReplayState {
    pub(super) source: OutputSourceIdentity,
    pub(super) source_epoch: u64,
    pub(super) expected_source_epoch: Option<u64>,
    pub(super) expected_sink_frontier: Option<NativeSafeFrontier>,
    pub(super) disposition: ProOutputSourceDisposition,
    pub(super) cursor: JunieOutputCursor,
}

pub(super) fn replay_outputs(
    store: &Store,
    sessions: &[JunieSessionPath],
    source_root: &Path,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) {
    let Some(sink) = profile.sink().map(std::sync::Arc::as_ref) else {
        return;
    };
    for session in sessions {
        if let Err(error) = replay_output_source(store, session, source_root, context, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "junie_nativepath_output_replay",
                error.to_string(),
            ));
        }
    }
}

pub(super) fn replay_output_source(
    store: &Store,
    session_path: &JunieSessionPath,
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let observation = JunieSessionObservation::read(session_path)?;
    let provider_session_id = junie_provider_session_id(session_path)?;
    let locator_identity = provider_path_identity(&session_path.events_path)?;
    let canonical_identity = provider_path_identity(&observation.canonical_path)?;
    let source_identity = format!("junie-session-events:{canonical_identity}");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Ok(());
    };
    let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) else {
        return Ok(());
    };
    let core_cursor = JunieStoreCursor::decode(committed.provider_cursor())?;
    if core_cursor.source_identity != source_identity
        || core_cursor.retired
        || !core_cursor.terminal
        || core_cursor.frontier.pending.is_some()
        || core_cursor.source_revision != observation.source_revision()
        || core_cursor.observed_length != observation.events_file.length
        || core_cursor.device != observation.events_file.device
        || core_cursor.inode != observation.events_file.inode
        || core_cursor.frontier.offset != observation.events_file.length
        || hash_prefix(&session_path.events_path, core_cursor.frontier.offset)?
            != core_cursor.frontier.prefix_sha256
    {
        return Ok(());
    }

    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Junie.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: locator_identity.clone(),
    };
    let progress = sink
        .observe_source(&output_source)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let mut state = output_replay_state(
        session_path,
        &observation,
        &source_identity,
        context.imported_at,
        sink,
        output_source,
        progress,
    )?;
    if state.cursor.terminal
        && state.cursor.source_revision == observation.source_revision()
        && state.cursor.observed_length == observation.events_file.length
    {
        return Ok(());
    }

    loop {
        if !observation.revalidate(session_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let parsed = parse_turn(&session_path.events_path, &state.cursor.frontier)?;
        validate_output_pending_replay(&state.cursor.frontier, &parsed)?;
        if parsed.incomplete {
            return Ok(());
        }
        let pending_start = state
            .cursor
            .frontier
            .pending
            .as_ref()
            .map_or(0_usize, |pending| pending.next_row as usize);
        if pending_start > parsed.outputs.len() {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut output_start = pending_start;
        loop {
            let output_end = output_page_end(&parsed.outputs, output_start)?;
            let mut next = state.cursor.clone();
            next.source_revision = observation.source_revision();
            next.observed_length = observation.events_file.length;
            next.device = observation.events_file.device;
            next.inode = observation.events_file.inode;
            if output_end < parsed.outputs.len() {
                next.terminal = false;
                next.frontier.pending = Some(PendingTurn {
                    start_offset: parsed.start_offset,
                    end_offset: parsed.end_offset,
                    start_ordinal: parsed.start_ordinal,
                    end_ordinal: parsed.end_ordinal,
                    base_event_index: parsed.base_event_index,
                    next_event_index: parsed.next_event_index,
                    next_row: u32::try_from(output_end).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Junie output turn count exceeds u32".to_owned(),
                        )
                    })?,
                    row_count: u32::try_from(parsed.outputs.len()).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Junie output turn count exceeds u32".to_owned(),
                        )
                    })?,
                    turn_sha256: parsed.turn_sha256,
                    terminal: parsed.terminal,
                    after_state: parsed.after_state.clone(),
                    after_prefix_sha256: parsed.after_prefix_sha256,
                });
            } else {
                next.frontier = Frontier {
                    offset: parsed.end_offset,
                    next_ordinal: parsed.end_ordinal,
                    next_event_index: parsed.next_event_index,
                    prefix_sha256: parsed.after_prefix_sha256,
                    state: parsed.after_state.clone(),
                    pending: None,
                };
                next.terminal =
                    parsed.terminal && parsed.end_offset == observation.events_file.length;
            }
            if !observation.revalidate(session_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if !publish_output_page(
                sink,
                session_path,
                &provider_session_id,
                &locator_identity,
                &observation,
                &mut state,
                next,
                &parsed.outputs[output_start..output_end],
                &parsed.after_state,
            )? {
                return Ok(());
            }
            output_start = output_end;
            if output_start >= parsed.outputs.len() {
                break;
            }
        }
        if state.cursor.frontier.pending.is_some() {
            continue;
        }
        if state.cursor.terminal {
            break;
        }
    }
    Ok(())
}

pub(super) fn output_replay_state(
    session_path: &JunieSessionPath,
    observation: &JunieSessionObservation,
    source_identity: &str,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
) -> Result<OutputReplayState> {
    let fresh = |generation| JunieOutputCursor {
        version: OUTPUT_FRONTIER_VERSION,
        provider: CaptureProvider::Junie.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: observation.source_revision(),
        observed_length: observation.events_file.length,
        device: observation.events_file.device,
        inode: observation.events_file.inode,
        generation,
        terminal: false,
        frontier: Frontier {
            offset: 0,
            next_ordinal: 0,
            next_event_index: 0,
            prefix_sha256: Sha256::digest([]).into(),
            state: RuntimeState::fresh(
                &bounded_junie_index_meta(&session_path.index_meta),
                imported_at,
            ),
            pending: None,
        },
    };
    let Some(progress) = progress else {
        return Ok(OutputReplayState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            cursor: fresh(0),
        });
    };
    let prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let candidate = progress
        .cursor
        .as_ref()
        .filter(|cursor| cursor.version == OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| JunieOutputCursor::decode(&cursor.payload).ok());
    let can_resume = progress.parser_revision == OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && candidate.as_ref().is_some_and(|cursor| {
            let (prefix_boundary, expected_prefix) = cursor.frontier.pending.as_ref().map_or(
                (cursor.frontier.offset, cursor.frontier.prefix_sha256),
                |pending| (pending.end_offset, pending.after_prefix_sha256),
            );
            cursor.source_identity == source_identity
                && cursor.device == observation.events_file.device
                && cursor.inode == observation.events_file.inode
                && observation.events_file.length >= prefix_boundary
                && hash_prefix(&session_path.events_path, prefix_boundary)
                    .is_ok_and(|digest| digest == expected_prefix)
        });
    let rewrite = !can_resume;
    let source_epoch = if rewrite {
        progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie output source epoch exhausted",
            ))?
    } else {
        progress.source_epoch
    };
    Ok(OutputReplayState {
        source,
        source_epoch,
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: prior_frontier,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
        cursor: if rewrite {
            fresh(source_epoch)
        } else {
            candidate.ok_or(CaptureError::SystemInvariant(
                "Junie resumable output cursor disappeared",
            ))?
        },
    })
}

pub(super) fn validate_output_pending_replay(
    frontier: &Frontier,
    parsed: &ParsedTurn,
) -> Result<()> {
    let Some(pending) = &frontier.pending else {
        return Ok(());
    };
    if pending.start_offset != parsed.start_offset
        || pending.end_offset != parsed.end_offset
        || pending.start_ordinal != parsed.start_ordinal
        || pending.end_ordinal != parsed.end_ordinal
        || pending.base_event_index != parsed.base_event_index
        || pending.next_event_index != parsed.next_event_index
        || pending.row_count as usize != parsed.outputs.len()
        || pending.turn_sha256 != parsed.turn_sha256
        || pending.terminal != parsed.terminal
        || pending.after_state != parsed.after_state
        || pending.after_prefix_sha256 != parsed.after_prefix_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn output_page_end(outputs: &[OutputDraft], start: usize) -> Result<usize> {
    if start >= outputs.len() {
        return Ok(start);
    }
    let mut bytes = 0_usize;
    let mut end = start;
    while end < outputs.len() && end - start < OUTPUT_PAGE_MAX_ROWS {
        let output = &outputs[end];
        let next = output
            .content
            .len()
            .saturating_add(output.locator_payload.len())
            .saturating_add(output.call_id.len())
            .saturating_add(output.command.as_ref().map_or(0, String::len))
            .saturating_add(2 * 1024);
        if next > OUTPUT_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie transient output exceeds the bounded Pro page".to_owned(),
            ));
        }
        if end != start && bytes.saturating_add(next) > OUTPUT_PAGE_MAX_BYTES {
            break;
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    Ok(end)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_output_page(
    sink: &dyn ProOutputSink,
    session_path: &JunieSessionPath,
    provider_session_id: &str,
    locator_identity: &str,
    observation: &JunieSessionObservation,
    state: &mut OutputReplayState,
    next: JunieOutputCursor,
    outputs: &[OutputDraft],
    runtime: &RuntimeState,
) -> Result<bool> {
    let expected_frontier = output_safe_frontier(&state.cursor)?;
    let next_frontier = output_safe_frontier(&next)?;
    let observations = outputs
        .iter()
        .map(|output| output_observation(provider_session_id, output, runtime))
        .collect::<Vec<_>>();
    let claimed_bytes = observations.iter().fold(
        expected_frontier
            .bytes
            .len()
            .saturating_add(next_frontier.bytes.len())
            .saturating_add(locator_identity.len())
            .saturating_add(64 * 1024),
        |bytes, output| {
            bytes
                .saturating_add(output.content.len())
                .saturating_add(output.locator.payload.len())
                .saturating_add(2 * 1024)
        },
    );
    if claimed_bytes > crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Junie transient output page exceeds the NativePath byte bound".to_owned(),
        ));
    }
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: observation.source_revision(),
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Junie.as_str(), locator_identity),
        expected_frontier,
        next_frontier.clone(),
        next.terminal,
        NativePageAccounting {
            logical_units: outputs.len().max(1),
            conservative_serialized_bytes: claimed_bytes,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(replay, sink).is_err() {
        sink.mark_behind(ProOutputSinkError::new(
            "junie_nativepath_output_page",
            format!(
                "failed to materialize Junie output page for {}",
                session_path.events_path.display()
            ),
        ));
        return Ok(false);
    }
    state.cursor = next;
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

pub(super) fn output_safe_frontier(cursor: &JunieOutputCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, cursor.encode()?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn output_observation(
    provider_session_id: &str,
    output: &OutputDraft,
    runtime: &RuntimeState,
) -> ProOutputObservation {
    ProOutputObservation {
        kind: if output.command.is_some() {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: output.call_id.clone(),
            native_sequence: output.event_index,
            native_record_id: Some(output.native_record_id.clone()),
            source_record_ordinal: Some(output.source_ordinal),
            source_record_subrecord_index: Some(output.source_subrecord),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(output.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: provider_session_id.to_owned(),
            root_session_id: provider_session_id.to_owned(),
            parent_session_id: None,
            provider_session_id: Some(provider_session_id.to_owned()),
            agent_id: None,
            repository: None,
        },
        call_id: Some(output.call_id.clone()),
        command: output.command.as_ref().and_then(|command| {
            let command = provider_local_preview(command, PROVIDER_MAX_PREVIEW_CHARS).0;
            (!command.contains('\0')).then(|| OutputCommandContext {
                tool_name: output.tool_name.clone(),
                command,
                working_directory: runtime.cwd.as_deref().and_then(|cwd| {
                    (!cwd.is_empty()
                        && cwd.len() <= PROVIDER_MAX_PREVIEW_CHARS
                        && !cwd.chars().any(char::is_control))
                    .then(|| cwd.to_owned())
                }),
            })
        }),
        outcome: OutputOutcomeMetadata {
            outcome: output.outcome,
            exit_code: output.exit_code,
            duration_ms: output.duration_ms,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: RECORD_SET_KIND.to_owned(),
            payload: output.locator_payload.clone(),
        },
        content: output.content.clone(),
    }
}

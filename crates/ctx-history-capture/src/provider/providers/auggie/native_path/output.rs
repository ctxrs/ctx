use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    paths: &BTreeSet<PathBuf>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) {
    let Some(sink) = options.import_profile.sink().map(AsRef::as_ref) else {
        return;
    };
    for path in paths {
        let parsed = match parse_auggie_source(
            path,
            context,
            options.inventory_observation_token.as_deref(),
            true,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "auggie_nativepath_output_source",
                    error.to_string(),
                ));
                continue;
            }
        };
        if let Err(error) = verify_committed_core(store, context, &parsed) {
            sink.mark_behind(ProOutputSinkError::new(
                "auggie_nativepath_output_core",
                error.to_string(),
            ));
            continue;
        }
        replay_parsed_outputs_or_mark_behind(&parsed, configured_source_root, Some(sink));
    }
}

pub(super) fn replay_parsed_outputs_or_mark_behind(
    parsed: &ParsedAuggieSource,
    configured_source_root: &Path,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_parsed_outputs(parsed, configured_source_root, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "auggie_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn verify_committed_core(
    store: &Store,
    context: &ProviderAdapterContext,
    parsed: &ParsedAuggieSource,
) -> Result<()> {
    let locator_identity = provider_path_identity(&parsed.stamp.canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Auggie output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = decode_cursor(committed.provider_cursor())?;
    validate_native_cursor(&cursor, &parsed.stamp.canonical_path)?;
    if !cursor.terminal
        || cursor.source_revision != parsed.source_revision
        || cursor.provider_session_id != parsed.session.provider_session_id
    {
        return Err(CaptureError::InvalidPayload(
            "Auggie output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn replay_parsed_outputs(
    parsed: &ParsedAuggieSource,
    configured_source_root: &Path,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let locator_identity = provider_path_identity(&parsed.stamp.canonical_path)?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Auggie.as_str().to_owned(),
        namespace_id: configured_source_root.display().to_string(),
        source_id: locator_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let mut state = AuggieOutputState::new(
        output_source,
        progress,
        &parsed.source_revision,
        sink.materializer_revision(),
    )?;
    let mut next_output = state.next_output.min(parsed.outputs.len());
    if parsed.outputs.is_empty() {
        publish_output_page(parsed, sink, &locator_identity, &mut state, 0, 0, true)?;
        return Ok(());
    }
    while next_output < parsed.outputs.len() {
        let mut end = next_output;
        let mut content_bytes = 0_usize;
        while end < parsed.outputs.len()
            && end.saturating_sub(next_output) < AUGGIE_OUTPUTS_PER_PAGE
        {
            let next_bytes = content_bytes.saturating_add(parsed.outputs[end].content.len());
            if end != next_output && next_bytes > AUGGIE_OUTPUT_PAGE_CONTENT_BYTES {
                break;
            }
            if next_bytes > AUGGIE_OUTPUT_PAGE_CONTENT_BYTES {
                return Err(CaptureError::InvalidPayload(
                    "one Auggie output body exceeds the bounded Pro page".to_owned(),
                ));
            }
            content_bytes = next_bytes;
            end = end.saturating_add(1);
        }
        let terminal = end == parsed.outputs.len();
        if !publish_output_page(
            parsed,
            sink,
            &locator_identity,
            &mut state,
            next_output,
            end,
            terminal,
        )? {
            break;
        }
        next_output = end;
    }
    Ok(())
}

struct AuggieOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    next_output: usize,
}

impl AuggieOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        observed_revision: &str,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                next_output: 0,
            });
        };
        let decoded = progress
            .cursor
            .as_ref()
            .filter(|cursor| cursor.version == AUGGIE_OUTPUT_FRONTIER_VERSION)
            .and_then(|cursor| serde_json::from_slice::<AuggieOutputFrontier>(&cursor.payload).ok())
            .filter(|frontier| frontier.version == AUGGIE_OUTPUT_FRONTIER_VERSION);
        let can_resume = progress.parser_revision == AUGGIE_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == observed_revision
            && decoded
                .as_ref()
                .is_some_and(|frontier| frontier.source_revision == observed_revision);
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let source_epoch = if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Auggie output source epoch exhausted",
                ))?
        };
        Ok(Self {
            source,
            source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: if can_resume {
                ProOutputSourceDisposition::AppendOrResume
            } else {
                ProOutputSourceDisposition::Rewrite
            },
            next_output: if can_resume {
                decoded
                    .and_then(|frontier| usize::try_from(frontier.next_output).ok())
                    .unwrap_or(0)
            } else {
                0
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_output_page(
    parsed: &ParsedAuggieSource,
    sink: &dyn ProOutputSink,
    locator_identity: &str,
    state: &mut AuggieOutputState,
    start: usize,
    end: usize,
    terminal: bool,
) -> Result<bool> {
    let expected_frontier = output_frontier(&parsed.source_revision, start)?;
    let next_frontier = output_frontier(&parsed.source_revision, end)?;
    let observations = parsed.outputs[start..end]
        .iter()
        .map(|output| output_observation(parsed, output))
        .collect::<Result<Vec<_>>>()?;
    let content_bytes = parsed.outputs[start..end]
        .iter()
        .fold(0_usize, |total, output| {
            total.saturating_add(output.content.len())
        });
    let accounting = NativePageAccounting {
        logical_units: observations.len().max(1),
        conservative_serialized_bytes: content_bytes.saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES),
    };
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: parsed.source_revision.clone(),
        parser_revision: AUGGIE_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Auggie.as_str(), locator_identity),
        expected_frontier,
        next_frontier.clone(),
        terminal,
        accounting,
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(replay, sink).is_err() {
        return Ok(false);
    }
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

fn output_observation(
    parsed: &ParsedAuggieSource,
    output: &ParsedAuggieOutput,
) -> Result<ProOutputObservation> {
    let direct_session_id = parsed.session.provider_session_id.clone();
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "{}:{}:{}",
                output.chat_index, output.node_collection, output.node_index
            ),
            native_sequence: u64::from(output.output_sequence),
            native_record_id: output.call_id.clone(),
            source_record_ordinal: Some(u64::try_from(output.chat_index).unwrap_or(u64::MAX)),
            source_record_subrecord_index: Some(output.output_sequence),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: output.occurred_at.map(|time| time.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: parsed
                .session
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: parsed.session.parent_provider_session_id.clone(),
            provider_session_id: Some(direct_session_id),
            agent_id: parsed.session.external_agent_id.clone(),
            repository: None,
        },
        call_id: output.call_id.clone(),
        command: None,
        outcome: output.outcome.clone(),
        locator: OutputSourceLocator {
            version: 1,
            kind: "auggie-session-json-node-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": parsed.stamp.canonical_path,
                "chat_index": output.chat_index,
                "node_collection": output.node_collection,
                "node_index": output.node_index,
                "content_sha256": output.content_sha256,
            }))?,
        },
        content: output.content.clone(),
    })
}

fn output_frontier(source_revision: &str, next_output: usize) -> Result<NativeSafeFrontier> {
    let frontier = AuggieOutputFrontier {
        version: AUGGIE_OUTPUT_FRONTIER_VERSION,
        source_revision: source_revision.to_owned(),
        next_output: u64::try_from(next_output).map_err(|_| {
            CaptureError::InvalidPayload("Auggie output frontier exceeds u64".to_owned())
        })?,
    };
    NativeSafeFrontier::new(
        AUGGIE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

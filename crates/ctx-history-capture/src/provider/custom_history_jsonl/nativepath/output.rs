use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    parsed: &ParsedCustomHistory,
    outputs: &[CustomOutput],
) {
    let Some(sink) = options.import_profile.sink().map(AsRef::as_ref) else {
        return;
    };
    if let Err(error) = verify_committed_core(
        store,
        context,
        logical_locator,
        stream,
        &parsed.source_revision,
    ) {
        sink.mark_behind(ProOutputSinkError::new(
            "custom_history_nativepath_output_core",
            error.to_string(),
        ));
        return;
    }
    if let Err(error) = replay_outputs(sink, logical_locator, &parsed.source_revision, outputs) {
        sink.mark_behind(ProOutputSinkError::new(
            "custom_history_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

pub(super) fn verify_committed_core(
    store: &Store,
    context: &ProviderAdapterContext,
    logical_locator: &str,
    stream: &str,
    source_revision: &str,
) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "custom history output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = decode_cursor(committed.provider_cursor())?;
    validate_cursor(&cursor, logical_locator)?;
    if cursor.retired
        || cursor.source_revision != source_revision
        || cursor.phase != CustomCursorPhase::Complete
    {
        return Err(CaptureError::InvalidPayload(
            "custom history output source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn replay_outputs(
    sink: &dyn ProOutputSink,
    logical_locator: &str,
    source_revision: &str,
    outputs: &[CustomOutput],
) -> Result<()> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Custom.as_str().to_owned(),
        namespace_id: logical_locator.to_owned(),
        source_id: logical_locator.to_owned(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let mut state = CustomOutputState::new(
        source,
        progress,
        source_revision,
        sink.materializer_revision(),
    )?;
    let mut next_output = state.next_output.min(outputs.len());
    if outputs.is_empty() {
        publish_output_page(
            sink,
            logical_locator,
            source_revision,
            outputs,
            &mut state,
            0,
            0,
            true,
        )?;
        return Ok(());
    }
    while next_output < outputs.len() {
        let mut end = next_output;
        let mut content_bytes = 0_usize;
        while end < outputs.len() && end.saturating_sub(next_output) < CUSTOM_OUTPUTS_PER_PAGE {
            let bytes = output_content(&outputs[end].payload)?.len();
            let next_bytes = content_bytes.saturating_add(bytes);
            if end != next_output && next_bytes > CUSTOM_OUTPUT_PAGE_BYTES {
                break;
            }
            if next_bytes > CUSTOM_OUTPUT_PAGE_BYTES {
                return Err(CaptureError::InvalidPayload(
                    "one custom history output exceeds the bounded Pro page".to_owned(),
                ));
            }
            content_bytes = next_bytes;
            end = end.saturating_add(1);
        }
        let terminal = end == outputs.len();
        if !publish_output_page(
            sink,
            logical_locator,
            source_revision,
            outputs,
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

pub(super) struct CustomOutputState {
    pub(super) source: OutputSourceIdentity,
    pub(super) source_epoch: u64,
    pub(super) expected_source_epoch: Option<u64>,
    pub(super) expected_sink_frontier: Option<NativeSafeFrontier>,
    pub(super) disposition: ProOutputSourceDisposition,
    pub(super) next_output: usize,
}

impl CustomOutputState {
    pub(super) fn new(
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
            .filter(|cursor| cursor.version == CUSTOM_OUTPUT_FRONTIER_VERSION)
            .and_then(|cursor| serde_json::from_slice::<CustomOutputFrontier>(&cursor.payload).ok())
            .filter(|frontier| frontier.version == CUSTOM_OUTPUT_FRONTIER_VERSION);
        let can_resume = progress.parser_revision == CUSTOM_PARSER_REVISION
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
                    "custom history output source epoch exhausted",
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
pub(super) fn publish_output_page(
    sink: &dyn ProOutputSink,
    logical_locator: &str,
    source_revision: &str,
    outputs: &[CustomOutput],
    state: &mut CustomOutputState,
    start: usize,
    end: usize,
    terminal: bool,
) -> Result<bool> {
    let expected_frontier = output_frontier(source_revision, start)?;
    let next_frontier = output_frontier(source_revision, end)?;
    let observations = outputs[start..end]
        .iter()
        .map(custom_output_observation)
        .collect::<Result<Vec<_>>>()?;
    let content_bytes = observations.iter().fold(0_usize, |total, output| {
        total.saturating_add(output.content.len())
    });
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: source_revision.to_owned(),
        parser_revision: CUSTOM_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Custom.as_str(), logical_locator),
        expected_frontier,
        next_frontier.clone(),
        terminal,
        NativePageAccounting {
            logical_units: end.saturating_sub(start).max(1),
            conservative_serialized_bytes: content_bytes
                .saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES),
        },
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

pub(super) fn custom_output_observation(output: &CustomOutput) -> Result<ProOutputObservation> {
    let call_id = output
        .payload
        .get("call_id")
        .or_else(|| output.payload.get("tool_call_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let command = output
        .payload
        .get("command")
        .and_then(Value::as_str)
        .map(|command| OutputCommandContext {
            tool_name: if output.event_type == EventType::CommandOutput {
                "command".to_owned()
            } else {
                output
                    .payload
                    .get("tool")
                    .or_else(|| output.payload.get("tool_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("custom")
                    .to_owned()
            },
            command: command.to_owned(),
            working_directory: output
                .payload
                .get("cwd")
                .or_else(|| output.payload.get("workdir"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    Ok(ProOutputObservation {
        kind: if output.event_type == EventType::CommandOutput {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "{}:{}:{}",
                output.source_id, output.session_id, output.event_index
            ),
            native_sequence: output.event_index,
            native_record_id: output.event_id.clone(),
            source_record_ordinal: Some(output.event_index),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(output.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: output.session_id.clone(),
            root_session_id: output.root_session_id.clone(),
            parent_session_id: output.parent_session_id.clone(),
            provider_session_id: Some(output.session_id.clone()),
            agent_id: output.external_agent_id.clone(),
            repository: None,
        },
        call_id,
        command,
        outcome: output_outcome(&output.payload),
        locator: OutputSourceLocator {
            version: 1,
            kind: "ctx-history-jsonl-v1-event-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "source_id": output.source_id,
                "session_id": output.session_id,
                "event_index": output.event_index,
                "event_id": output.event_id,
                "event_hash": output.event_hash,
            }))?,
        },
        content: output_content(&output.payload)?,
    })
}

pub(super) fn output_content(payload: &Value) -> Result<Vec<u8>> {
    for key in ["body", "output", "content", "text", "result"] {
        if let Some(value) = payload.get(key) {
            if let Some(text) = value.as_str() {
                return Ok(text.as_bytes().to_vec());
            }
            if !value.is_null() {
                return serde_json::to_vec(value).map_err(CaptureError::from);
            }
        }
    }
    serde_json::to_vec(payload).map_err(CaptureError::from)
}

pub(super) fn output_outcome(payload: &Value) -> OutputOutcomeMetadata {
    let exit_code = payload
        .get("exit_code")
        .or_else(|| payload.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = payload
        .get("duration_ms")
        .or_else(|| payload.get("durationMs"))
        .and_then(Value::as_u64);
    let status = payload
        .get("result_outcome")
        .or_else(|| payload.get("outcome"))
        .or_else(|| payload.get("status"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let outcome = if payload
        .get("timed_out")
        .or_else(|| payload.get("timedOut"))
        .and_then(Value::as_bool)
        == Some(true)
        || matches!(
            status.as_deref(),
            Some("timeout" | "timed_out" | "timedout")
        ) {
        OutputOutcome::Timeout
    } else if payload
        .get("is_error")
        .or_else(|| payload.get("isError"))
        .and_then(Value::as_bool)
        == Some(true)
        || exit_code.is_some_and(|code| code != 0)
        || matches!(
            status.as_deref(),
            Some("failure" | "failed" | "error" | "errored")
        )
    {
        OutputOutcome::Failure
    } else if payload
        .get("is_error")
        .or_else(|| payload.get("isError"))
        .and_then(Value::as_bool)
        == Some(false)
        || exit_code == Some(0)
        || matches!(
            status.as_deref(),
            Some("success" | "succeeded" | "complete" | "completed" | "ok")
        )
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

pub(super) fn output_frontier(
    source_revision: &str,
    next_output: usize,
) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        CUSTOM_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&CustomOutputFrontier {
            version: CUSTOM_OUTPUT_FRONTIER_VERSION,
            source_revision: source_revision.to_owned(),
            next_output: u64::try_from(next_output).map_err(|_| {
                CaptureError::InvalidPayload(
                    "custom history output frontier exceeds u64".to_owned(),
                )
            })?,
        })?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

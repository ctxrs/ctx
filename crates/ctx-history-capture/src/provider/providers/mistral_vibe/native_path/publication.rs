use super::*;

pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    publication: MistralVibeCorePublication<'_>,
) -> Result<(ProviderImportSummary, Checkpoint, String)> {
    let MistralVibeCorePublication {
        source,
        observation,
        page,
    } = publication;
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let stream = source_cursor_stream(&observation.canonical_messages_path)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let next_provider_cursor = encode_checkpoint(&page.next)?;
    let next_cursor = provider_sync_cursor(
        &context.machine_id,
        stream.clone(),
        next_provider_cursor,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        next_cursor,
    );
    let publication_id = publication_id(&page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;

    let raw_source_path = observation.canonical_messages_path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(&observation.canonical_messages_path)?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::MistralVibe,
            source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream.clone(),
            proposed_source_identity: page.next.canonical_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: page.next.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    if matches!(
        classification,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary {
            skipped_events: page.events.len(),
            skipped: page.events.len(),
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok((summary, page.next, resolution.canonical_source_identity));
    }
    let source_id = if page.next.generation == 0 {
        committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::MistralVibe,
                MISTRAL_VIBE_SOURCE_FORMAT,
                &context.machine_id,
                &resolution.canonical_source_identity,
                &page.next.session.provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                native_source_id(
                    &resolution.canonical_source_identity,
                    &page.next.session.provider_session_id,
                    page.next.generation,
                )
            })
    } else {
        native_source_id(
            &resolution.canonical_source_identity,
            &page.next.session.provider_session_id,
            page.next.generation,
        )
    };
    let capture_source = capture_source(
        context,
        &page.next.session,
        source_id,
        &raw_source_path,
        &source_root,
        &resolution.canonical_source_identity,
        &page.next.source_revision,
    );
    group.upsert_capture_source(&capture_source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session = canonical_session(
        committed_store,
        context,
        options,
        &page.next.session,
        source_id,
        &resolution.canonical_source_identity,
    )?;
    let session_existed = committed_store.get_session(session.id).is_ok();
    if let (Some(parent_id), Some(parent_external_id)) = (
        session.parent_session_id,
        page.next.session.parent_provider_session_id.as_deref(),
    ) {
        if committed_store.get_session(parent_id).is_err() {
            group.upsert_session(&relationship_placeholder(
                context,
                options,
                source_id,
                parent_id,
                parent_external_id,
                &resolution.canonical_source_identity,
            ))?;
        }
    }
    group.upsert_session(&session)?;
    let mut summary = ProviderImportSummary::default();
    if session_existed {
        summary.skipped_sessions = 1;
        summary.skipped = 1;
    } else {
        summary.imported_sessions = 1;
        summary.imported = 1;
    }
    if let Some(parent_id) = session.parent_session_id {
        let edge = relationship_edge(
            context,
            source_id,
            &session,
            parent_id,
            &resolution.canonical_source_identity,
        );
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    for event in &page.events {
        publish_event(
            &mut group,
            committed_store,
            context,
            options,
            source_id,
            &session,
            event,
            &mut summary,
        )?;
    }
    for detached in &page.detached_touches {
        publish_file_touches(
            &mut group,
            committed_store,
            context,
            options,
            source_id,
            &session,
            detached.ordinal,
            detached.occurred_at,
            None,
            &detached.touches,
            &mut summary,
        )?;
    }
    for rejection in page.rejections {
        summary.record_failure(rejection);
    }
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok((summary, page.next, resolution.canonical_source_identity))
}

pub(super) fn replay_outputs_or_mark_behind(
    source: &MistralVibeSessionSource,
    observation: &SourceObservation,
    core: &Checkpoint,
    canonical_source_identity: &str,
    profile: &ImportProfile,
) {
    if let Err(error) = replay_outputs(
        source,
        observation,
        core,
        canonical_source_identity,
        profile,
    ) {
        if let Some(sink) = profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                "mistral_vibe_nativepath_output_replay",
                error.to_string(),
            ));
        }
    }
}

pub(super) fn replay_outputs(
    source: &MistralVibeSessionSource,
    observation: &SourceObservation,
    core: &Checkpoint,
    canonical_source_identity: &str,
    profile: &ImportProfile,
) -> Result<()> {
    let Some(sink) = profile.sink().map(std::sync::Arc::as_ref) else {
        return Ok(());
    };
    if !observation.revalidate(source)?
        || core.complete_prefix_end > observation.messages.length
        || hash_file_prefix(
            &observation.canonical_messages_path,
            core.complete_prefix_end,
        )? != core.complete_prefix_sha256
    {
        sink.mark_behind(ProOutputSinkError::new(
            "source_changed",
            "Mistral Vibe source changed before output replay",
        ));
        return Ok(());
    }
    let output_source_id =
        exact_source_progress_id(canonical_source_identity, &core.session.provider_session_id)?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::MistralVibe.as_str().to_owned(),
        namespace_id: core.machine_id.clone(),
        source_id: output_source_id.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let plan = output_plan(progress, sink.materializer_revision(), observation, core)?;
    if plan.no_op {
        return Ok(());
    }
    let mut reader = BufReader::new(File::open(&observation.canonical_messages_path)?);
    reader.seek(SeekFrom::Start(plan.frontier.complete_prefix_end))?;
    let mut hasher = hash_prefix(
        &observation.canonical_messages_path,
        plan.frontier.complete_prefix_end,
        initial_prefix_hasher(),
    )?;
    let mut frontier = plan.frontier;
    let mut disposition = plan.disposition;
    let mut expected_prior_frontier = plan.expected_prior_frontier;
    let mut expected_prior_epoch = plan.expected_prior_epoch;

    loop {
        let expected = frontier.safe_frontier()?;
        let mut observations = Vec::new();
        let mut physical_records = 0_usize;
        let mut estimated_bytes = PAGE_BASE_BYTES;
        let mut terminal = false;
        while physical_records < PAGE_MAX_UNITS
            && frontier.complete_prefix_end < core.complete_prefix_end
        {
            let start = frontier.complete_prefix_end;
            let ordinal = frontier.next_ordinal;
            let hasher_before = hasher.clone();
            let line =
                read_bounded_line(&mut reader, &mut hasher, core.complete_prefix_end, start)?;
            let (bytes, end) = match line {
                Line::EndOfFile => {
                    terminal = core.terminal;
                    break;
                }
                Line::IncompleteTail => {
                    hasher = hasher_before;
                    reader.seek(SeekFrom::Start(start))?;
                    break;
                }
                Line::Oversized { end } => (Vec::new(), end),
                Line::Complete { bytes, end } => (bytes, end),
            };
            if !bytes.is_empty() {
                match output_observation(&bytes, ordinal, start, end, &core.session) {
                    Ok(Some(output)) => {
                        let output_bytes = estimate_output_bytes(&output);
                        if output_bytes > PAGE_MAX_BYTES {
                            sink.mark_behind(ProOutputSinkError::new(
                                "output_too_large",
                                "Mistral Vibe output exceeds the bounded Pro page",
                            ));
                            return Ok(());
                        }
                        if !observations.is_empty()
                            && estimated_bytes.saturating_add(output_bytes) > PAGE_MAX_BYTES
                        {
                            hasher = hasher_before;
                            reader.seek(SeekFrom::Start(start))?;
                            break;
                        }
                        estimated_bytes = estimated_bytes.saturating_add(output_bytes);
                        observations.push(output);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        sink.mark_behind(ProOutputSinkError::new(
                            "malformed_output",
                            error.to_string(),
                        ));
                        return Ok(());
                    }
                }
            }
            frontier.complete_prefix_end = end;
            frontier.next_ordinal = frontier.next_ordinal.saturating_add(1);
            frontier.complete_prefix_sha256 = prefix_digest(&hasher);
            physical_records = physical_records.saturating_add(1);
        }
        if frontier.complete_prefix_end == core.complete_prefix_end {
            terminal = core.terminal;
        }
        if physical_records == 0
            && observations.is_empty()
            && expected == frontier.safe_frontier()?
        {
            return Ok(());
        }
        let next = frontier.safe_frontier()?;
        let logical_units = observations.len();
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: plan.source_epoch,
            observed_revision: core.source_revision.clone(),
            parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_prior_epoch,
            expected_prior_frontier: expected_prior_frontier.clone(),
            observations,
        };
        let page = match NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::MistralVibe.as_str(),
                output_source_id.clone(),
            ),
            expected,
            next.clone(),
            terminal,
            NativePageAccounting {
                logical_units,
                conservative_serialized_bytes: estimated_bytes
                    .saturating_add(next.bytes.len())
                    .saturating_add(4096),
            },
            output,
        ) {
            Ok(page) => page,
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "invalid_output_page",
                    error.to_string(),
                ));
                return Ok(());
            }
        };
        if process_pro_replay_only(page, sink).is_err() {
            return Ok(());
        }
        disposition = ProOutputSourceDisposition::AppendOrResume;
        expected_prior_epoch = Some(plan.source_epoch);
        expected_prior_frontier = Some(next);
        if terminal {
            return Ok(());
        }
    }
}

pub(super) struct OutputPlan {
    pub(super) frontier: OutputFrontier,
    pub(super) source_epoch: u64,
    pub(super) disposition: ProOutputSourceDisposition,
    pub(super) expected_prior_epoch: Option<u64>,
    pub(super) expected_prior_frontier: Option<NativeSafeFrontier>,
    pub(super) no_op: bool,
}

pub(super) fn output_plan(
    progress: Option<ProOutputProgress>,
    materializer_revision: &str,
    observation: &SourceObservation,
    core: &Checkpoint,
) -> Result<OutputPlan> {
    let fresh = OutputFrontier {
        version: OUTPUT_FRONTIER_VERSION,
        complete_prefix_end: 0,
        next_ordinal: 0,
        complete_prefix_sha256: initial_prefix_digest(),
        generation_identity: core.generation_identity,
    };
    let Some(progress) = progress else {
        return Ok(OutputPlan {
            frontier: fresh,
            source_epoch: 0,
            disposition: ProOutputSourceDisposition::NewSource,
            expected_prior_epoch: None,
            expected_prior_frontier: None,
            no_op: false,
        });
    };
    let raw_prior = progress
        .cursor
        .as_ref()
        .map(|cursor| {
            NativeSafeFrontier::new(cursor.version, cursor.payload.clone())
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
        })
        .transpose()?;
    let decoded = progress.cursor.as_ref().and_then(OutputFrontier::decode);
    let valid_prefix = decoded.as_ref().is_some_and(|frontier| {
        frontier.generation_identity == core.generation_identity
            && frontier.complete_prefix_end <= core.complete_prefix_end
            && hash_file_prefix(
                &observation.canonical_messages_path,
                frontier.complete_prefix_end,
            )
            .is_ok_and(|digest| digest == frontier.complete_prefix_sha256)
    });
    let compatible = progress.parser_revision == OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && valid_prefix;
    if compatible {
        let frontier = decoded.expect("compatible output frontier is decoded");
        let no_op = progress.terminal
            && core.terminal
            && frontier.complete_prefix_end == core.complete_prefix_end
            && frontier.complete_prefix_sha256 == core.complete_prefix_sha256;
        return Ok(OutputPlan {
            frontier,
            source_epoch: progress.source_epoch,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            expected_prior_epoch: Some(progress.source_epoch),
            expected_prior_frontier: raw_prior,
            no_op,
        });
    }
    Ok(OutputPlan {
        frontier: fresh,
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe output epoch overflowed",
            ))?,
        disposition: ProOutputSourceDisposition::Rewrite,
        expected_prior_epoch: Some(progress.source_epoch),
        expected_prior_frontier: raw_prior,
        no_op: false,
    })
}

pub(super) fn output_observation(
    bytes: &[u8],
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    session: &SessionFact,
) -> Result<Option<ProOutputObservation>> {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Ok(role) = valid_mistral_vibe_record_role(&value) else {
        return Ok(None);
    };
    if mistral_vibe_event_type(role, &value) != EventType::ToolOutput {
        return Ok(None);
    }
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe output line number exceeds platform limits",
        ))?;
    let metadata = output_metadata(&value, line_number, role, session.cwd.as_deref());
    let content = mistral_vibe_result_content(&value).unwrap_or_default();
    let mut locator = Vec::with_capacity(16);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    Ok(Some(ProOutputObservation {
        kind: metadata.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: metadata.native_record_id.clone(),
            native_sequence: ordinal,
            native_record_id: Some(metadata.native_record_id),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(
            native_jsonl_timestamp(&value)
                .unwrap_or(session.started_at)
                .timestamp_millis(),
        ),
        associations: OutputAssociations {
            direct_session_id: session.provider_session_id.clone(),
            root_session_id: session
                .parent_provider_session_id
                .clone()
                .unwrap_or_else(|| session.provider_session_id.clone()),
            parent_session_id: session.parent_provider_session_id.clone(),
            provider_session_id: Some(session.provider_session_id.clone()),
            agent_id: session.external_agent_id.clone(),
            repository: None,
        },
        call_id: metadata.call_id,
        command: metadata.command,
        outcome: metadata.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "mistral-vibe-jsonl-range-v1".to_owned(),
            payload: locator,
        },
        content: content.into_bytes(),
    }))
}

pub(super) fn exact_source_progress_id(
    canonical_source_identity: &str,
    provider_session_id: &str,
) -> Result<String> {
    let encoded = serde_json::to_string(&(
        "mistral-vibe-exact-source-v1",
        canonical_source_identity,
        provider_session_id,
    ))?;
    Ok(stable_capture_uuid(&encoded, "mistral-vibe-output-source").to_string())
}

pub(super) struct OutputMetadata {
    pub(super) kind: OutputObservationKind,
    pub(super) native_record_id: String,
    pub(super) call_id: Option<String>,
    pub(super) command: Option<OutputCommandContext>,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn output_metadata(
    value: &Value,
    line_number: usize,
    role: &str,
    session_cwd: Option<&str>,
) -> OutputMetadata {
    let call_id = value
        .get("tool_call_id")
        .or_else(|| value.get("toolCallId"))
        .or_else(|| value.get("call_id"))
        .or_else(|| value.get("callId"))
        .or_else(|| value.get("tool_use_id"))
        .or_else(|| value.get("toolUseId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let tool_name = value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("tool"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool")
        .to_owned();
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: tool_name.clone(),
        command: value
            .get("input")
            .or_else(|| value.get("arguments"))
            .or_else(|| value.get("args"))
            .and_then(tool_input::command)
            .unwrap_or_default(),
        working_directory: value
            .get("input")
            .or_else(|| value.get("arguments"))
            .or_else(|| value.get("args"))
            .and_then(tool_input::working_directory)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
    let timed_out = value_timed_out(value);
    let exit_code =
        i64_field(value, &["exit_code", "exitCode"]).and_then(|value| i32::try_from(value).ok());
    let duration_ms = i64_field(value, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputMetadata {
        kind,
        native_record_id: mistral_vibe_event_id(value, line_number, role),
        call_id,
        command,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    }
}

pub(super) fn value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

pub(super) fn i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| values.values().find_map(|value| i64_field(value, fields))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

pub(super) fn estimate_output_bytes(output: &ProOutputObservation) -> usize {
    OUTPUT_BASE_BYTES
        .saturating_add(output.coordinate.unit_key.len())
        .saturating_add(output.content.len())
        .saturating_add(output.call_id.as_deref().map_or(0, str::len))
        .saturating_add(
            output
                .command
                .as_ref()
                .map_or(0, |command| command.tool_name.len() + command.command.len()),
        )
        .saturating_add(output.locator.payload.len())
}

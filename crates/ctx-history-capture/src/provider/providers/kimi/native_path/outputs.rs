use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct KimiOutputReplay {
    behind: bool,
    rejected_outputs: u64,
}

pub(super) fn replay_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) -> Result<KimiOutputReplay> {
    let Some(sink) = sink else {
        return Ok(KimiOutputReplay::default());
    };
    replay_kimi_outputs(paths, source_root, imported_at, sink)
}

pub(super) fn replay_kimi_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<KimiOutputReplay> {
    let mut replay = KimiOutputReplay::default();
    for path in paths {
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::KimiCodeCli.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(_) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "kimi_output_progress",
                    "Kimi Pro output progress is unavailable",
                ));
                replay.behind = true;
                continue;
            }
        };
        let source_replay = replay_kimi_source(
            path,
            source_root,
            imported_at,
            sink,
            source,
            locator_identity,
            progress,
        )?;
        replay.behind |= source_replay.behind;
        replay.rejected_outputs = replay
            .rejected_outputs
            .saturating_add(source_replay.rejected_outputs);
    }
    Ok(replay)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn replay_kimi_source(
    path: &Path,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
) -> Result<KimiOutputReplay> {
    let observation = KimiWireObservation::read(path)?;
    let scope_revision =
        kimi_admission_scope_revision_for_display(Some(source_root.display().to_string()));
    let observed_revision = observation.source_revision(&scope_revision);
    let route_sha256 = route_sha256(&locator_identity);
    let progress_checkpoint = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == KIMI_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<KimiNativeCheckpoint>(&cursor.payload).ok());
    let parser_matches = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == KIMI_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
    });
    let (mut checkpoint, start_offset, start_ordinal, mut hasher, source_can_resume) =
        plan_output_scan(
            path,
            &observation,
            route_sha256,
            scope_revision,
            parser_matches
                .then_some(progress_checkpoint.as_ref())
                .flatten(),
        )?;
    if source_can_resume
        && progress.as_ref().is_some_and(|progress| {
            progress.terminal && progress.observed_revision == observed_revision
        })
        && checkpoint.terminal
    {
        return Ok(KimiOutputReplay {
            behind: false,
            rejected_outputs: checkpoint.rejected_outputs,
        });
    }
    let mut state = match KimiOutputState::new(
        output_source,
        progress,
        source_can_resume,
        sink.materializer_revision(),
    ) {
        Ok(state) => state,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "kimi_output_progress",
                "Kimi Pro output progress is invalid",
            ));
            return Ok(KimiOutputReplay {
                behind: true,
                rejected_outputs: checkpoint.rejected_outputs,
            });
        }
    };
    let mut file = File::open(path)?;
    if KimiFrozenFileMetadata::from_metadata(&file.metadata()?)? != *observation.wire() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(start_offset))?;
    let mut reader = BufReader::new(file);
    let mut offset = start_offset;
    let mut ordinal = start_ordinal;
    let mut expected_checkpoint = checkpoint.clone();
    let mut observations = Vec::new();
    let mut page_units = 0_usize;
    let mut reached_eof = false;

    while !reached_eof {
        let checkpoint_before = checkpoint.clone();
        let hasher_before = hasher.clone();
        let raw = read_bounded_line(&mut reader, &mut hasher, MAX_PROVIDER_JSONL_LINE_BYTES)?;
        if raw.observed_bytes == 0 {
            reached_eof = true;
        } else if !raw.terminated {
            hasher = hasher_before;
            reached_eof = true;
        } else {
            let byte_start = offset;
            offset =
                offset
                    .checked_add(raw.observed_bytes)
                    .ok_or(CaptureError::SystemInvariant(
                        "Kimi output byte offset overflowed",
                    ))?;
            let line_number = usize::try_from(ordinal)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(CaptureError::SystemInvariant(
                    "Kimi output line number overflowed",
                ))?;
            let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Kimi output ordinal overflowed",
            ))?;
            checkpoint.complete_offset = offset;
            checkpoint.next_ordinal = next_ordinal;
            checkpoint.committed_prefix_sha256 = prefix_digest(&hasher);
            checkpoint.observed_file_len = observation.wire().length;
            checkpoint.wire_revision = observation.wire().revision_component();
            checkpoint.terminal = false;
            checkpoint.retired = false;
            let mut output = None;
            if !raw.oversized {
                let record = json_record_bytes(&raw.bytes);
                if let Ok(value) = serde_json::from_slice::<Value>(record) {
                    output = match kimi_output_observation(
                        &observation,
                        &locator_identity,
                        ordinal,
                        line_number,
                        byte_start,
                        offset,
                        &value,
                        imported_at,
                    ) {
                        Ok(output) => output,
                        Err(_) => {
                            sink.mark_behind(ProOutputSinkError::new(
                                "kimi_output_page",
                                "Kimi Pro output observation is invalid",
                            ));
                            return Ok(KimiOutputReplay {
                                behind: true,
                                rejected_outputs: checkpoint.rejected_outputs,
                            });
                        }
                    };
                }
            }

            let candidate_bytes = kimi_output_page_owned_bytes(
                &state,
                &observed_revision,
                sink.materializer_revision(),
                &checkpoint,
                &observations,
                output.as_ref(),
            )?;
            if page_units != 0
                && (page_units.saturating_add(1) > KIMI_OUTPUT_PAGE_MAX_OBSERVATIONS
                    || candidate_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES)
            {
                if !publish_output_page(
                    sink,
                    &observation,
                    &locator_identity,
                    &observed_revision,
                    &mut state,
                    &expected_checkpoint,
                    &checkpoint_before,
                    false,
                    page_units,
                    std::mem::take(&mut observations),
                )? {
                    return Ok(KimiOutputReplay {
                        behind: true,
                        rejected_outputs: checkpoint_before.rejected_outputs,
                    });
                }
                expected_checkpoint = checkpoint_before;
                page_units = 0;
            }

            if let Some(candidate) = output.as_ref() {
                let singleton_bytes = kimi_output_page_owned_bytes(
                    &state,
                    &observed_revision,
                    sink.materializer_revision(),
                    &checkpoint,
                    &[],
                    Some(candidate),
                )?;
                if singleton_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
                    checkpoint.rejected_outputs = checkpoint.rejected_outputs.saturating_add(1);
                    output = None;
                }
            }
            if kimi_output_page_owned_bytes(
                &state,
                &observed_revision,
                sink.materializer_revision(),
                &checkpoint,
                &observations,
                output.as_ref(),
            )? > NATIVE_INGESTION_PAGE_MAX_BYTES
            {
                return Err(CaptureError::SystemInvariant(
                    "Kimi bounded singleton Pro page exceeds its exact owned-byte bound",
                ));
            }
            page_units = page_units.saturating_add(1);
            if let Some(output) = output {
                observations.push(output);
            }
            ordinal = next_ordinal;
        }
    }
    checkpoint.terminal = offset == observation.wire().length;
    let published = publish_output_page(
        sink,
        &observation,
        &locator_identity,
        &observed_revision,
        &mut state,
        &expected_checkpoint,
        &checkpoint,
        checkpoint.terminal,
        page_units.max(1),
        observations,
    )?;
    Ok(KimiOutputReplay {
        behind: !published,
        rejected_outputs: checkpoint.rejected_outputs,
    })
}

pub(super) fn plan_output_scan(
    path: &Path,
    observation: &KimiWireObservation,
    route_sha256: [u8; 32],
    scope_revision: String,
    previous: Option<&KimiNativeCheckpoint>,
) -> Result<(KimiNativeCheckpoint, u64, u64, Sha256, bool)> {
    let Some(previous) = previous else {
        let checkpoint = KimiNativeCheckpoint::initial(route_sha256, observation, scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let physical = observation.wire().physical_identity();
    let identity_matches = previous.version == KIMI_NATIVE_CURSOR_VERSION
        && !previous.retired
        && previous.route_sha256 == route_sha256
        && previous.physical_device == physical.0
        && previous.physical_inode == physical.1
        && previous.auxiliary_revision == observation.session.auxiliary_revision
        && previous.admission_scope_revision == scope_revision
        && previous.complete_offset <= observation.wire().length;
    if !identity_matches {
        let checkpoint = KimiNativeCheckpoint::initial(route_sha256, observation, scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    }
    let Some(hasher) = verify_prefix(path, previous)? else {
        let checkpoint = KimiNativeCheckpoint::initial(route_sha256, observation, scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    Ok((
        previous.clone(),
        previous.complete_offset,
        previous.next_ordinal,
        hasher,
        true,
    ))
}

pub(super) struct KimiOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl KimiOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume
            || progress.parser_revision != KIMI_OUTPUT_PARSER_REVISION
            || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Kimi output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

#[derive(Default)]
pub(super) struct KimiOwnedByteCounter {
    bytes: usize,
}

impl KimiOwnedByteCounter {
    fn fixed(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.fixed(std::mem::size_of::<u64>());
        self.fixed(value.len());
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn optional_string(&mut self, value: Option<&str>) {
        self.fixed(std::mem::size_of::<u8>());
        if let Some(value) = value {
            self.string(value);
        }
    }

    fn optional_fixed(&mut self, present: bool, bytes: usize) {
        self.fixed(std::mem::size_of::<u8>());
        if present {
            self.fixed(bytes);
        }
    }

    fn frontier(&mut self, frontier: &NativeSafeFrontier) {
        self.fixed(std::mem::size_of::<u32>());
        self.bytes(&frontier.bytes);
    }

    fn optional_frontier(&mut self, frontier: Option<&NativeSafeFrontier>) {
        self.fixed(std::mem::size_of::<u8>());
        if let Some(frontier) = frontier {
            self.frontier(frontier);
        }
    }
}

pub(super) fn add_kimi_output_observation_owned_bytes(
    counter: &mut KimiOwnedByteCounter,
    observation: &ProOutputObservation,
) {
    counter.fixed(std::mem::size_of::<u8>());
    counter.string(&observation.coordinate.unit_key);
    counter.fixed(std::mem::size_of::<u64>());
    counter.optional_string(observation.coordinate.native_record_id.as_deref());
    counter.optional_fixed(
        observation.coordinate.source_record_ordinal.is_some(),
        std::mem::size_of::<u64>(),
    );
    counter.optional_fixed(
        observation
            .coordinate
            .source_record_subrecord_index
            .is_some(),
        std::mem::size_of::<u32>(),
    );
    counter.optional_fixed(
        observation.coordinate.byte_start.is_some(),
        std::mem::size_of::<u64>(),
    );
    counter.optional_fixed(
        observation.coordinate.byte_end_exclusive.is_some(),
        std::mem::size_of::<u64>(),
    );
    counter.optional_fixed(
        observation.occurred_at_unix_ms.is_some(),
        std::mem::size_of::<i64>(),
    );
    counter.string(&observation.associations.direct_session_id);
    counter.string(&observation.associations.root_session_id);
    counter.optional_string(observation.associations.parent_session_id.as_deref());
    counter.optional_string(observation.associations.provider_session_id.as_deref());
    counter.optional_string(observation.associations.agent_id.as_deref());
    counter.fixed(std::mem::size_of::<u8>());
    if let Some(repository) = &observation.associations.repository {
        counter.string(&repository.repository_id);
        counter.optional_string(repository.checkout_id.as_deref());
        counter.optional_string(repository.worktree_id.as_deref());
        counter.optional_string(repository.object_format.as_deref());
    }
    counter.optional_string(observation.call_id.as_deref());
    counter.fixed(std::mem::size_of::<u8>());
    if let Some(command) = &observation.command {
        counter.string(&command.tool_name);
        counter.string(&command.command);
        counter.optional_string(command.working_directory.as_deref());
    }
    counter.fixed(std::mem::size_of::<u8>());
    counter.optional_fixed(
        observation.outcome.exit_code.is_some(),
        std::mem::size_of::<i32>(),
    );
    counter.optional_fixed(
        observation.outcome.duration_ms.is_some(),
        std::mem::size_of::<u64>(),
    );
    counter.fixed(std::mem::size_of::<u32>());
    counter.string(&observation.locator.kind);
    counter.bytes(&observation.locator.payload);
    counter.bytes(&observation.content);
}

pub(super) fn kimi_output_page_owned_bytes(
    state: &KimiOutputState,
    observed_revision: &str,
    materializer_revision: &str,
    next_checkpoint: &KimiNativeCheckpoint,
    observations: &[ProOutputObservation],
    additional: Option<&ProOutputObservation>,
) -> Result<usize> {
    let next_frontier = next_checkpoint.safe_frontier()?;
    let mut counter = KimiOwnedByteCounter::default();
    counter.fixed(32);
    counter.frontier(&next_frontier);
    counter.fixed(std::mem::size_of::<u8>());
    counter.fixed(std::mem::size_of::<u64>());
    counter.string(&state.source.provider);
    counter.string(&state.source.namespace_id);
    counter.string(&state.source.source_id);
    counter.fixed(std::mem::size_of::<u64>());
    counter.string(observed_revision);
    counter.string(KIMI_OUTPUT_PARSER_REVISION);
    counter.string(materializer_revision);
    counter.fixed(std::mem::size_of::<u8>());
    counter.optional_fixed(
        state.expected_source_epoch.is_some(),
        std::mem::size_of::<u64>(),
    );
    counter.optional_frontier(state.expected_sink_frontier.as_ref());
    counter.fixed(std::mem::size_of::<u64>());
    for observation in observations {
        add_kimi_output_observation_owned_bytes(&mut counter, observation);
    }
    if let Some(observation) = additional {
        add_kimi_output_observation_owned_bytes(&mut counter, observation);
    }
    Ok(counter.bytes)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_output_page(
    sink: &dyn ProOutputSink,
    observation: &KimiWireObservation,
    locator_identity: &str,
    observed_revision: &str,
    state: &mut KimiOutputState,
    expected_checkpoint: &KimiNativeCheckpoint,
    next_checkpoint: &KimiNativeCheckpoint,
    terminal: bool,
    logical_units: usize,
    observations: Vec<ProOutputObservation>,
) -> Result<bool> {
    if !observation.revalidate(observation.canonical_path())? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let output_page = (|| {
        let expected_frontier = expected_checkpoint.safe_frontier()?;
        let next_safe_frontier = next_checkpoint.safe_frontier()?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: observed_revision.to_owned(),
            parser_revision: KIMI_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations,
        };
        let exact_owned_bytes = kimi_output_page_owned_bytes(
            state,
            observed_revision,
            sink.materializer_revision(),
            next_checkpoint,
            &output.observations,
            None,
        )?;
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::KimiCodeCli.as_str(), locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            terminal,
            NativePageAccounting {
                logical_units,
                conservative_serialized_bytes: exact_owned_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok::<_, CaptureError>((replay, next_safe_frontier))
    })();
    let (replay, next_safe_frontier) = output_page?;
    if process_pro_replay_only(replay, sink).is_err() {
        return Ok(false);
    }
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_safe_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

pub(super) fn record_output_replay(summary: &mut ProviderImportSummary, replay: KimiOutputReplay) {
    if replay.behind {
        summary.record_failure(ProviderImportFailure {
            line: 0,
            error: "Kimi Pro output is behind committed Core".to_owned(),
        });
    }
    summary.failed = summary
        .failed
        .saturating_add(usize::try_from(replay.rejected_outputs).unwrap_or(usize::MAX));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn kimi_output_observation(
    observation: &KimiWireObservation,
    locator_identity: &str,
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    value: &Value,
    imported_at: DateTime<Utc>,
) -> Result<Option<ProOutputObservation>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if kimi_event_type(record_type, value) != EventType::ToolOutput {
        return Ok(None);
    }
    let metadata = kimi_output_metadata(value, line_number, observation.session.cwd.as_deref());
    let content = kimi_output_content(value).unwrap_or_default().into_bytes();
    let occurred_at = kimi_record_timestamp(value, imported_at).unwrap_or(imported_at);
    let source_item = locator_identity.as_bytes();
    let source_len = u32::try_from(source_item.len()).map_err(|_| {
        CaptureError::InvalidPayload("Kimi output source identity exceeds u32".to_owned())
    })?;
    let mut locator = Vec::with_capacity(source_item.len().saturating_add(20));
    locator.extend_from_slice(&source_len.to_be_bytes());
    locator.extend_from_slice(source_item);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    let direct_session_id = observation.session.provider_session_id.clone();
    Ok(Some(ProOutputObservation {
        kind: metadata.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("{}:0", metadata.native_record_id),
            native_sequence: ordinal,
            native_record_id: Some(metadata.native_record_id),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: observation
                .session
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: observation.session.parent_provider_session_id.clone(),
            provider_session_id: Some(direct_session_id),
            agent_id: Some(observation.session.agent_id.clone()),
            repository: None,
        },
        call_id: metadata.call_id,
        command: metadata.command,
        outcome: metadata.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: KIMI_OUTPUT_LOCATOR_KIND.to_owned(),
            payload: locator,
        },
        content,
    }))
}

pub(super) struct KimiOutputMetadata {
    pub(super) kind: OutputObservationKind,
    pub(super) native_record_id: String,
    pub(super) call_id: Option<String>,
    pub(super) command: Option<OutputCommandContext>,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn kimi_output_metadata(
    value: &Value,
    line_number: usize,
    session_cwd: Option<&str>,
) -> KimiOutputMetadata {
    let event = value.get("event").unwrap_or(value);
    let call_id = [
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ]
    .into_iter()
    .find_map(|field| event.get(field).and_then(Value::as_str))
    .filter(|value| !value.trim().is_empty())
    .map(str::to_owned);
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .or_else(|| event.get("name"))
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
        command: event
            .get("input")
            .or_else(|| event.get("arguments"))
            .or_else(|| event.get("args"))
            .and_then(tool_input::command)
            .unwrap_or_default(),
        working_directory: event
            .get("input")
            .or_else(|| event.get("arguments"))
            .or_else(|| event.get("args"))
            .and_then(tool_input::working_directory)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
    let timed_out = kimi_value_timed_out(event);
    let exit_code = kimi_i64_field(event, &["exit_code", "exitCode"])
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = kimi_i64_field(event, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(event) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, event).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let native_record_id = kimi_legacy_provider_event_hash(record_type, value, line_number);
    KimiOutputMetadata {
        kind,
        native_record_id,
        call_id,
        command,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    }
}

pub(super) fn kimi_value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(kimi_value_timed_out),
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
            }) || values.values().any(kimi_value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

pub(super) fn kimi_i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| kimi_i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| kimi_i64_field(value, fields))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

#[cfg(test)]
mod tests;

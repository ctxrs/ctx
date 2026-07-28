use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenHandsOutputFrontier {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    route_sha256: [u8; 32],
    content_sha256: Option<[u8; 32]>,
    terminal: bool,
    deleted: bool,
}

impl OpenHandsOutputFrontier {
    fn initial(route_sha256: [u8; 32]) -> Self {
        Self {
            version: OPENHANDS_OUTPUT_FRONTIER_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256,
            content_sha256: None,
            terminal: false,
            deleted: false,
        }
    }

    fn terminal(source: &OpenHandsObservedFile) -> Self {
        Self {
            version: OPENHANDS_OUTPUT_FRONTIER_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256: source.route_sha256,
            content_sha256: source.content_sha256,
            terminal: true,
            deleted: false,
        }
    }

    fn deleted(route_sha256: [u8; 32]) -> Self {
        Self {
            version: OPENHANDS_OUTPUT_FRONTIER_VERSION,
            parser_revision: OPENHANDS_NATIVE_PARSER_REVISION,
            policy_revision: OPENHANDS_NATIVE_POLICY_REVISION,
            route_sha256,
            content_sha256: None,
            terminal: true,
            deleted: true,
        }
    }

    fn safe(&self) -> Result<NativeSafeFrontier> {
        NativeSafeFrontier::new(OPENHANDS_OUTPUT_FRONTIER_VERSION, serde_json::to_vec(self)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }
}

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    live_paths: &BTreeSet<PathBuf>,
    inventory: &OpenHandsInventory,
    known_routes: &[KnownOpenHandsRoute],
    relocation_state: &OpenHandsRelocationState,
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(
        store,
        live_paths,
        inventory,
        known_routes,
        relocation_state,
        source_root,
        context,
        sink,
    ) {
        sink.mark_behind(ProOutputSinkError::new(
            "openhands_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    store: &Store,
    live_paths: &BTreeSet<PathBuf>,
    inventory: &OpenHandsInventory,
    known_routes: &[KnownOpenHandsRoute],
    relocation_state: &OpenHandsRelocationState,
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in live_paths {
        let source = inventory.open_source(path)?;
        if !core_source_is_committed(store, &source, context)? {
            sink.mark_behind(ProOutputSinkError::new(
                "openhands_core_not_committed",
                "OpenHands output replay requires the exact terminal NativePath Core source",
            ));
            continue;
        }
        let identity_path = relocation_state
            .output_identity_paths
            .get(path)
            .or_else(|| {
                known_routes
                    .iter()
                    .find(|route| route.path == *path)
                    .map(|route| &route.identity_path)
            })
            .map_or(source.path_identity.as_str(), String::as_str);
        replay_live_output(&source, identity_path, source_root, context, sink)?;
    }
    for route in known_routes.iter().filter(|route| {
        !live_paths.contains(&route.path)
            && !relocation_state
                .relocated_locators
                .contains(&route.locator_identity)
    }) {
        replay_deleted_output(route, source_root, sink)?;
    }
    Ok(())
}

fn core_source_is_committed(
    store: &Store,
    source: &OpenHandsObservedFile,
    context: &ProviderAdapterContext,
) -> Result<bool> {
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
    else {
        return Ok(false);
    };
    let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) else {
        return Ok(false);
    };
    let Ok(cursor) = serde_json::from_str::<OpenHandsNativeCursor>(committed.provider_cursor())
    else {
        return Ok(false);
    };
    Ok(cursor.supported_for(source)
        && cursor.terminal
        && cursor.content_sha256 == source.content_sha256)
}

fn replay_live_output(
    source: &OpenHandsObservedFile,
    identity_path: &str,
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::OpenHands.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: identity_path.to_owned(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let next = OpenHandsOutputFrontier::terminal(source);
    let prior = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == OPENHANDS_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<OpenHandsOutputFrontier>(&cursor.payload).ok());
    let exact = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == OPENHANDS_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.terminal
            && prior.as_ref() == Some(&next)
    });
    if exact {
        return Ok(());
    }
    let expected_frontier = OpenHandsOutputFrontier::initial(source.route_sha256).safe()?;
    let next_safe_frontier = next.safe()?;
    let mut observations = Vec::new();
    if let Some(raw_bytes) = source.raw_bytes.as_deref() {
        if let Ok(decoded) = decode_openhands_event(&source.canonical_path, raw_bytes) {
            if matches!(
                decoded.event_type(),
                EventType::ToolOutput | EventType::CommandOutput
            ) {
                if let Some(content) = super::openhands_result_content(&decoded) {
                    observations.push(output_observation(source, &decoded, content));
                }
            }
        }
    }
    let can_resume = prior.as_ref().is_some_and(|prior| {
        prior.version == OPENHANDS_OUTPUT_FRONTIER_VERSION
            && prior.parser_revision == OPENHANDS_NATIVE_PARSER_REVISION
            && prior.policy_revision == OPENHANDS_NATIVE_POLICY_REVISION
            && prior.route_sha256 == source.route_sha256
            && !prior.deleted
            && prior.content_sha256 == source.content_sha256
    });
    let state = output_state(progress, can_resume, sink.materializer_revision())?;
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: output_source,
        source_epoch: state.source_epoch,
        observed_revision: source.cursor_revision(None),
        parser_revision: OPENHANDS_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier,
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::OpenHands.as_str(), identity_path),
        expected_frontier,
        next_safe_frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(replay, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "openhands_output_page",
            format!("{:?}", failure.output_error),
        ));
    }
    let _ = context;
    Ok(())
}

fn replay_deleted_output(
    route: &KnownOpenHandsRoute,
    source_root: &Path,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::OpenHands.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: route.identity_path.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(Some(progress)) => progress,
        Ok(None) => return Ok(()),
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let route_sha256 = route.checkpoint.as_ref().map_or_else(
        || route_hash(&route.path_identity),
        |cursor| cursor.route_sha256,
    );
    let next = OpenHandsOutputFrontier::deleted(route_sha256);
    let prior = progress
        .cursor
        .as_ref()
        .and_then(|cursor| serde_json::from_slice::<OpenHandsOutputFrontier>(&cursor.payload).ok());
    if prior.as_ref() == Some(&next)
        && progress.parser_revision == OPENHANDS_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
    {
        return Ok(());
    }
    let source_epoch =
        progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands deleted output source epoch exhausted",
            ))?;
    let expected_prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::OpenHands.as_str(), &route.identity_path),
        OpenHandsOutputFrontier::initial(route_sha256).safe()?,
        next.safe()?,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source,
            source_epoch,
            observed_revision: "openhands-nativepath-source-deleted-v1".to_owned(),
            parser_revision: OPENHANDS_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: ProOutputSourceDisposition::Rewrite,
            expected_prior_source_epoch: Some(progress.source_epoch),
            expected_prior_frontier,
            observations: Vec::new(),
        },
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(replay, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "openhands_output_delete",
            format!("{:?}", failure.output_error),
        ));
    }
    Ok(())
}

struct OutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    can_resume_source: bool,
    materializer_revision: &str,
) -> Result<OutputState> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let can_resume = progress.parser_revision == OPENHANDS_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && can_resume_source;
    let expected_sink_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(OutputState {
        source_epoch: if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "OpenHands output source epoch exhausted",
                ))?
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier,
        disposition: if can_resume {
            ProOutputSourceDisposition::AppendOrResume
        } else {
            ProOutputSourceDisposition::Rewrite
        },
    })
}

fn output_observation(
    source: &OpenHandsObservedFile,
    decoded: &OpenHandsDecodedEvent,
    content: String,
) -> ProOutputObservation {
    let outcome = openhands_output_outcome(decoded);
    ProOutputObservation {
        kind: if decoded.event_type() == EventType::CommandOutput {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: decoded.event_id().to_owned(),
            native_sequence: event_identity_index(source, decoded.event_id()),
            native_record_id: Some(decoded.event_id().to_owned()),
            source_record_ordinal: Some(0),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(decoded.timestamp().timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: source.session_id.clone(),
            root_session_id: source.session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(source.session_id.clone()),
            agent_id: source.user_id.clone(),
            repository: None,
        },
        call_id: openhands_output_call_id(decoded.value()),
        command: openhands_output_command_context(decoded),
        outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: OPENHANDS_LOCATOR_KIND.to_owned(),
            payload: source.canonical_path_text.as_bytes().to_vec(),
        },
        content: content.into_bytes(),
    }
}

pub(super) fn openhands_output_outcome(decoded: &OpenHandsDecodedEvent) -> OutputOutcomeMetadata {
    let value = decoded.value();
    let exit_code = [
        "/observation/exit_code",
        "/observation/metadata/exit_code",
        "/exit_code",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_i64))
    .and_then(|value| i32::try_from(value).ok());
    let duration_ms = [
        "/observation/duration_ms",
        "/observation/metadata/duration_ms",
        "/duration_ms",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_u64));
    let timed_out = openhands_value_indicates_timeout(value);
    let classification = provider_result_outcome_evidence(decoded.event_type(), value);
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else {
        match classification.as_str() {
            Some("success") => OutputOutcome::Success,
            Some("failure") => OutputOutcome::Failure,
            _ => OutputOutcome::Unknown,
        }
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn openhands_value_indicates_timeout(value: &Value) -> bool {
    const MAX_NODES: usize = 4_096;

    fn visit(value: &Value, remaining: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match value {
            Value::Array(values) => values.iter().any(|value| visit(value, remaining)),
            Value::Object(values) => values.iter().any(|(key, value)| {
                let normalized = key
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                let direct = matches!(normalized.as_str(), "timeout" | "timedout" | "istimeout")
                    && (value.as_bool().unwrap_or(false)
                        || value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        }));
                direct || visit(value, remaining)
            }),
            Value::String(value) => matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "timeout" | "timed_out" | "timedout"
            ),
            Value::Null | Value::Bool(_) | Value::Number(_) => false,
        }
    }

    let mut remaining = MAX_NODES;
    visit(value, &mut remaining)
}

pub(super) fn openhands_output_call_id(value: &Value) -> Option<String> {
    [
        "/tool_call_id",
        "/action_id",
        "/observation/tool_call_id",
        "/observation/action_id",
        "/observation/command_id",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .filter(|value| valid_output_token(value, 384))
    .map(str::to_owned)
}

pub(super) fn openhands_output_command_context(
    decoded: &OpenHandsDecodedEvent,
) -> Option<OutputCommandContext> {
    if decoded.event_type() != EventType::CommandOutput {
        return None;
    }
    let observation = decoded.value().get("observation")?;
    let tool_name = observation
        .get("kind")
        .and_then(Value::as_str)
        .filter(|value| valid_output_token(value, 256))
        .unwrap_or("command");
    Some(OutputCommandContext {
        tool_name: tool_name.to_owned(),
        command: tool_input::command(observation)?,
        working_directory: tool_input::working_directory(observation),
    })
}

pub(super) fn apply_failure_diagnostic(
    event: &mut OpenHandsEventFact,
    content: Option<&str>,
    outcome: &OutputOutcomeMetadata,
    call_id: Option<&str>,
    command: Option<&OutputCommandContext>,
) -> Result<()> {
    let payload = event
        .payload
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "OpenHands failure event payload must be an object",
        ))?;
    payload.insert("result_outcome".to_owned(), json!("failure"));
    payload.insert(
        "timed_out".to_owned(),
        json!(outcome.outcome == OutputOutcome::Timeout),
    );
    if let Some(exit_code) = outcome.exit_code {
        payload.insert("exit_code".to_owned(), json!(exit_code));
    }
    if let Some(duration_ms) = outcome.duration_ms {
        payload.insert("duration_ms".to_owned(), json!(duration_ms));
    }
    if let Some(call_id) = call_id {
        payload.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
    }
    if let Some(command) = command {
        payload.insert("command".to_owned(), Value::String(command.command.clone()));
        if let Some(working_directory) = command.working_directory.as_ref() {
            payload.insert("cwd".to_owned(), Value::String(working_directory.clone()));
        }
    }
    if let Some(content) = content {
        payload.insert("output_bytes".to_owned(), json!(content.len()));
        let (preview, _) = provider_local_preview(content, PROVIDER_MAX_PREVIEW_CHARS);
        if !preview.trim().is_empty() {
            payload.insert("output_preview".to_owned(), Value::String(preview));
        }
    }
    Ok(())
}

fn valid_output_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

pub(super) fn bounded_failure(mut failure: String) -> String {
    if failure.len() <= OPENHANDS_MAX_FAILURE_BYTES {
        return failure;
    }
    let mut boundary = OPENHANDS_MAX_FAILURE_BYTES;
    while !failure.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    failure.truncate(boundary);
    failure
}

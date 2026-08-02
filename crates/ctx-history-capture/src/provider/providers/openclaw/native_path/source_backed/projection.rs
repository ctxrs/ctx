use super::*;
use crate::provider::providers::openclaw::OpenClawOutputMetadata;
use ctx_history_core::SubrecordSelector;

const TOOL_CALL_SELECTOR_NAMESPACE: &str = "openclaw.tool-call-block";
const TOOL_CALL_POSITION_KIND: &str = "openclaw.tool-call-block-position";
const MAX_SELECTOR_CALL_ID_BYTES: usize = 16 * 1024;

pub(super) enum StateBucket {
    Pending,
    Running,
}

impl OpenClawProjector {
    pub(super) fn remember_state(
        &mut self,
        bucket: StateBucket,
        identity: &str,
        state: PendingCallState,
    ) {
        let capacity = match bucket {
            StateBucket::Pending => MAX_PENDING_CALLS,
            StateBucket::Running => MAX_RUNNING_PROCESSES,
        };
        {
            let states = match bucket {
                StateBucket::Pending => &mut self.pending_calls,
                StateBucket::Running => &mut self.running_processes,
            };
            if let Some(existing) = states.get_mut(identity) {
                *existing = PendingCallState::Ambiguous;
                return;
            }
            if states.len() >= capacity {
                self.linkage_capacity_exceeded = true;
                return;
            }
            states.insert(identity.to_owned(), state);
        }
        if !projector_checkpoint_fits(self) {
            match bucket {
                StateBucket::Pending => self.pending_calls.remove(identity),
                StateBucket::Running => self.running_processes.remove(identity),
            };
            self.linkage_capacity_exceeded = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn project_event(
        &mut self,
        source_bytes: &[u8],
        source_value: &Value,
        event: &normalization::OpenClawEventFact,
        tool_call: Option<&NativeToolCall<'_>>,
        tool_result: Option<&NativeToolResult<'_>>,
        output: Option<&OpenClawOutputMetadata>,
        subrecord: Option<(SubrecordSelector, TypedKey, u64)>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let event_type = if tool_call.is_some() {
            EventType::ToolCall
        } else if output.is_some_and(|output| output.kind == OutputObservationKind::Command) {
            EventType::CommandOutput
        } else {
            event.event_type
        };
        let tool_call_projection = tool_call
            .map(|call| {
                strict_tool_call_projection(
                    call.block,
                    subrecord.as_ref().map_or(0, |(_, _, ordinal)| *ordinal),
                )
            })
            .transpose()?;
        let (body, structured_content) = if let Some(projected) = &tool_call_projection {
            (
                projected.normalized_body.clone(),
                tool_call.map(|call| call.block.clone()),
            )
        } else if let Some(result) = tool_result {
            let projected = project_tool_result(result, output);
            (projected.body, Some(projected.structured_content))
        } else {
            (event.lexical_text.clone(), None)
        };
        if body.trim().is_empty() {
            return Ok(());
        }
        let (native_item_key, mut native_event_key) = native_event_keys(
            event.provider_event_hash.as_deref(),
            source_value,
            event,
            &self.source,
            self.session_id,
            &mut self.fallback_identities,
        )?;
        let (selector, event_subordinal) = match subrecord {
            Some((selector, native_suffix, subordinal)) => {
                native_event_key =
                    TypedKey::composite(vec![native_event_key, native_suffix]).map_err(contract)?;
                (Some(selector), subordinal)
            }
            None => (None, 0),
        };
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: selector.as_ref(),
        })
        .map_err(contract)?;
        let event_sequence = event
            .provider_event_index
            .checked_mul(u64::from(u32::MAX) + 1)
            .and_then(|sequence| sequence.checked_add(event_subordinal))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw event sequence overflowed",
            ))?;
        let mut record = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.session.root_session_id,
            self.source.clone(),
            event_sequence,
            event_type.as_str(),
            self.session.agent_type.as_str(),
            self.session.is_primary,
            PARSER_REVISION,
            body,
        )
        .map_err(contract)?;
        record.parent_session_id = self.session.parent_session_id;
        record.provider_session_id = Some(self.session.provider_session_id.clone());
        record.native_event_id = Some(native_event_key);
        record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
        record.role = event.role.map(|role| role.as_str().to_owned());
        record.branch = self.session.branch.clone();
        record.cwd = self.session.cwd.clone();
        let mut input = AttributionInput {
            activity_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            session_cwd: self.session.cwd.clone(),
            structured_content,
            ..AttributionInput::default()
        };
        if let (Some(call), Some(projected)) = (tool_call, tool_call_projection.as_ref()) {
            self.observe_tool_call(call, projected, event_sequence, &mut input);
        }
        let running = if let (Some(result), Some(output)) = (tool_result, output) {
            self.observe_tool_result(source_bytes, event, result, output, &mut input)
        } else {
            None
        };
        let mut annotation = self.attributor.attribute(input);
        if let Some(abstention) = tool_call_projection
            .as_ref()
            .and_then(|projection| projection.abstention)
        {
            append_invocation_abstention(&mut annotation, abstention);
        }
        apply_annotation(&mut record, annotation);
        record.validate_contract().map_err(contract)?;
        emit(record)?;
        if let Some((process_session_id, context)) = running {
            self.remember_state(
                StateBucket::Running,
                &process_session_id,
                PendingCallState::Exact(context),
            );
        }
        Ok(())
    }

    fn observe_tool_call(
        &mut self,
        call: &NativeToolCall<'_>,
        projected: &StrictToolCallProjection,
        event_sequence: u64,
        input: &mut AttributionInput,
    ) {
        input.command = call.command.clone();
        input.declared_tool_workdir = call.declared_workdir.clone();
        input.repository_file_invocation_evidence = projected.file_invocations.clone();
        input.file_observations = call.file_observations.clone();
        let Some(call_id) = call.call_id.filter(|id| !id.is_empty()) else {
            return;
        };
        let state = match call
            .process_session_id
            .and_then(|session_id| self.running_processes.get(session_id))
            .cloned()
        {
            Some(PendingCallState::Exact(mut pending)) => {
                if pending.continuation_call_id_sha256.len() < 64 {
                    pending
                        .continuation_call_id_sha256
                        .push(Sha256::digest(call_id.as_bytes()).into());
                    PendingCallState::Exact(pending)
                } else {
                    self.linkage_capacity_exceeded = true;
                    PendingCallState::Ambiguous
                }
            }
            Some(PendingCallState::Ambiguous) => PendingCallState::Ambiguous,
            None => PendingCallState::Exact(PendingCall {
                origin_call_id: call_id.to_owned(),
                command: call.command.clone(),
                declared_workdir: call.declared_workdir.clone(),
                event_sequence,
                continuation_call_id_sha256: Vec::new(),
            }),
        };
        if let PendingCallState::Exact(pending) = &state {
            input.command = pending.command.clone();
            input.declared_tool_workdir = pending.declared_workdir.clone();
        }
        self.remember_state(StateBucket::Pending, call_id, state);
    }

    fn observe_tool_result(
        &mut self,
        source_bytes: &[u8],
        event: &normalization::OpenClawEventFact,
        result: &NativeToolResult<'_>,
        output: &OpenClawOutputMetadata,
        input: &mut AttributionInput,
    ) -> Option<(String, PendingCall)> {
        let (context, _linkage_abstained) = resolve_pending_call(
            &mut self.pending_calls,
            result.call_id,
            self.linkage_capacity_exceeded,
            input,
        );
        let context = context?;
        input.command = context.command.clone();
        input.declared_tool_workdir = context.declared_workdir.clone();
        if let Some(process_session_id) = result.running_process_session_id {
            return Some((process_session_id.to_owned(), context));
        }
        if let (Some(command), Some(result_call_id)) = (context.command.as_deref(), result.call_id)
        {
            if let Some(linked) = linked_outcome_evidence(LinkedOutcomeInput {
                provider: "openclaw",
                command,
                session_cwd: self.session.cwd.as_deref(),
                declared_workdir: context.declared_workdir.as_deref(),
                origin_call_id: &context.origin_call_id,
                result_call_id,
                origin_event_sequence: context.event_sequence,
                continuation_call_id_sha256: &context.continuation_call_id_sha256,
                result_record_sha256: Sha256::digest(source_bytes).into(),
                observed_at_unix_ms: event.occurred_at.timestamp_millis(),
                result_outcome: output.outcome.outcome,
                result_output: result.output,
                structured_commit_oid: result.structured_commit_oid,
                output_repository_path: result.output_workdir,
            }) {
                input.provider_native_repository_aliases =
                    linked.provider_native_repository_aliases;
                input.outcome_operation_repository_path = linked.outcome_operation_repository_path;
                input.outcome_output_repository_path = linked.outcome_output_repository_path;
                input.outcome_observations = linked.outcomes;
                input.outcome_abstentions = linked.abstentions;
            }
        }
        None
    }
}

fn append_invocation_abstention(
    annotation: &mut ctx_history_core::CoreRecordAnnotation,
    abstention: StrictInvocationAbstention,
) {
    let (reason, detail) = match abstention {
        StrictInvocationAbstention::Capacity => (
            RepositoryAbstentionReason::CandidateLimitExceeded,
            "openclaw_file_invocation_evidence_overflow",
        ),
        StrictInvocationAbstention::Opaque => (
            RepositoryAbstentionReason::Unsupported,
            "openclaw_file_invocation_schema_not_proven",
        ),
    };
    let abstention = RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::FileActivity,
        reason,
        detail: Some(detail.to_owned()),
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    };
    if !annotation.repository_abstentions.contains(&abstention) {
        annotation.repository_abstentions.push(abstention);
    }
}

struct ProjectedToolResult {
    body: String,
    structured_content: Value,
}

fn project_tool_result(
    result: &NativeToolResult<'_>,
    output: Option<&OpenClawOutputMetadata>,
) -> ProjectedToolResult {
    let message_body = ["content", "text", "output"]
        .into_iter()
        .find_map(|key| result.message.get(key).and_then(explicit_result_text));
    let details_body = openclaw_details_explicit_text(result.message.get("details"));
    let selected_explicit_body = message_body.is_some() || details_body.is_some();
    let body = message_body
        .or(details_body)
        .or_else(|| {
            result
                .message
                .get("details")
                .and_then(provider_explicit_result_value_text)
                .filter(|text| !text.trim().is_empty())
        })
        .unwrap_or_else(|| "tool output".to_owned());
    let call_id = result
        .call_id
        .filter(|value| !value.trim().is_empty() && value.len() <= MAX_SELECTOR_CALL_ID_BYTES);
    let tool_name = ["toolName", "name", "tool_name", "tool"]
        .into_iter()
        .find_map(|key| result.message.get(key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty() && value.len() <= MAX_SELECTOR_CALL_ID_BYTES);
    let mut structured_content = serde_json::json!({
        "type": "tool_result",
        "tool_call_id": call_id,
        "tool_name": tool_name,
        "result_content_location": "normalized_body",
        "result_content_complete": true,
        "outcome": output.map(|output| output_outcome_label(output.outcome.outcome)),
        "exit_code": output.and_then(|output| output.outcome.exit_code),
        "duration_ms": output.and_then(|output| output.outcome.duration_ms),
    });
    if let Some(is_error) = result.message.get("isError").and_then(Value::as_bool) {
        if let Some(object) = structured_content.as_object_mut() {
            object.insert("is_error".to_owned(), Value::Bool(is_error));
        }
    }
    if selected_explicit_body {
        if let (Some(object), Some(metadata)) = (
            structured_content.as_object_mut(),
            openclaw_result_metadata(result.message.get("details")),
        ) {
            object.insert("result_metadata".to_owned(), metadata);
        }
    }
    ProjectedToolResult {
        body,
        structured_content,
    }
}

fn explicit_result_text(value: &Value) -> Option<String> {
    provider_explicit_result_value_text(value).filter(|text| !text.trim().is_empty())
}

fn openclaw_details_explicit_text(details: Option<&Value>) -> Option<String> {
    let details = details?.as_object()?;
    let streams = ["stdout", "stderr"]
        .into_iter()
        .filter_map(|key| details.get(key).and_then(explicit_result_text))
        .collect::<Vec<_>>();
    if !streams.is_empty() {
        return Some(streams.join("\n"));
    }
    ["output", "content", "result", "text"]
        .into_iter()
        .find_map(|key| details.get(key).and_then(explicit_result_text))
}

fn openclaw_result_metadata(details: Option<&Value>) -> Option<Value> {
    let Value::Object(details) = details? else {
        return None;
    };
    let mut metadata = details.clone();
    metadata.retain(|key, _| {
        ![
            "stdout", "stderr", "output", "outputs", "content", "result", "results", "text",
        ]
        .iter()
        .any(|body_key| key.eq_ignore_ascii_case(body_key))
    });
    if metadata.is_empty() {
        return None;
    }
    let metadata = Value::Object(metadata);
    let encoded_len = serde_json::to_vec(&metadata).ok()?.len();
    (encoded_len <= MAX_RESULT_METADATA_BYTES).then_some(metadata)
}

fn output_outcome_label(outcome: crate::OutputOutcome) -> &'static str {
    match outcome {
        crate::OutputOutcome::Success => "success",
        crate::OutputOutcome::Failure => "failure",
        crate::OutputOutcome::Timeout => "timeout",
        crate::OutputOutcome::Unknown => "unknown",
    }
}

impl JsonlFamilyProjector for OpenClawProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if !value.is_object() {
            return Ok(());
        }
        if value.get("type").and_then(Value::as_str) == Some("session") {
            self.session.observe_header(&value);
            return Ok(());
        }
        let evidence = record.evidence();
        let line_number = usize::try_from(evidence.physical_ordinal())
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw line number exceeds platform limits",
            ))?;
        let occurred_at = provider_timestamp_value(value.get("timestamp"), self.session.started_at);
        let event = normalization::event_fact(
            evidence.physical_ordinal(),
            line_number,
            &value,
            occurred_at,
        );
        let tool_calls = native_tool_calls(&value);
        if !tool_calls.is_empty() {
            let mut call_id_counts = HashMap::<&str, usize>::new();
            for call_id in tool_calls.iter().filter_map(|call| call.call_id) {
                *call_id_counts.entry(call_id).or_default() += 1;
            }
            for call in &tool_calls {
                let unique_call_id = call
                    .call_id
                    .is_some_and(|call_id| call_id_counts.get(call_id) == Some(&1));
                let subrecord =
                    tool_call_subrecord(call, unique_call_id, evidence.record_digest())?;
                self.project_event(
                    bytes,
                    &value,
                    &event,
                    Some(call),
                    None,
                    None,
                    Some(subrecord),
                    emit,
                )?;
            }
            return Ok(());
        }
        let tool_result = native_tool_result(&value);
        let output = openclaw_output_metadata(&value);
        self.project_event(
            bytes,
            &value,
            &event,
            None,
            tool_result.as_ref(),
            output.as_ref(),
            None,
            emit,
        )
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(index) = &self.index_file {
            index.revalidate()?;
        }
        self.authority.revalidate()
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        encode_projector_checkpoint(self).map(Some)
    }
}

fn tool_call_subrecord(
    call: &NativeToolCall<'_>,
    unique_call_id: bool,
    record_digest: [u8; 32],
) -> Result<(SubrecordSelector, TypedKey, u64)> {
    let subordinal = u64::try_from(call.block_index).map_err(|_| {
        CaptureError::SystemInvariant("OpenClaw tool-call block index exceeds platform limits")
    })?;
    if let Some(call_id) = call
        .call_id
        .filter(|call_id| unique_call_id && call_id.len() <= MAX_SELECTOR_CALL_ID_BYTES)
    {
        let call_key = TypedKey::utf8(call_id).map_err(contract)?;
        return Ok((
            SubrecordSelector::native_id(TOOL_CALL_SELECTOR_NAMESPACE, call_key.clone())
                .map_err(contract)?,
            TypedKey::composite(vec![
                TypedKey::utf8("tool_call_id").map_err(contract)?,
                call_key,
            ])
            .map_err(contract)?,
            subordinal,
        ));
    }
    let coordinate = TypedKey::U64(subordinal);
    let revision_scope = TypedKey::bytes(record_digest.to_vec()).map_err(contract)?;
    Ok((
        SubrecordSelector::revision_scoped_position(
            TOOL_CALL_POSITION_KIND,
            coordinate.clone(),
            revision_scope.clone(),
        )
        .map_err(contract)?,
        TypedKey::composite(vec![
            TypedKey::utf8("tool_call_position").map_err(contract)?,
            coordinate,
            revision_scope,
        ])
        .map_err(contract)?,
        subordinal,
    ))
}

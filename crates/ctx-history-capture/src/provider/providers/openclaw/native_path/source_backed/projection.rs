use super::*;
use crate::provider::providers::openclaw::OpenClawOutputMetadata;
use ctx_history_core::{EventOrigin, SessionRelationshipKind, SubrecordSelector};

const TOOL_CALL_SELECTOR_NAMESPACE: &str = "openclaw.tool-call-block";
const TOOL_CALL_POSITION_KIND: &str = "openclaw.tool-call-block-position";
const MAX_SELECTOR_CALL_ID_BYTES: usize = 16 * 1024;

pub(super) enum StateBucket {
    Pending,
    Running,
}

impl OpenClawProjector {
    fn tool_call_source_contribution(
        &self,
        source_value: &Value,
        tool_calls: &[NativeToolCall<'_>],
    ) -> ctx_retrieval::ContributionClass {
        let message = source_value.get("message").unwrap_or(source_value);
        let mut contributions = Vec::new();
        if !exact_object_keys(source_value, &["type", "id", "timestamp", "message"])
            || !exact_object_keys(message, &["role", "content"])
        {
            contributions.push(ctx_retrieval::ContributionClass::Unknown);
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            contributions.push(ctx_retrieval::ContributionClass::Unknown);
            return ctx_retrieval::reduce_contributions(contributions);
        };
        for (block_index, block) in content.iter().enumerate() {
            match block.get("type").and_then(Value::as_str) {
                Some("toolCall") => {
                    let contribution = tool_calls
                        .iter()
                        .find(|call| call.block_index == block_index)
                        .map_or(ctx_retrieval::ContributionClass::Unknown, |call| {
                            self.tool_call_contribution(call)
                        });
                    contributions.push(contribution);
                }
                Some("text") => {
                    contributions.push(
                        if exact_object_keys(block, &["type", "text"])
                            && block.get("text").is_some_and(Value::is_string)
                        {
                            ctx_retrieval::ContributionClass::Ordinary
                        } else {
                            ctx_retrieval::ContributionClass::Unknown
                        },
                    );
                }
                _ => contributions.push(ctx_retrieval::ContributionClass::Unknown),
            }
        }
        ctx_retrieval::reduce_contributions(contributions)
    }

    fn tool_call_contribution(
        &self,
        call: &NativeToolCall<'_>,
    ) -> ctx_retrieval::ContributionClass {
        if !exact_object_keys(call.block, &["type", "id", "name", "arguments"])
            || call.call_id.is_none_or(|call_id| call_id.is_empty())
        {
            return ctx_retrieval::ContributionClass::Unknown;
        }
        if let Some(process_session_id) = attested_process_session_id(call) {
            return match self.running_processes.get(process_session_id) {
                Some(PendingCallState::Exact(pending)) => pending.retrieval_contribution.into(),
                Some(PendingCallState::Ambiguous) | None => {
                    ctx_retrieval::ContributionClass::Unknown
                }
            };
        }
        let Some(tool_name) = call.tool_name.filter(|name| !name.trim().is_empty()) else {
            return ctx_retrieval::ContributionClass::Unknown;
        };
        if tool_name == "process" {
            return ctx_retrieval::ContributionClass::Unknown;
        }
        if tool_name != "exec" {
            return ctx_retrieval::ContributionClass::Ordinary;
        }
        let Some(arguments) = call.block.get("arguments") else {
            return ctx_retrieval::ContributionClass::Unknown;
        };
        if !exact_object_keys(arguments, &["command", "workdir", "cwd", "sessionId"]) {
            return ctx_retrieval::ContributionClass::Unknown;
        }
        ctx_retrieval::classify_direct_cli_tool_input(arguments)
    }

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
        source_contribution: Option<ctx_retrieval::ContributionClass>,
        subrecord: Option<(SubrecordSelector, TypedKey, u64)>,
        worker: &mut JsonlFamilyWorkerContext,
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
            self.session_id,
            self.source.clone(),
            event_sequence,
            event_type.as_str(),
            self.session.agent_type.as_str(),
            true,
            PARSER_REVISION,
            body,
        )
        .map_err(contract)?;
        if let Some(parent_session_id) = self.session.parent_session_id {
            record
                .set_session_relationship(
                    self.session.relationship,
                    Some(parent_session_id),
                    self.session.root_session_id,
                )
                .map_err(contract)?;
            if self.session.relationship == SessionRelationshipKind::Delegated {
                record.event_origin = EventOrigin::UniqueToSession;
            }
        }
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
            self.observe_tool_call(
                call,
                projected,
                event_sequence,
                source_contribution.unwrap_or(ctx_retrieval::ContributionClass::Unknown),
                &mut input,
            );
        }
        let (running, result_contribution) =
            if let (Some(result), Some(output)) = (tool_result, output) {
                self.observe_tool_result(
                    source_bytes,
                    source_value,
                    event,
                    result,
                    output,
                    &mut input,
                )
            } else {
                (None, None)
            };
        let mut annotation = worker.repository_attributor().attribute(input);
        if let Some(abstention) = tool_call_projection
            .as_ref()
            .and_then(|projection| projection.abstention)
        {
            append_invocation_abstention(&mut annotation, abstention);
        }
        apply_annotation(&mut record, annotation).map_err(contract)?;
        record.content.discovery_exclusion = ctx_retrieval::discovery_exclusion_for(
            source_contribution.into_iter().chain(result_contribution),
        );
        record
            .content
            .omit_structured_content_if_aggregate_exceeds_limit()
            .map_err(contract)?;
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
        source_contribution: ctx_retrieval::ContributionClass,
        input: &mut AttributionInput,
    ) {
        input.command = call.command.clone();
        input.declared_tool_workdir = call.declared_workdir.clone();
        input.repository_file_invocation_evidence = projected.file_invocations.clone();
        input.file_observations = call.file_observations.clone();
        let Some(call_id) = call.call_id.filter(|id| !id.is_empty()) else {
            return;
        };
        let process_session_id = attested_process_session_id(call);
        let state = match process_session_id
            .and_then(|session_id| self.running_processes.get(session_id))
            .cloned()
        {
            Some(PendingCallState::Exact(mut pending)) => {
                pending.retrieval_contribution = source_contribution.into();
                pending.process_session_id = process_session_id.map(str::to_owned);
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
                process_session_id: None,
                retrieval_contribution: source_contribution.into(),
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
        source_value: &Value,
        event: &normalization::OpenClawEventFact,
        result: &NativeToolResult<'_>,
        output: &OpenClawOutputMetadata,
        input: &mut AttributionInput,
    ) -> (
        Option<(String, PendingCall)>,
        Option<ctx_retrieval::ContributionClass>,
    ) {
        let (context, _linkage_abstained) =
            match result.call_id.filter(|call_id| !call_id.is_empty()) {
                Some(call_id) if !self.terminal_authority.is_unique(call_id) => {
                    self.pending_calls.remove(call_id);
                    input.outcome_abstentions.push((
                        RepositoryAbstentionReason::ProviderOutputUnjoined,
                        "openclaw_tool_result_call_id_is_ambiguous",
                    ));
                    (None, true)
                }
                _ => resolve_pending_call(
                    &mut self.pending_calls,
                    result.call_id,
                    self.linkage_capacity_exceeded,
                    input,
                ),
            };
        let linked_invocation = context
            .as_ref()
            .map(|context| ctx_retrieval::ContributionClass::from(context.retrieval_contribution));
        let result_contribution = if crate::common::json::raw_object_keys_are_unique(source_bytes) {
            classify_openclaw_tool_result(source_value, result, linked_invocation)
        } else {
            ctx_retrieval::ContributionClass::Unknown
        };
        let Some(mut context) = context else {
            return (None, Some(result_contribution));
        };
        input.command = context.command.clone();
        input.declared_tool_workdir = context.declared_workdir.clone();
        if let Some(process_session_id) = result.running_process_session_id {
            if let Some(previous_process_session_id) = context.process_session_id.take() {
                self.running_processes.remove(&previous_process_session_id);
            }
            context.process_session_id = Some(process_session_id.to_owned());
            return (
                Some((process_session_id.to_owned(), context)),
                Some(result_contribution),
            );
        }
        if let Some(process_session_id) = context.process_session_id.take() {
            self.running_processes.remove(&process_session_id);
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
                input.pull_request_associations = linked.pull_request_associations;
                input.outcome_abstentions = linked.abstentions;
            }
        }
        (None, Some(result_contribution))
    }
}

fn attested_process_session_id<'a>(call: &NativeToolCall<'a>) -> Option<&'a str> {
    if call.tool_name != Some("process") {
        return None;
    }
    call.process_session_id
        .filter(|session_id| !session_id.is_empty())
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

fn exact_object_keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn classify_openclaw_tool_result(
    source_value: &Value,
    result: &NativeToolResult<'_>,
    linked_invocation: Option<ctx_retrieval::ContributionClass>,
) -> ctx_retrieval::ContributionClass {
    let mut atoms = vec![ctx_retrieval::ResultAtom::KnownProviderEnvelope];
    if !exact_object_keys(source_value, &["type", "id", "timestamp", "message"])
        || !exact_object_keys(
            result.message,
            &[
                "role",
                "toolCallId",
                "tool_call_id",
                "toolName",
                "tool_name",
                "name",
                "tool",
                "content",
                "details",
                "isError",
                "is_error",
                "success",
                "ok",
                "status",
                "state",
                "outcome",
                "exitCode",
                "exit_code",
                "timedOut",
                "timed_out",
                "timeout",
            ],
        )
    {
        atoms.push(ctx_retrieval::ResultAtom::Unknown);
    }
    let call_id_members = ["toolCallId", "tool_call_id"]
        .into_iter()
        .filter_map(|key| result.message.get(key))
        .collect::<Vec<_>>();
    if call_id_members.len() != 1
        || call_id_members[0]
            .as_str()
            .filter(|call_id| !call_id.is_empty())
            != result.call_id
    {
        atoms.push(ctx_retrieval::ResultAtom::Unknown);
    }

    let mut payload_members = usize::from(result.message.get("content").is_some());
    if result
        .message
        .get("content")
        .is_some_and(has_openclaw_structural_diagnostic)
    {
        atoms.push(ctx_retrieval::ResultAtom::Diagnostic);
    }
    if let Some(details) = result.message.get("details") {
        let Some(details) = details.as_object() else {
            atoms.push(ctx_retrieval::ResultAtom::Unknown);
            return ctx_retrieval::classify_linked_result(
                linked_invocation,
                strict_openclaw_terminal_status(result.message, None),
                atoms,
            );
        };
        const PAYLOAD_KEYS: &[&str] = &["stdout", "output", "content", "result", "text"];
        const KNOWN_DETAIL_KEYS: &[&str] = &[
            "stdout",
            "output",
            "content",
            "result",
            "text",
            "stderr",
            "warning",
            "warnings",
            "error",
            "errors",
            "diagnostic",
            "diagnostics",
            "status",
            "state",
            "outcome",
            "success",
            "ok",
            "isError",
            "is_error",
            "timedOut",
            "timed_out",
            "timeout",
            "exitCode",
            "exit_code",
            "durationMs",
            "duration_ms",
            "sessionId",
            "cwd",
            "commit_oid",
            "commitOid",
        ];
        payload_members += details
            .keys()
            .filter(|key| PAYLOAD_KEYS.contains(&key.as_str()))
            .count();
        if details
            .keys()
            .any(|key| !KNOWN_DETAIL_KEYS.contains(&key.as_str()))
        {
            atoms.push(ctx_retrieval::ResultAtom::Unknown);
        }
        if details.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "stderr"
                    | "warning"
                    | "warnings"
                    | "error"
                    | "errors"
                    | "diagnostic"
                    | "diagnostics"
            ) || (PAYLOAD_KEYS.contains(&key.as_str()) && has_openclaw_structural_diagnostic(value))
        }) {
            atoms.push(ctx_retrieval::ResultAtom::Diagnostic);
        }
    }
    match payload_members {
        1 => atoms.push(ctx_retrieval::ResultAtom::Payload),
        0 => {}
        _ => atoms.push(ctx_retrieval::ResultAtom::Unknown),
    }
    ctx_retrieval::classify_linked_result(
        linked_invocation,
        strict_openclaw_terminal_status(
            result.message,
            result.message.get("details").and_then(Value::as_object),
        ),
        atoms,
    )
}

fn has_openclaw_structural_diagnostic(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(has_openclaw_structural_diagnostic),
        Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "stderr"
                    | "warning"
                    | "warnings"
                    | "error"
                    | "errors"
                    | "diagnostic"
                    | "diagnostics"
            ) || has_openclaw_structural_diagnostic(value)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn strict_openclaw_terminal_status(
    message: &Value,
    details: Option<&serde_json::Map<String, Value>>,
) -> ctx_retrieval::ResultTerminalStatus {
    let mut success = false;
    let mut failure = false;
    let mut unknown = false;
    for object in [message.as_object(), details].into_iter().flatten() {
        for key in ["success", "ok"] {
            if let Some(value) = object.get(key) {
                match value.as_bool() {
                    Some(true) => success = true,
                    Some(false) => failure = true,
                    None => unknown = true,
                }
            }
        }
        for key in ["isError", "is_error"] {
            if let Some(value) = object.get(key) {
                match value.as_bool() {
                    Some(true) => failure = true,
                    Some(false) => {}
                    None => unknown = true,
                }
            }
        }
        for key in ["timedOut", "timed_out", "timeout"] {
            if let Some(value) = object.get(key) {
                match value.as_bool() {
                    Some(true) => failure = true,
                    Some(false) => {}
                    None => unknown = true,
                }
            }
        }
        for key in ["exitCode", "exit_code"] {
            if let Some(value) = object.get(key) {
                match value.as_i64() {
                    Some(0) => success = true,
                    Some(_) => failure = true,
                    None => unknown = true,
                }
            }
        }
        for key in ["status", "state", "outcome"] {
            if let Some(value) = object.get(key) {
                let Some(value) = value.as_str() else {
                    unknown = true;
                    continue;
                };
                match value.trim().to_ascii_lowercase().as_str() {
                    "success" | "succeeded" | "complete" | "completed" | "ok" => success = true,
                    "failed" | "failure" | "error" | "errored" | "timeout" | "timed_out"
                    | "timedout" | "cancelled" | "canceled" => failure = true,
                    "running" | "pending" | "in_progress" => unknown = true,
                    _ => unknown = true,
                }
            }
        }
    }
    if failure {
        ctx_retrieval::ResultTerminalStatus::Failed
    } else if unknown || !success {
        ctx_retrieval::ResultTerminalStatus::Unknown
    } else {
        ctx_retrieval::ResultTerminalStatus::Succeeded
    }
}

impl JsonlFamilyProjector for OpenClawProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        worker: &mut JsonlFamilyWorkerContext,
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
            let source_contribution = if crate::common::json::raw_object_keys_are_unique(bytes) {
                self.tool_call_source_contribution(&value, &tool_calls)
            } else {
                ctx_retrieval::ContributionClass::Unknown
            };
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
                    Some(source_contribution),
                    Some(subrecord),
                    worker,
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
            None,
            worker,
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

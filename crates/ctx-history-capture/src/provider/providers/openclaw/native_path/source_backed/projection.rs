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
        let body = tool_call
            .and_then(|call| serde_json::to_string(call.block).ok())
            .or_else(|| tool_result.and_then(|result| serde_json::to_string(result.message).ok()))
            .unwrap_or_else(|| event.lexical_text.clone());
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
        let structured_content = tool_call
            .map(|call| call.block.clone())
            .or_else(|| tool_result.map(|result| result.message.clone()));
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
        if let Some(call) = tool_call {
            self.observe_tool_call(call, event_sequence, &mut input);
        }
        let running = if let (Some(result), Some(output)) = (tool_result, output) {
            self.observe_tool_result(source_bytes, event, result, output, &mut input)
        } else {
            None
        };
        let annotation = self.attributor.attribute(input);
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
        event_sequence: u64,
        input: &mut AttributionInput,
    ) {
        input.command = call.command.clone();
        input.declared_tool_workdir = call.declared_workdir.clone();
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

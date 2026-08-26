use super::*;
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CoreActivity,
    LiteralFactKind, ProviderDeclaredFact, SubrecordSelector, CORE_ACTIVITY_REVISION,
    MAX_CORE_CONTENT_BYTES,
};
use ctx_history_jsonl::{fit_jsonl_activity, JsonlActivityObservedBytes};

const TOOL_CALL_SELECTOR_NAMESPACE: &str = "openclaw.tool-call-block";
const TOOL_CALL_POSITION_KIND: &str = "openclaw.tool-call-block-position";

impl<R: crate::JsonlProviderRuntime> OpenClawProjector<R> {
    #[allow(clippy::too_many_arguments)]
    fn project_event(
        &mut self,
        source_value: &Value,
        event: &normalization::OpenClawEventFact,
        tool_call: Option<&NativeToolCall<'_>>,
        tool_call_id_is_unique: bool,
        tool_result: Option<&NativeToolResult<'_>>,
        subrecord: Option<(SubrecordSelector, TypedKey, u64)>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let event_type = if tool_call.is_some() {
            EventType::ToolCall
        } else {
            event.event_type
        };
        let body = if let Some(call) = tool_call {
            serde_json::to_string(call.block)?
        } else if let Some(result) = tool_result {
            exact_result_body(result)
        } else {
            event.lexical_text.clone()
        };
        if body.trim().is_empty() {
            return Ok(());
        }
        let (native_item_key, mut native_event_key) = native_event_keys::<R>(
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
        let mut facts = session_facts(&self.session);
        let activity = if let Some(call) = tool_call {
            extend_call_facts(&mut facts, call);
            call_activity(call, tool_call_id_is_unique, facts)?
        } else if let Some(result) = tool_result {
            result_activity(
                result,
                self.terminal_authority
                    .is_unique(TERMINAL_CALL_ID_DOMAIN, result.call_id.unwrap_or_default()),
                facts,
            )?
        } else {
            facts_activity(facts)
        };
        let mut record = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.source.clone(),
            event_sequence,
            event_type.as_str(),
            PARSER_REVISION,
            body.clone(),
        )
        .map_err(contract)?;
        record.parent_session_id = self.session.parent_session_id;
        record.root_session_id = self.session.root_session_id;
        record.session_relationship = self.session.relationship;
        record.agent_scope = self.session.agent_scope;
        record.provider_session_id = Some(self.session.provider_session_id.clone());
        record.native_event_id = Some(native_event_key);
        record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
        record.role = event.role.map(|role| role.as_str().to_owned());
        record.content.structured_content = Some(source_value.clone());
        record.content.activity = activity;
        fit_jsonl_activity(
            &body,
            record.content.structured_content.as_ref(),
            &mut record.content.activity,
            JsonlActivityObservedBytes::infer_from_present(),
            MAX_CORE_CONTENT_BYTES,
        );
        record
            .content
            .omit_provider_declared_facts_if_aggregate_exceeds_limit()
            .map_err(contract)?;
        record
            .content
            .omit_structured_content_if_aggregate_exceeds_limit()
            .map_err(contract)?;
        record.validate_contract().map_err(contract)?;
        emit(record)
    }
}

fn session_facts(session: &SessionState) -> Vec<ProviderDeclaredFact> {
    let mut facts = Vec::new();
    if let Some(cwd) = &session.cwd {
        push_fact(&mut facts, LiteralFactKind::SessionCwd, cwd.clone());
    }
    if let Some(branch) = &session.branch {
        push_fact(&mut facts, LiteralFactKind::Branch, branch.clone());
    }
    facts
}

fn extend_call_facts(facts: &mut Vec<ProviderDeclaredFact>, call: &NativeToolCall<'_>) {
    if let Some(command) = &call.command {
        push_fact(facts, LiteralFactKind::Command, command.clone());
    }
    if let Some(workdir) = &call.declared_workdir {
        push_fact(facts, LiteralFactKind::ToolWorkdir, workdir.clone());
    }
    for path in &call.file_references {
        push_fact(facts, LiteralFactKind::File, path.clone());
    }
}

fn push_fact(facts: &mut Vec<ProviderDeclaredFact>, kind: LiteralFactKind, value: String) {
    if let Some(fact) = admit_provider_declared_fact(kind, value, facts.len()) {
        facts.push(fact);
    }
}

fn call_activity(
    call: &NativeToolCall<'_>,
    call_id_is_unique: bool,
    facts: Vec<ProviderDeclaredFact>,
) -> Result<Option<CoreActivity>> {
    if call.call_id.is_some_and(|call_id| !call_id.is_empty()) && !call_id_is_unique {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw tool call ID is ambiguous within the source".to_owned(),
        ));
    }
    let provider_call_id = admit_optional_provider_call_id(
        call.call_id
            .filter(|_| call_id_is_unique)
            .map(str::to_owned),
    );
    let invocation = provider_call_id.as_ref().and_then(|_| {
        admit_optional_metadata_text(call.tool_name.map(str::to_owned)).map(|tool| {
            ActivityInvocation {
                protocol: None,
                server: None,
                tool,
                arguments: call.block.get("arguments").map_or(
                    ActivityJsonCapture::Absent,
                    |value| ActivityJsonCapture::Present {
                        value: value.clone(),
                    },
                ),
                started_at_unix_ms: None,
            }
        })
    });
    Ok(admitted_activity(provider_call_id, invocation, None, facts))
}

fn result_activity(
    result: &NativeToolResult<'_>,
    call_id_is_unique: bool,
    facts: Vec<ProviderDeclaredFact>,
) -> Result<Option<CoreActivity>> {
    if result.call_id.is_some_and(|call_id| !call_id.is_empty())
        && (!call_id_is_unique || result.ambiguous_linkage)
    {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw tool result call ID is ambiguous within the source".to_owned(),
        ));
    }
    let provider_call_id = admit_optional_provider_call_id(
        result
            .call_id
            .filter(|_| call_id_is_unique && !result.ambiguous_linkage)
            .map(str::to_owned),
    );
    let activity_result = provider_call_id.as_ref().map(|_| ActivityResult {
        status: None,
        completed_at_unix_ms: None,
        duration_ns: None,
        text: ActivityTextCapture::NormalizedBody,
        structured_content: ActivityJsonCapture::Present {
            value: result.output.clone(),
        },
    });
    Ok(admitted_activity(
        provider_call_id,
        None,
        activity_result,
        facts,
    ))
}

fn facts_activity(facts: Vec<ProviderDeclaredFact>) -> Option<CoreActivity> {
    admitted_activity(None, None, None, facts)
}

fn admitted_activity(
    provider_call_id: Option<TypedKey>,
    invocation: Option<ActivityInvocation>,
    result: Option<ActivityResult>,
    facts: Vec<ProviderDeclaredFact>,
) -> Option<CoreActivity> {
    (invocation.is_some() || result.is_some() || !facts.is_empty()).then_some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    })
}

fn exact_result_body(result: &NativeToolResult<'_>) -> String {
    ["content", "text", "output"]
        .into_iter()
        .find_map(|key| result.message.get(key).and_then(explicit_result_text))
        .or_else(|| provider_explicit_result_value_text(result.output))
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| serde_json::to_string(result.output).unwrap_or_default())
}

fn explicit_result_text(value: &Value) -> Option<String> {
    provider_explicit_result_value_text(value).filter(|text| !text.trim().is_empty())
}

impl<R: crate::JsonlProviderRuntime> JsonlFamilyProjector for OpenClawProjector<R> {
    type Runtime = R;

    fn preflight(
        &mut self,
        reader: &mut JsonlReader,
        certified_prefix_end: Option<u64>,
    ) -> Result<bool> {
        let mut authority = OpenClawTerminalAuthority::available();
        let mut certified_prefix_exhausted = false;
        while reader
            .visit_page(&mut |record: JsonlRecordRef<'_>| -> Result<()> {
                let evidence = record.evidence();
                let region = match certified_prefix_end {
                    Some(end) if evidence.byte_end_exclusive() <= end => {
                        JsonlTerminalObservationRegion::CertifiedPrefix
                    }
                    Some(_) => JsonlTerminalObservationRegion::AppendedSuffix,
                    None => JsonlTerminalObservationRegion::WholeSource,
                };
                observe_terminal_record(&mut authority, record.bytes(), region);
                if region == JsonlTerminalObservationRegion::CertifiedPrefix {
                    certified_prefix_exhausted = authority.exhausted();
                }
                Ok(())
            })?
            .is_some()
        {}
        self.terminal_authority = authority;
        Ok(certified_prefix_end.is_some()
            && self.terminal_authority.append_requires_replacement()
            && (!self.terminal_authority.exhausted() || !certified_prefix_exhausted))
    }

    fn retry_replacement(&mut self) {
        self.session.restore(self.replacement_session.clone());
        self.fallback_identities = FallbackEventIdentityState::<R>::default();
    }

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if record.oversized() {
            self.rejections.malformed(
                record,
                format!(
                    "OpenClaw record exceeds the {} byte limit",
                    crate::MAX_PROVIDER_JSONL_LINE_BYTES
                ),
            );
            return Ok(());
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                self.rejections
                    .malformed(record, format!("malformed OpenClaw JSONL: {error}"));
                return Ok(());
            }
        };
        if !value.is_object() {
            self.rejections.record(
                record,
                SourceBackedRecordRejectionClass::UnsupportedRecord,
                "OpenClaw record has a well-formed but unsupported shape",
            );
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
            for call in &tool_calls {
                let unique_call_id = call.call_id.is_some_and(|call_id| {
                    self.terminal_authority
                        .is_unique(TERMINAL_INVOCATION_ID_DOMAIN, call_id)
                });
                let subrecord =
                    tool_call_subrecord(call, unique_call_id, evidence.record_digest())?;
                self.project_event(
                    &value,
                    &event,
                    Some(call),
                    unique_call_id,
                    None,
                    Some(subrecord),
                    emit,
                )?;
            }
            return Ok(());
        }
        let tool_result = native_tool_result(&value);
        self.project_event(
            &value,
            &event,
            None,
            false,
            tool_result.as_ref(),
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

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
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
    if let Some(call_key) = admit_optional_provider_call_id(
        call.call_id
            .filter(|call_id| unique_call_id && call_id.len() <= MAX_SELECTOR_CALL_ID_BYTES)
            .map(str::to_owned),
    ) {
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

use super::*;

impl DirectJsonlProjector {
    pub(super) fn project_line(
        &mut self,
        bytes: &[u8],
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        record_digest: [u8; 32],
    ) -> Result<ProjectedLine> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(ProjectedLine::default());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                    raw_ordinal: ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason: format!(
                        "{}:{} malformed JSONL: {error}",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
        };

        let starts_session = match self.provider {
            CaptureProvider::Qoder => {
                super::qoder_parser::qoder_header_session_id(&value).is_some()
            }
            CaptureProvider::QwenCode => {
                super::qwen_code::qwen_code_header_session_id(&value).is_some()
            }
            _ => native_jsonl_record_starts_session(self.provider, &value),
        };
        if self.session.is_none() {
            if !starts_session {
                return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                    raw_ordinal: ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason: format!(
                        "{}:{}: record appeared before an importable native JSONL session header",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
            self.session = Some(session_from_header(
                self.provider,
                &self.source_format,
                &self.path,
                self.source_root.as_deref(),
                self.imported_at,
                &value,
            ));
        } else if starts_session {
            let observed = session_from_header(
                self.provider,
                &self.source_format,
                &self.path,
                self.source_root.as_deref(),
                self.imported_at,
                &value,
            );
            let expected = self.session.as_ref().ok_or(CaptureError::SystemInvariant(
                "direct JSONL reader lost its provider session",
            ))?;
            if observed.native_session_id != expected.native_session_id
                || observed.provider_session_id != expected.provider_session_id
                || observed.parent_provider_session_id != expected.parent_provider_session_id
                || observed.root_provider_session_id != expected.root_provider_session_id
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
        }
        let session = self.session.as_ref().ok_or(CaptureError::SystemInvariant(
            "direct JSONL reader lost its provider session",
        ))?;
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "direct JSONL line number exceeds platform limits",
            ))?;
        let event_type = direct_jsonl_event_type(self.provider, &value);
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(session.started_at);

        if event_type == EventType::ToolOutput
            && matches!(
                self.provider,
                CaptureProvider::FactoryAiDroid
                    | CaptureProvider::Qoder
                    | CaptureProvider::QwenCode
            )
        {
            return self.project_result_line(
                None,
                &value,
                bytes,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                occurred_at,
                record_digest,
            );
        }
        let result_profile = (event_type == EventType::ToolOutput)
            .then(|| native_jsonl_result_content_profile(self.provider))
            .flatten();
        if let Some(profile) = result_profile {
            return self.project_result_line(
                Some(profile),
                &value,
                bytes,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                occurred_at,
                record_digest,
            );
        }

        let touches = direct_jsonl_touches(&value, event_type, false);
        let generic_tool_call_body = self.provider == CaptureProvider::CopilotCli
            && event_type == EventType::ToolCall
            && super::copilot::copilot_start_requires_generic_body(bytes);
        let mcp_exchange = (self.provider == CaptureProvider::CopilotCli)
            .then(|| super::copilot::copilot_mcp_exchange(bytes, None))
            .flatten();
        let fallback_identity_discriminator = mcp_exchange
            .as_ref()
            .filter(|exchange| exchange.invocation.is_some())
            .map(|exchange| {
                crate::compute_payload_hash(&json!({
                    "domain": "ctx.copilot-mcp-invocation-fallback-v1",
                    "provider_call_id": exchange.provider_call_id,
                    "invocation": exchange.invocation,
                }))
            })
            .transpose()?;
        let mut event = direct_event(
            self.provider,
            &self.source_format,
            &value,
            ordinal,
            0,
            line_number,
            occurred_at,
            None,
            touches,
            generic_tool_call_body,
            fallback_identity_discriminator.as_deref(),
        )?;
        event.mcp_exchange = mcp_exchange;
        event.source_record = DirectJsonlSourceRecord {
            byte_start,
            byte_end_exclusive,
            record_digest,
        };
        Ok(ProjectedLine::event(event))
    }

    #[allow(clippy::too_many_arguments)]
    fn project_result_line(
        &self,
        profile: Option<&str>,
        value: &Value,
        record_bytes: &[u8],
        ordinal: u64,
        line_number: usize,
        byte_start: u64,
        byte_end_exclusive: u64,
        occurred_at: DateTime<Utc>,
        record_digest: [u8; 32],
    ) -> Result<ProjectedLine> {
        let extracted = match self.provider {
            CaptureProvider::FactoryAiDroid => super::enumerate_factory_droid_results(value),
            CaptureProvider::Qoder => super::qoder_parser::enumerate_qoder_results(value),
            CaptureProvider::QwenCode => super::qwen_code::enumerate_qwen_code_results(value),
            _ => {
                let Some(profile) = profile else {
                    return Err(CaptureError::SystemInvariant(
                        "direct JSONL result reader has no provider parser",
                    ));
                };
                enumerate_native_jsonl_result_subrecords(profile, value)
            }
        };
        let subrecords = match extracted {
            Ok(subrecords) => subrecords,
            Err(NativeJsonlResultExtractionError::Redacted) => {
                return Ok(ProjectedLine::default());
            }
            Err(NativeJsonlResultExtractionError::InvalidShape) => {
                return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                    raw_ordinal: ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason: format!(
                        "{}:{} native result record has an invalid shape",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
            Err(NativeJsonlResultExtractionError::UnsupportedProfile) => {
                return Err(CaptureError::SystemInvariant(
                    "direct JSONL result reader used an invalid provider profile",
                ));
            }
        };
        let mut projected = ProjectedLine::default();
        let has_retained_failure = subrecords.iter().any(|subrecord| {
            subrecord
                .content
                .as_deref()
                .is_some_and(|content| !content.trim().is_empty())
                && matches!(
                    subrecord.outcome.outcome,
                    OutputOutcome::Failure | OutputOutcome::Timeout
                )
        });
        let retained_failure_touches = if has_retained_failure {
            direct_jsonl_touches(value, EventType::ToolOutput, true)
        } else {
            Vec::new()
        };
        for subrecord in subrecords {
            let content = subrecord
                .content
                .as_deref()
                .filter(|content| !content.trim().is_empty());
            let retain_contentless_copilot_completion =
                self.provider == CaptureProvider::CopilotCli && subrecord.call_id.is_some();
            if content.is_none() && !retain_contentless_copilot_completion {
                continue;
            }
            let sub_ordinal = subrecord.subrecord_index;
            let touches = if matches!(
                subrecord.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            ) {
                retained_failure_touches.clone()
            } else {
                Vec::new()
            };
            debug_assert!(content.is_some() || retain_contentless_copilot_completion);
            let mut event = direct_event(
                self.provider,
                &self.source_format,
                value,
                ordinal,
                sub_ordinal,
                line_number,
                occurred_at,
                Some(&subrecord),
                touches,
                false,
                None,
            )?;
            if self.provider == CaptureProvider::CopilotCli {
                event.mcp_tool_call = self
                    .copilot_mcp_tool_calls
                    .attribution_for_projected_completion(ordinal, record_digest);
                event.mcp_exchange = super::copilot::copilot_mcp_exchange(
                    record_bytes,
                    event.mcp_tool_call.as_ref(),
                );
            }
            event.source_record = DirectJsonlSourceRecord {
                byte_start,
                byte_end_exclusive,
                record_digest,
            };
            projected.events.push(event);
        }
        Ok(projected)
    }
}

fn direct_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_type(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_type(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_type(value),
        _ => native_jsonl_event_type(provider, value),
    }
}

fn direct_jsonl_role(provider: CaptureProvider, value: &Value) -> ctx_history_core::EventRole {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_role(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_role(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_role(value),
        _ => native_jsonl_role(provider, value),
    }
}

fn direct_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    event_type: EventType,
    entry_type: &str,
) -> String {
    if event_type == EventType::ToolCall {
        if let Some(text) = direct_jsonl_structured_tool_call_text(provider, value) {
            return text;
        }
    }
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_text(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_text(value, event_type),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_text(value),
        _ => native_jsonl_event_text(provider, value, event_type, entry_type),
    }
}

fn direct_jsonl_structured_tool_call_text(
    provider: CaptureProvider,
    value: &Value,
) -> Option<String> {
    let selected = match provider {
        CaptureProvider::Antigravity => {
            direct_jsonl_selected_object(value, &["content", "thinking", "tool_calls"])
        }
        CaptureProvider::Tabnine => direct_jsonl_selected_object(value, &["content", "toolCalls"]),
        CaptureProvider::CopilotCli => value.get("data").cloned(),
        CaptureProvider::FactoryAiDroid | CaptureProvider::Qoder | CaptureProvider::QwenCode => {
            value
                .pointer("/message/content")
                .or_else(|| value.get("content"))
                .cloned()
        }
        CaptureProvider::Windsurf => value.get("code_action").cloned(),
        _ => None,
    }?;
    Some(selected.to_string())
}

fn direct_jsonl_selected_object(value: &Value, fields: &[&str]) -> Option<Value> {
    let selected = fields
        .iter()
        .filter_map(|field| {
            value
                .get(*field)
                .map(|selected| ((*field).to_owned(), selected.clone()))
        })
        .collect::<serde_json::Map<_, _>>();
    (!selected.is_empty()).then_some(Value::Object(selected))
}

fn direct_jsonl_lexical_text(
    event_type: EventType,
    text: &str,
    result: Option<&super::result_content::NativeJsonlResultSubrecord<'_>>,
) -> String {
    if let Some(result) = result {
        return result.content.as_deref().unwrap_or_default().to_owned();
    }
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text.to_owned()
    }
}

fn direct_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_model(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_model(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_model(value),
        _ => native_jsonl_model(provider, value),
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectedLine {
    pub(crate) events: Vec<DirectJsonlEvent>,
    pub(crate) rejections: Vec<DirectJsonlRejection>,
}

impl ProjectedLine {
    fn event(event: DirectJsonlEvent) -> Self {
        Self {
            events: vec![event],
            ..Self::default()
        }
    }

    pub(super) fn rejection(rejection: DirectJsonlRejection) -> Self {
        Self {
            rejections: vec![rejection],
            ..Self::default()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn direct_event(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    raw_ordinal: u64,
    sub_ordinal: u32,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    result: Option<&super::result_content::NativeJsonlResultSubrecord<'_>>,
    touches: Vec<DirectJsonlTouch>,
    generic_tool_call_body: bool,
    fallback_identity_discriminator: Option<&str>,
) -> Result<DirectJsonlEvent> {
    let event_type = direct_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = direct_jsonl_role(provider, value);
    let text = if generic_tool_call_body || event_type == EventType::ToolOutput {
        String::new()
    } else {
        direct_jsonl_event_text(provider, value, event_type, &entry_type)
    };
    let mut lexical_text = direct_jsonl_lexical_text(event_type, &text, result);
    if provider == CaptureProvider::CopilotCli && result.is_some() && lexical_text.trim().is_empty()
    {
        lexical_text = event_type.as_str().to_owned();
    }
    if result.is_some() && lexical_text.trim().is_empty() {
        return Err(CaptureError::InvalidPayload(
            "direct JSONL result record has no meaningful selected content".to_owned(),
        ));
    }
    let tool_result = result.map(|result| {
        json!({
            "call_id": result.call_id,
            "tool_name": result.tool_name,
            "outcome": direct_jsonl_outcome(result.outcome.outcome),
            "exit_code": result.outcome.exit_code,
            "duration_ms": result.outcome.duration_ms,
        })
    });

    let positional_event_index = direct_jsonl_event_sequence(raw_ordinal, sub_ordinal)?;
    let native_record_id = direct_jsonl_native_event_identity(provider, value);
    let stable_retry_discriminator = match provider {
        CaptureProvider::FactoryAiDroid if result.is_some() => {
            factory_droid_retry_discriminator(value, sub_ordinal).map_err(|_| {
                CaptureError::SystemInvariant(
                    "validated Factory Droid retry result lost its native discriminator",
                )
            })?
        }
        _ => None,
    };
    let provider_event_sequence_index = positional_event_index;
    let mut provider_event_hash_input = json!({
        "event_type": event_type.as_str(),
        "role": role.as_str(),
        "native_record_id": native_record_id,
        "stable_retry_discriminator": stable_retry_discriminator,
        "sub_ordinal": sub_ordinal,
        "lexical_text": lexical_text,
        "tool_result": tool_result.clone(),
        "touches": touches,
    });
    if native_record_id.is_none() {
        if let Some(discriminator) = fallback_identity_discriminator {
            provider_event_hash_input
                .as_object_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "direct JSONL fallback hash input is not an object",
                ))?
                .insert(
                    "fallback_identity_discriminator".to_owned(),
                    Value::String(discriminator.to_owned()),
                );
        }
    }
    let provider_event_hash = crate::compute_payload_hash(&provider_event_hash_input)?;
    Ok(DirectJsonlEvent {
        raw_ordinal,
        sub_ordinal,
        native_record_id,
        stable_retry_discriminator,
        provider_event_sequence_index,
        provider_event_hash,
        event_type,
        role,
        occurred_at,
        mcp_tool_call: None,
        mcp_exchange: None,
        lexical_text,
        metadata: json!({
            "source": source_format,
            "source_format": source_format,
            "line": line_number,
            "entry_type": entry_type,
            "status": value.get("status").and_then(Value::as_str),
            "model": direct_jsonl_model(provider, value),
            "tokens": native_jsonl_tokens(provider, value),
            "source_record_ordinal": raw_ordinal,
            "source_record_subrecord_index": sub_ordinal,
            "tool_result": tool_result,
        }),
        touches,
        source_record: DirectJsonlSourceRecord::default(),
    })
}

fn direct_jsonl_outcome(outcome: OutputOutcome) -> &'static str {
    match outcome {
        OutputOutcome::Success => "success",
        OutputOutcome::Failure => "failure",
        OutputOutcome::Timeout => "timeout",
        OutputOutcome::Unknown => "unknown",
    }
}

fn direct_jsonl_event_sequence(raw_ordinal: u64, sub_ordinal: u32) -> Result<u64> {
    const SUBRECORD_STRIDE: u64 = 65_536;
    const SUBRECORD_SEQUENCE_BASE: u64 = 1_u64 << 62;

    if sub_ordinal == 0 {
        return i64::try_from(raw_ordinal)
            .map(|_| raw_ordinal)
            .map_err(|_| {
                CaptureError::SystemInvariant("direct JSONL provider event sequence overflowed")
            });
    }
    let sub_ordinal = u64::from(sub_ordinal);
    if sub_ordinal >= SUBRECORD_STRIDE {
        return Err(CaptureError::SystemInvariant(
            "direct JSONL provider event has too many subrecords",
        ));
    }
    raw_ordinal
        .checked_mul(SUBRECORD_STRIDE)
        .and_then(|index| index.checked_add(sub_ordinal))
        .filter(|index| *index < SUBRECORD_SEQUENCE_BASE)
        .map(|index| index | SUBRECORD_SEQUENCE_BASE)
        .ok_or(CaptureError::SystemInvariant(
            "direct JSONL provider event sequence overflowed",
        ))
}

fn direct_jsonl_touches(
    value: &Value,
    event_type: EventType,
    retained_failure: bool,
) -> Vec<DirectJsonlTouch> {
    if event_type == EventType::ToolOutput && !retained_failure {
        return Vec::new();
    }
    let mut touches = Vec::new();
    let mut seen = BTreeSet::new();
    visit_all_file_touch_drafts(value, |draft| {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if seen.insert(key) {
            touches.push(DirectJsonlTouch {
                path: draft.path,
                old_path: draft.old_path,
                change_kind: draft.change_kind,
            });
        }
        Ok::<_, std::convert::Infallible>(())
    })
    .expect("direct JSONL file-touch collection is infallible");
    touches
}

fn direct_jsonl_native_event_identity(provider: CaptureProvider, value: &Value) -> Option<String> {
    match provider {
        CaptureProvider::Antigravity => value
            .get("step_index")
            .and_then(Value::as_u64)
            .map(|step| format!("step-{step}"))
            .or_else(|| generic_native_event_identity(value)),
        CaptureProvider::CopilotCli => {
            super::copilot::copilot_event_identity(value).map(str::to_owned)
        }
        CaptureProvider::FactoryAiDroid => {
            super::factory_droid_event_identity(value).map(str::to_owned)
        }
        CaptureProvider::Qoder => {
            super::qoder_parser::qoder_event_identity(value).map(str::to_owned)
        }
        CaptureProvider::QwenCode => {
            super::qwen_code::qwen_code_event_identity(value).map(str::to_owned)
        }
        CaptureProvider::Tabnine => {
            super::tabnine::tabnine_event_identity(value).map(str::to_owned)
        }
        CaptureProvider::Windsurf => generic_native_event_identity(value),
        _ => None,
    }
}

fn generic_native_event_identity(value: &Value) -> Option<String> {
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
        .map(str::to_owned)
}

fn session_from_header(
    provider: CaptureProvider,
    source_format: &str,
    path: &Path,
    _source_root: Option<&Path>,
    imported_at: DateTime<Utc>,
    header: &Value,
) -> DirectJsonlSession {
    let native_session_id = match provider {
        CaptureProvider::Antigravity => {
            antigravity_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
        }
        CaptureProvider::Windsurf => super::windsurf::windsurf_session_id_from_path(path)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        CaptureProvider::Qoder => super::qoder_parser::qoder_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        _ => native_jsonl_header_session_id(provider, header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
    };
    let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
        native_jsonl_path_session(provider, path, header, &native_session_id);
    let started_at = native_jsonl_timestamp(header)
        .or_else(|| native_jsonl_header_start_time(provider, header))
        .unwrap_or(imported_at);
    let cwd = match provider {
        CaptureProvider::Qoder => super::qoder_parser::qoder_header_cwd(header),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_header_cwd(header),
        _ => native_jsonl_header_cwd(provider, header),
    };
    let metadata = native_jsonl_session_metadata_from_normalized_header(
        provider,
        source_format,
        &super::normalization::native_jsonl_normalized_header_metadata(header),
        path,
    );
    let is_subagent =
        parent_provider_session_id.is_some() || agent_type == ctx_history_core::AgentType::Subagent;
    DirectJsonlSession {
        native_session_id,
        provider_session_id,
        root_provider_session_id: parent_provider_session_id.clone(),
        parent_provider_session_id,
        external_agent_id,
        agent_type,
        role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
        is_primary: !is_subagent,
        status: native_jsonl_session_status(provider, header),
        started_at,
        ended_at: None,
        cwd,
        metadata,
    }
}

#[cfg(test)]
mod event_sequence_tests {
    use super::direct_jsonl_event_sequence;

    #[test]
    fn direct_jsonl_event_sequence_preserves_record_and_subrecord_order_in_sqlite_range() {
        let first = direct_jsonl_event_sequence(1, 0).unwrap();
        let first_result = direct_jsonl_event_sequence(1, 1).unwrap();
        let last_first_result = direct_jsonl_event_sequence(1, u16::MAX.into()).unwrap();
        let second = direct_jsonl_event_sequence(2, 0).unwrap();
        let second_result = direct_jsonl_event_sequence(2, 1).unwrap();

        assert!(first < second);
        assert!(second < first_result);
        assert!(first_result < last_first_result);
        assert!(last_first_result < second_result);
        assert!(second_result <= i64::MAX as u64);
    }

    #[test]
    fn direct_jsonl_event_sequence_rejects_unrepresentable_coordinates() {
        assert!(direct_jsonl_event_sequence(1, u32::from(u16::MAX) + 1).is_err());
        assert!(direct_jsonl_event_sequence(i64::MAX as u64 + 1, 0).is_err());
        assert!(direct_jsonl_event_sequence((1_u64 << 46) + 1, 1).is_err());
    }
}

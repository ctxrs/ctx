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

        let observed_grok_session_id = (self.provider == CaptureProvider::GrokBuild)
            .then(|| super::grok_build::grok_build_header_session_id(&value))
            .flatten();
        let starts_session = match self.provider {
            CaptureProvider::GrokBuild => observed_grok_session_id.is_some(),
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
        } else if self.provider == CaptureProvider::GrokBuild && observed_grok_session_id.is_none()
        {
            return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                raw_ordinal: ordinal,
                byte_start,
                byte_end_exclusive,
                reason: format!(
                    "{}:{} Grok Build record is missing a nonempty sessionId",
                    self.path.display(),
                    ordinal.saturating_add(1)
                ),
            }));
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
        let occurred_at = if self.provider == CaptureProvider::GrokBuild {
            super::grok_build::grok_build_timestamp(&value)
        } else {
            native_jsonl_timestamp(&value)
        }
        .unwrap_or(session.started_at);

        if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput)
            && matches!(
                self.provider,
                CaptureProvider::FactoryAiDroid
                    | CaptureProvider::GrokBuild
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
        let result_profile = matches!(event_type, EventType::ToolOutput | EventType::CommandOutput)
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

        let facts = direct_jsonl_facts(&value);
        let generic_tool_call_body = self.provider == CaptureProvider::CopilotCli
            && event_type == EventType::ToolCall
            && super::copilot::copilot_start_requires_generic_body(bytes);
        let activity = (self.provider == CaptureProvider::CopilotCli)
            .then(|| super::copilot::copilot_activity(bytes))
            .flatten();
        let fallback_identity_discriminator = activity
            .as_ref()
            .filter(|activity| activity.invocation.is_some())
            .map(|activity| {
                crate::compute_payload_hash(&json!({
                    "domain": "ctx.copilot-activity-invocation-fallback-v1",
                    "provider_call_id": activity.provider_call_id,
                    "invocation": activity.invocation,
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
            facts,
            generic_tool_call_body,
            fallback_identity_discriminator.as_deref(),
        )?;
        event.activity = activity;
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
            CaptureProvider::GrokBuild => super::grok_build::enumerate_grok_build_results(value),
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
        for subrecord in subrecords {
            let content = subrecord
                .content
                .as_deref()
                .filter(|content| !content.trim().is_empty());
            let retain_contentless_completion = matches!(
                self.provider,
                CaptureProvider::CopilotCli | CaptureProvider::GrokBuild
            ) && subrecord.call_id.is_some();
            let contentless_grok_completion =
                self.provider == CaptureProvider::GrokBuild && content.is_none();
            if content.is_none() && !retain_contentless_completion {
                continue;
            }
            let sub_ordinal = subrecord.subrecord_index;
            let facts = if contentless_grok_completion {
                Vec::new()
            } else {
                direct_jsonl_facts(value)
            };
            debug_assert!(content.is_some() || retain_contentless_completion);
            let mut event = direct_event(
                self.provider,
                &self.source_format,
                value,
                ordinal,
                sub_ordinal,
                line_number,
                occurred_at,
                Some(&subrecord),
                facts,
                false,
                None,
            )?;
            if contentless_grok_completion {
                event.native_value =
                    super::grok_build::grok_build_contentless_result_evidence(value);
            }
            event.activity = if self.provider == CaptureProvider::CopilotCli {
                super::copilot::copilot_activity(record_bytes)
                    .or(native_result_activity(&subrecord, &event.native_value)?)
            } else {
                native_result_activity(&subrecord, &event.native_value)?
            };
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

fn native_result_activity(
    subrecord: &NativeJsonlResultSubrecord<'_>,
    native_value: &Value,
) -> Result<Option<CoreActivity>> {
    let Some(provider_call_id) =
        admit_optional_provider_call_id(subrecord.call_id.map(str::to_owned))
    else {
        return Ok(None);
    };
    let text = if subrecord.content.is_some() {
        ActivityTextCapture::NormalizedBody
    } else {
        ActivityTextCapture::Absent
    };
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(provider_call_id),
        invocation: None,
        result: Some(ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text,
            structured_content: ActivityJsonCapture::Present {
                value: native_value.clone(),
            },
        }),
        facts: Vec::new(),
    }))
}

fn direct_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_type(value),
        CaptureProvider::GrokBuild => super::grok_build::grok_build_event_type(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_type(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_type(value),
        _ => native_jsonl_event_type(provider, value),
    }
}

fn direct_jsonl_role(provider: CaptureProvider, value: &Value) -> ctx_history_core::EventRole {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_role(value),
        CaptureProvider::GrokBuild => super::grok_build::grok_build_role(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_role(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_role(value),
        _ => native_jsonl_role(provider, value),
    }
}

fn direct_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    event_type: EventType,
) -> String {
    if event_type == EventType::ToolCall {
        if let Some(text) = direct_jsonl_structured_tool_call_text(provider, value) {
            return text;
        }
    }
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_text(value),
        CaptureProvider::GrokBuild => super::grok_build::grok_build_event_text(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_text(value, event_type),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_text(value),
        _ => native_jsonl_event_text(provider, value, event_type),
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
        CaptureProvider::GrokBuild => {
            return super::grok_build::grok_build_structured_tool_call_text(value)
        }
        CaptureProvider::FactoryAiDroid | CaptureProvider::Qoder | CaptureProvider::QwenCode => {
            value
                .pointer("/message/content")
                .or_else(|| value.get("content"))
                .cloned()
        }
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
        CaptureProvider::GrokBuild => None,
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
    facts: Vec<ctx_history_core::ProviderDeclaredFact>,
    generic_tool_call_body: bool,
    fallback_identity_discriminator: Option<&str>,
) -> Result<DirectJsonlEvent> {
    let event_type = direct_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = direct_jsonl_role(provider, value);
    let text = if generic_tool_call_body
        || matches!(event_type, EventType::ToolOutput | EventType::CommandOutput)
    {
        String::new()
    } else {
        direct_jsonl_event_text(provider, value, event_type)
    };
    let mut lexical_text = direct_jsonl_lexical_text(event_type, &text, result);
    if matches!(
        provider,
        CaptureProvider::CopilotCli | CaptureProvider::GrokBuild
    ) && result.is_some()
        && lexical_text.trim().is_empty()
    {
        lexical_text = event_type.as_str().to_owned();
    }
    if result.is_some() && lexical_text.trim().is_empty() {
        return Err(CaptureError::InvalidPayload(
            "direct JSONL result record has no meaningful selected content".to_owned(),
        ));
    }
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
        "native_value": value,
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
        activity: None,
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
        }),
        facts,
        native_value: value.clone(),
        source_record: DirectJsonlSourceRecord::default(),
    })
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

fn direct_jsonl_facts(value: &Value) -> Vec<ctx_history_core::ProviderDeclaredFact> {
    let mut facts = Vec::new();
    visit_literal_file_reference_drafts(value, |draft| {
        if let Some(fact) = admit_provider_declared_fact(draft.kind, draft.value, facts.len()) {
            facts.push(fact);
        }
        Ok::<_, std::convert::Infallible>(())
    })
    .expect("direct JSONL file-reference collection is infallible");
    facts
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
        CaptureProvider::GrokBuild => {
            super::grok_build::grok_build_event_identity(value).map(str::to_owned)
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
        CaptureProvider::GrokBuild => super::grok_build::grok_build_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        CaptureProvider::Qoder => super::qoder_parser::qoder_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        _ => native_jsonl_header_session_id(provider, header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
    };
    let (
        provider_session_id,
        parent_provider_session_id,
        external_agent_id,
        agent_scope,
        session_relationship,
    ) = native_jsonl_path_session(provider, path, header, &native_session_id);
    let started_at = match provider {
        CaptureProvider::GrokBuild => super::grok_build::grok_build_timestamp(header),
        _ => native_jsonl_timestamp(header),
    }
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
    DirectJsonlSession {
        native_session_id,
        provider_session_id,
        root_provider_session_id: None,
        parent_provider_session_id,
        external_agent_id,
        agent_scope,
        session_relationship,
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

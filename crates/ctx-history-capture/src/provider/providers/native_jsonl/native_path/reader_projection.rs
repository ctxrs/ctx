use super::*;

impl DirectJsonlPageReader {
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

        if self.session.is_none() {
            let starts_session = match self.provider {
                CaptureProvider::Qoder => {
                    super::qoder_parser::qoder_header_session_id(&value).is_some()
                }
                CaptureProvider::QwenCode => {
                    super::qwen_code::qwen_code_header_session_id(&value).is_some()
                }
                _ => native_jsonl_record_starts_session(self.provider, &value),
            };
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
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                occurred_at,
                record_digest,
            );
        }

        let touches = match direct_jsonl_touches(&value, event_type, false) {
            DirectJsonlTouchProjection::Accepted(touches) => touches,
            DirectJsonlTouchProjection::LimitExceeded => {
                return Ok(ProjectedLine::rejection(file_touch_limit_rejection(
                    &self.path,
                    ordinal,
                    byte_start,
                    byte_end_exclusive,
                )));
            }
        };
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
        )?;
        attach_direct_message_locator(
            &mut event,
            self.provider,
            &self.source_format,
            &value,
            bytes,
            byte_start,
            byte_end_exclusive,
            line_number,
        )?;
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
        let retained_failures = subrecords
            .iter()
            .filter(|subrecord| {
                matches!(
                    subrecord.outcome.outcome,
                    OutputOutcome::Failure | OutputOutcome::Timeout
                )
            })
            .count();
        let retained_failure_touches = if retained_failures == 0 {
            Vec::new()
        } else {
            match direct_jsonl_touches(value, EventType::ToolOutput, true) {
                DirectJsonlTouchProjection::Accepted(touches)
                    if touches.len().saturating_mul(retained_failures)
                        <= DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD =>
                {
                    touches
                }
                DirectJsonlTouchProjection::Accepted(_)
                | DirectJsonlTouchProjection::LimitExceeded => {
                    return Ok(ProjectedLine::rejection(file_touch_limit_rejection(
                        &self.path,
                        ordinal,
                        byte_start,
                        byte_end_exclusive,
                    )));
                }
            }
        };
        for subrecord in subrecords {
            let sub_ordinal = subrecord.subrecord_index;
            if matches!(
                subrecord.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            ) {
                let mut event = direct_event(
                    self.provider,
                    &self.source_format,
                    value,
                    ordinal,
                    sub_ordinal,
                    line_number,
                    occurred_at,
                    Some(&subrecord),
                    retained_failure_touches.clone(),
                )?;
                event.source_record = DirectJsonlSourceRecord {
                    byte_start,
                    byte_end_exclusive,
                    record_digest,
                };
                projected.events.push(event);
            }
        }
        projected.recompute_serialized_bytes();
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
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_text(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_text(value, event_type),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_text(value),
        _ => native_jsonl_event_text(provider, value, event_type, entry_type),
    }
}

fn direct_jsonl_lexical_text(
    event_type: EventType,
    text: &str,
    result: Option<&super::result_content::NativeJsonlResultSubrecord<'_>>,
) -> String {
    if let Some(result) = result {
        let tool_name = result.tool_name.unwrap_or(event_type.as_str());
        let outcome = match result.outcome.outcome {
            OutputOutcome::Failure => "failure",
            OutputOutcome::Timeout => "timeout",
            OutputOutcome::Success => "success",
            OutputOutcome::Unknown => "event",
        };
        return format!("{tool_name} {outcome}");
    }
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text.to_owned()
    }
}

pub(crate) fn hydrated_direct_jsonl_lexical_text(
    provider: CaptureProvider,
    value: &Value,
    sub_ordinal: u32,
) -> Result<Option<String>> {
    let event_type = direct_jsonl_event_type(provider, value);
    if event_type != EventType::ToolOutput {
        let entry_type = native_jsonl_entry_type(provider, value);
        let text = direct_jsonl_event_text(provider, value, event_type, &entry_type);
        return Ok(Some(direct_jsonl_lexical_text(event_type, &text, None)));
    }
    let subrecords = match provider {
        CaptureProvider::FactoryAiDroid => super::enumerate_factory_droid_results(value),
        CaptureProvider::Qoder => super::qoder_parser::enumerate_qoder_results(value),
        CaptureProvider::QwenCode => super::qwen_code::enumerate_qwen_code_results(value),
        _ => {
            let Some(profile) = native_jsonl_result_content_profile(provider) else {
                return Ok(None);
            };
            enumerate_native_jsonl_result_subrecords(profile, value)
        }
    }
    .map_err(|error| {
        CaptureError::InvalidPayload(format!(
            "direct JSONL result record changed shape during hydration: {error:?}"
        ))
    })?;
    Ok(subrecords
        .iter()
        .find(|result| {
            result.subrecord_index == sub_ordinal
                && matches!(
                    result.outcome.outcome,
                    OutputOutcome::Failure | OutputOutcome::Timeout
                )
        })
        .map(|result| direct_jsonl_lexical_text(event_type, "", Some(result))))
}

fn direct_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_model(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_model(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_model(value),
        _ => native_jsonl_model(provider, value),
    }
}

#[allow(clippy::too_many_arguments)]
fn attach_direct_message_locator(
    event: &mut DirectJsonlEvent,
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    line_number: usize,
) -> Result<()> {
    use crate::complete_content::jsonl::JSONL_COMPLETE_CONTENT_LOCATOR_KIND;
    use crate::complete_content::{
        attach_verified_content_locator, verified_content_address_supported,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
    };

    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let entry_type = native_jsonl_entry_type(provider, value);
    let text = direct_jsonl_event_text(provider, value, EventType::Message, &entry_type);
    if text.chars().count() <= crate::PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
        || byte_start >= byte_end_exclusive
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported direct JSONL route has no complete-content profile",
        ));
    };
    let mut range = [0_u8; 16];
    range[..8].copy_from_slice(&byte_start.to_be_bytes());
    range[8..].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range,
        native_jsonl_event_id(provider, value, line_number),
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("direct JSONL verified-content locator is malformed"),
    )?;
    Ok(())
}

#[derive(Default)]
pub(super) struct ProjectedLine {
    pub(super) events: Vec<DirectJsonlEvent>,
    pub(super) rejections: Vec<DirectJsonlRejection>,
    pub(super) serialized_bytes: usize,
}

impl ProjectedLine {
    fn event(event: DirectJsonlEvent) -> Self {
        let mut line = Self {
            events: vec![event],
            ..Self::default()
        };
        line.recompute_serialized_bytes();
        line
    }

    fn rejection(rejection: DirectJsonlRejection) -> Self {
        let serialized_bytes = rejection_wire_bytes(&rejection);
        Self {
            rejections: vec![rejection],
            serialized_bytes,
            ..Self::default()
        }
    }

    fn recompute_serialized_bytes(&mut self) {
        self.serialized_bytes = self
            .events
            .iter()
            .map(event_wire_bytes)
            .chain(self.rejections.iter().map(rejection_wire_bytes))
            .fold(0_usize, usize::saturating_add);
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
) -> Result<DirectJsonlEvent> {
    let event_type = direct_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = direct_jsonl_role(provider, value);
    let text = if event_type == EventType::ToolOutput {
        String::new()
    } else {
        direct_jsonl_event_text(provider, value, event_type, &entry_type)
    };
    let lexical_text = direct_jsonl_lexical_text(event_type, &text, result);

    let positional_event_index = if sub_ordinal == 0 {
        raw_ordinal
    } else {
        raw_ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|index| index.checked_add(u64::from(sub_ordinal)))
            .map(|index| index | (1_u64 << 63))
            .ok_or(CaptureError::SystemInvariant(
                "direct JSONL provider event identity index overflowed",
            ))?
    };
    let native_record_id = direct_jsonl_native_event_identity(provider, value);
    let provider_event_sequence_index = positional_event_index;
    let provider_event_hash = crate::compute_payload_hash(&json!({
        "event_type": event_type.as_str(),
        "role": role.as_str(),
        "native_record_id": native_record_id,
        "sub_ordinal": sub_ordinal,
        "lexical_text": lexical_text,
        "touches": touches,
    }))?;
    Ok(DirectJsonlEvent {
        raw_ordinal,
        sub_ordinal,
        native_record_id,
        provider_event_sequence_index,
        provider_event_hash,
        event_type,
        role,
        occurred_at,
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
        touches,
        source_record: DirectJsonlSourceRecord::default(),
    })
}

enum DirectJsonlTouchProjection {
    Accepted(Vec<DirectJsonlTouch>),
    LimitExceeded,
}

enum DirectJsonlTouchVisitError {
    LimitExceeded,
}

fn direct_jsonl_touches(
    value: &Value,
    event_type: EventType,
    retained_failure: bool,
) -> DirectJsonlTouchProjection {
    if event_type == EventType::ToolOutput && !retained_failure {
        return DirectJsonlTouchProjection::Accepted(Vec::new());
    }
    let mut touches = Vec::new();
    let mut seen = BTreeSet::new();
    let outcome = visit_all_file_touch_drafts(value, |draft| {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if seen.insert(key) {
            if touches.len() == DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD {
                return Err(DirectJsonlTouchVisitError::LimitExceeded);
            }
            touches.push(DirectJsonlTouch {
                path: draft.path,
                old_path: draft.old_path,
                change_kind: draft.change_kind,
            });
        }
        Ok(())
    });
    match outcome {
        Ok(()) => DirectJsonlTouchProjection::Accepted(touches),
        Err(DirectJsonlTouchVisitError::LimitExceeded) => DirectJsonlTouchProjection::LimitExceeded,
    }
}

pub(crate) fn direct_jsonl_complete_message_provider_event_hash(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    raw_ordinal: u64,
    line_number: usize,
) -> Option<String> {
    let event_type = direct_jsonl_event_type(provider, value);
    if event_type != EventType::Message {
        return None;
    }
    let DirectJsonlTouchProjection::Accepted(touches) =
        direct_jsonl_touches(value, event_type, false)
    else {
        return None;
    };
    direct_event(
        provider,
        source_format,
        value,
        raw_ordinal,
        0,
        line_number,
        DateTime::<Utc>::UNIX_EPOCH,
        None,
        touches,
    )
    .ok()
    .map(|event| event.provider_event_hash)
}

fn file_touch_limit_rejection(
    path: &Path,
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> DirectJsonlRejection {
    DirectJsonlRejection {
        raw_ordinal: ordinal,
        byte_start,
        byte_end_exclusive,
        reason: format!(
            "{}:{} exceeds the {} unique file-touch transaction bound",
            path.display(),
            ordinal.saturating_add(1),
            DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD
        ),
    }
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

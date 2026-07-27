use super::*;
use crate::complete_content::{
    verified_content_route_matches, BrokeredSourceAccess, ResolvedResultContent,
    ResultContentRequest,
};

pub(crate) const JUNIE_JSONL_RECORD_SET_LOCATOR_KIND: &str = "junie-jsonl-record-set-v1";
const JUNIE_RECORD_SET_HEADER_BYTES: usize = 7;
const JUNIE_RECORD_SET_ENTRY_BYTES: usize = 24;
const MAX_JUNIE_RECORD_SET_ENTRIES: usize = 64;
const JUNIE_RECORD_SET_DIGEST_DOMAIN: &[u8] = b"ctx-junie-jsonl-record-set-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JunieRecordSetTarget {
    UserPrompt,
    AssistantMessage,
    StepOutput(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JunieRecordSetEntry {
    ordinal: u64,
    range: JsonlRange,
    payload_digest: [u8; 32],
}

/// Bounded transient address builder for one buffered Junie turn.
///
/// It retains coordinates and digests only, never provider content. Turns that
/// cannot fit the V1 locator bound remain importable but are deliberately not
/// advertised as reopenable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct JunieRecordSetBinding {
    entries: Vec<JunieRecordSetEntry>,
    unavailable: bool,
}

impl JunieRecordSetBinding {
    pub(crate) fn observe(&mut self, record: &CapturedRecord) {
        if self.unavailable {
            return;
        }
        let CapturedRecordPayload::NativeBytes(payload) = record.payload() else {
            self.invalidate();
            return;
        };
        let Ok((byte_start, byte_end_exclusive)) = jsonl_locator_range(record.locator()) else {
            self.invalidate();
            return;
        };
        if byte_start >= byte_end_exclusive
            || self.entries.len() >= MAX_JUNIE_RECORD_SET_ENTRIES
            || self.entries.last().is_some_and(|prior| {
                prior.ordinal >= record.ordinal() || prior.range.byte_end_exclusive > byte_start
            })
        {
            self.invalidate();
            return;
        }
        self.entries.push(JunieRecordSetEntry {
            ordinal: record.ordinal(),
            range: JsonlRange {
                byte_start,
                byte_end_exclusive,
            },
            payload_digest: Sha256::digest(payload).into(),
        });
    }

    pub(crate) fn invalidate(&mut self) {
        self.entries.clear();
        self.unavailable = true;
    }

    fn encoded(&self, target: JunieRecordSetTarget) -> Option<Vec<u8>> {
        if self.unavailable || self.entries.is_empty() {
            return None;
        }
        let target_index = match target {
            JunieRecordSetTarget::UserPrompt => 0,
            JunieRecordSetTarget::AssistantMessage => 0,
            JunieRecordSetTarget::StepOutput(index) => index,
        };
        let target_tag = match target {
            JunieRecordSetTarget::UserPrompt => 3,
            JunieRecordSetTarget::AssistantMessage => 1,
            JunieRecordSetTarget::StepOutput(_) => 2,
        };
        let count = u16::try_from(self.entries.len()).ok()?;
        let mut encoded = Vec::with_capacity(
            JUNIE_RECORD_SET_HEADER_BYTES.checked_add(
                self.entries
                    .len()
                    .checked_mul(JUNIE_RECORD_SET_ENTRY_BYTES)?,
            )?,
        );
        encoded.extend_from_slice(&count.to_be_bytes());
        encoded.push(target_tag);
        encoded.extend_from_slice(&target_index.to_be_bytes());
        for entry in &self.entries {
            encoded.extend_from_slice(&entry.ordinal.to_be_bytes());
            encoded.extend_from_slice(&entry.range.byte_start.to_be_bytes());
            encoded.extend_from_slice(&entry.range.byte_end_exclusive.to_be_bytes());
        }
        valid_junie_record_set_locator(&encoded).then_some(encoded)
    }

    fn record_digest(&self) -> Option<CompleteContentBodyDigest> {
        if self.unavailable || self.entries.is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(JUNIE_RECORD_SET_DIGEST_DOMAIN);
        digest.update((self.entries.len() as u64).to_be_bytes());
        for entry in &self.entries {
            digest.update(entry.ordinal.to_be_bytes());
            digest.update(entry.range.byte_start.to_be_bytes());
            digest.update(entry.range.byte_end_exclusive.to_be_bytes());
            digest.update(entry.payload_digest);
        }
        CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
    }

    fn native_record_id(&self, target: JunieRecordSetTarget) -> Option<String> {
        let first = self.entries.first()?;
        let last = self.entries.last()?;
        let target = match target {
            JunieRecordSetTarget::UserPrompt => "user-prompt".to_owned(),
            JunieRecordSetTarget::AssistantMessage => "message".to_owned(),
            JunieRecordSetTarget::StepOutput(index) => format!("step-output-{index}"),
        };
        Some(format!(
            "junie-records-{}-{}-{target}",
            first.ordinal, last.ordinal
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DecodedJunieRecordSet {
    pub(super) target: JunieRecordSetTarget,
    pub(super) entries: Vec<(u64, JsonlRange)>,
}

impl DecodedJunieRecordSet {
    pub(super) fn decode(locator: &CompleteContentSourceLocator) -> Option<Self> {
        if locator.kind() != JUNIE_JSONL_RECORD_SET_LOCATOR_KIND {
            return None;
        }
        decode_junie_record_set_locator(locator.value())
    }
}

pub(crate) fn valid_junie_record_set_locator(value: &[u8]) -> bool {
    decode_junie_record_set_locator(value).is_some()
}

fn decode_junie_record_set_locator(value: &[u8]) -> Option<DecodedJunieRecordSet> {
    if value.len() < JUNIE_RECORD_SET_HEADER_BYTES {
        return None;
    }
    let count = usize::from(u16::from_be_bytes(value[..2].try_into().ok()?));
    if count == 0 || count > MAX_JUNIE_RECORD_SET_ENTRIES {
        return None;
    }
    let expected = JUNIE_RECORD_SET_HEADER_BYTES
        .checked_add(count.checked_mul(JUNIE_RECORD_SET_ENTRY_BYTES)?)?;
    if value.len() != expected {
        return None;
    }
    let target_index = u32::from_be_bytes(value[3..7].try_into().ok()?);
    let target = match value[2] {
        1 if target_index == 0 => JunieRecordSetTarget::AssistantMessage,
        2 => JunieRecordSetTarget::StepOutput(target_index),
        3 if target_index == 0 && count == 1 => JunieRecordSetTarget::UserPrompt,
        _ => return None,
    };
    let mut entries = Vec::with_capacity(count);
    for chunk in value[JUNIE_RECORD_SET_HEADER_BYTES..].chunks_exact(JUNIE_RECORD_SET_ENTRY_BYTES) {
        let ordinal = u64::from_be_bytes(chunk[..8].try_into().ok()?);
        let byte_start = u64::from_be_bytes(chunk[8..16].try_into().ok()?);
        let byte_end_exclusive = u64::from_be_bytes(chunk[16..24].try_into().ok()?);
        if byte_start >= byte_end_exclusive
            || entries
                .last()
                .is_some_and(|(prior_ordinal, prior_range): &(u64, JsonlRange)| {
                    *prior_ordinal >= ordinal || prior_range.byte_end_exclusive > byte_start
                })
        {
            return None;
        }
        entries.push((
            ordinal,
            JsonlRange {
                byte_start,
                byte_end_exclusive,
            },
        ));
    }
    Some(DecodedJunieRecordSet { target, entries })
}

pub(crate) fn attach_junie_record_set_locator(
    event: &mut ProviderEventEnvelope,
    role: VerifiedContentRole,
    content: &str,
    binding: &JunieRecordSetBinding,
    target: JunieRecordSetTarget,
) -> CaptureResult<Option<ContentRef>> {
    if (role == VerifiedContentRole::MessageBody
        && (event.event_type != EventType::Message
            || content.chars().count() <= PROVIDER_MAX_TEXT_CHARS))
        || (role == VerifiedContentRole::ResultBody
            && !matches!(
                event.event_type,
                EventType::ToolOutput | EventType::CommandOutput
            ))
        || content.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(None);
    }
    let Some(encoded) = binding.encoded(target) else {
        return Ok(None);
    };
    let Some(record_sha256) = binding.record_digest() else {
        return Ok(None);
    };
    let Some(native_record_id) = binding.native_record_id(target) else {
        return Ok(None);
    };
    let Some(content_ref) = ContentRef::from_bytes(content.as_bytes()) else {
        return Ok(None);
    };
    let Some(profile) = verified_content_profile(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        role,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "Junie record-set route must have a verified-content profile",
        ));
    };
    let Some(locator) = VerifiedContentLocatorV1::new(
        role,
        profile,
        content_ref.clone(),
        CompleteContentSourceFamily::Jsonl,
        JUNIE_JSONL_RECORD_SET_LOCATOR_KIND,
        &encoded,
        native_record_id,
        record_sha256,
    ) else {
        return Ok(None);
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    if role == VerifiedContentRole::ResultBody {
        let body = event
            .payload
            .get_mut("body")
            .and_then(Value::as_object_mut)
            .ok_or(CaptureError::SystemInvariant(
                "Junie result event body must be an object",
            ))?;
        body.insert(
            "result_content_ref".to_owned(),
            serde_json::json!(content_ref.clone()),
        );
    }
    Ok(Some(content_ref))
}

fn read_junie_record_set(
    access: &BrokeredSourceAccess,
    event_id: uuid::Uuid,
    decoded: &DecodedJunieRecordSet,
    expected_digest: &CompleteContentBodyDigest,
) -> Result<Vec<(u64, Value)>, CompleteContentError> {
    let total_bytes = decoded
        .entries
        .iter()
        .try_fold(0_usize, |total, (_, range)| {
            total.checked_add(range.length()?)
        });
    if total_bytes.is_none_or(|bytes| bytes > COMPLETE_CONTENT_MAX_BODY_BYTES) {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentTooLarge,
            event_id,
        ));
    }
    let mut digest = Sha256::new();
    digest.update(JUNIE_RECORD_SET_DIGEST_DOMAIN);
    digest.update((decoded.entries.len() as u64).to_be_bytes());
    let mut values = Vec::with_capacity(decoded.entries.len());
    for (ordinal, range) in &decoded.entries {
        let record = access.read_jsonl_record_for_aggregate(
            range.byte_start,
            range.byte_end_exclusive,
            event_id,
        )?;
        let payload = record
            .strip_suffix(b"\n")
            .unwrap_or(&record)
            .strip_suffix(b"\r")
            .unwrap_or_else(|| record.strip_suffix(b"\n").unwrap_or(&record));
        let value = serde_json::from_slice::<Value>(payload).map_err(|_| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            )
        })?;
        digest.update(ordinal.to_be_bytes());
        digest.update(range.byte_start.to_be_bytes());
        digest.update(range.byte_end_exclusive.to_be_bytes());
        digest.update(Sha256::digest(payload));
        values.push((*ordinal, value));
    }
    let observed = CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            )
        })?;
    if &observed != expected_digest {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            event_id,
        ));
    }
    Ok(values)
}

fn replay_junie_record_set(
    decoded: &DecodedJunieRecordSet,
    values: &[(u64, Value)],
    event_id: uuid::Uuid,
) -> Result<String, CompleteContentError> {
    if decoded.target == JunieRecordSetTarget::UserPrompt {
        let [(_, value)] = values else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            ));
        };
        return value
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| *kind == "UserPromptEvent")
            .and_then(|_| value.get("prompt"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    event_id,
                )
            });
    }
    let mut buffer = crate::provider::providers::junie::JunieAssistantBuffer::default();
    for (ordinal, value) in values {
        if value.get("kind").and_then(Value::as_str) != Some("SessionA2uxEvent") {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            ));
        }
        let agent_event = value
            .get("event")
            .and_then(|event| event.get("agentEvent"))
            .ok_or_else(|| {
                CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    event_id,
                )
            })?;
        let source_line_number = ordinal.checked_add(1).ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            )
        })?;
        let occurred_at = value
            .get("timestampMs")
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .or_else(|| DateTime::<Utc>::from_timestamp(0, 0))
            .ok_or_else(|| {
                CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    event_id,
                )
            })?;
        let agent_kind = agent_event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(
            agent_kind,
            "LlmResponseMetadataEvent"
                | "ResultBlockUpdatedEvent"
                | "AgentFailureEvent"
                | "ToolBlockUpdatedEvent"
                | "TerminalBlockUpdatedEvent"
                | "ViewFilesBlockUpdatedEvent"
                | "FileChangesBlockUpdatedEvent"
        ) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            ));
        }
        crate::provider::providers::junie::junie_merge_buffered_agent_event(
            &mut buffer,
            agent_event,
            source_line_number,
            occurred_at,
        );
    }
    match decoded.target {
        JunieRecordSetTarget::UserPrompt => {
            unreachable!("handled before replaying assistant state")
        }
        JunieRecordSetTarget::AssistantMessage => Ok(
            crate::provider::providers::junie::junie_buffer_result_text(&buffer),
        ),
        JunieRecordSetTarget::StepOutput(index) => {
            crate::provider::providers::junie::junie_buffer_step_output(&buffer, index)
                .map(str::to_owned)
                .ok_or_else(|| {
                    CompleteContentError::new(
                        CompleteContentErrorKind::ContentVerificationFailed,
                        event_id,
                    )
                })
        }
    }
}

pub(super) fn resolve_messages(
    requests: &[CompleteMessageRequest],
) -> Result<Vec<CompleteMessage>, CompleteContentError> {
    let Some(first) = requests.first() else {
        return Ok(Vec::new());
    };
    let mut prior = None;
    let mut decoded = Vec::with_capacity(requests.len());
    for request in requests {
        let locator = request
            .source_locator
            .as_ref()
            .and_then(DecodedJunieRecordSet::decode)
            .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
        let position = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if request.provider != CaptureProvider::Junie
            || request.source_format != JUNIE_SESSION_EVENTS_SOURCE_FORMAT
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Jsonl
            || request.expected_record_digest.is_none()
            || request.expected_native_record_id.as_deref()
                != decoded_native_record_id(&locator).as_deref()
            || !verified_content_route_matches(
                &request.content_profile,
                request.provider,
                &request.source_format,
                CompleteContentSourceFamily::Jsonl,
                VerifiedContentRole::MessageBody,
                request
                    .source_locator
                    .as_ref()
                    .map_or("", CompleteContentSourceLocator::kind),
            )
            || prior.is_some_and(|prior| prior >= position)
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        prior = Some(position);
        decoded.push(locator);
    }
    let mut messages = Vec::with_capacity(requests.len());
    for (request, locator) in requests.iter().zip(&decoded) {
        let expected_digest = request
            .expected_record_digest
            .as_ref()
            .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
        if locator.target == JunieRecordSetTarget::UserPrompt {
            let source_line = locator
                .entries
                .first()
                .and_then(|(ordinal, _)| ordinal.checked_add(1))
                .ok_or_else(|| {
                    error(request, CompleteContentErrorKind::ContentVerificationFailed)
                })?;
            if request.expected_provider_event_hash != format!("line:{source_line}:user") {
                return Err(error(
                    request,
                    CompleteContentErrorKind::ContentVerificationFailed,
                ));
            }
        }
        let values = read_junie_record_set(
            &request.source_access,
            request.event_id,
            locator,
            expected_digest,
        )?;
        let text = replay_junie_record_set(locator, &values, request.event_id)?;
        messages.push(CompleteMessage::verified(
            request,
            text,
            SourceVerification::VERIFIED,
        )?);
    }
    first.source_access.revalidate_jsonl(first.event_id)?;
    Ok(messages)
}

pub(super) fn resolve_results(
    requests: &[ResultContentRequest],
) -> Vec<Result<ResolvedResultContent, CompleteContentError>> {
    let group = resolve_result_group(requests);
    match group {
        Ok(results) => results,
        Err(error) => requests
            .iter()
            .map(|request| Err(CompleteContentError::new(error.kind, request.event_id)))
            .collect(),
    }
}

fn resolve_result_group(
    requests: &[ResultContentRequest],
) -> Result<Vec<Result<ResolvedResultContent, CompleteContentError>>, CompleteContentError> {
    let Some(first) = requests.first() else {
        return Ok(Vec::new());
    };
    let mut prior = None;
    let mut decoded = Vec::with_capacity(requests.len());
    for request in requests {
        let locator = DecodedJunieRecordSet::decode(&request.source_locator).ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                request.event_id,
            )
        })?;
        let position = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if request.provider != CaptureProvider::Junie
            || request.source_format != JUNIE_SESSION_EVENTS_SOURCE_FORMAT
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Jsonl
            || !matches!(locator.target, JunieRecordSetTarget::StepOutput(_))
            || request.expected_native_record_id
                != decoded_native_record_id(&locator).unwrap_or_default()
            || !verified_content_route_matches(
                &request.content_profile,
                request.provider,
                &request.source_format,
                request.source_family,
                VerifiedContentRole::ResultBody,
                request.source_locator.kind(),
            )
            || prior.is_some_and(|prior| prior >= position)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        prior = Some(position);
        decoded.push(locator);
    }
    let mut results = Vec::with_capacity(requests.len());
    for (request, locator) in requests.iter().zip(&decoded) {
        let resolved = (|| {
            let values = read_junie_record_set(
                &request.source_access,
                request.event_id,
                locator,
                &request.expected_record_digest,
            )?;
            let content = replay_junie_record_set(locator, &values, request.event_id)?;
            if !request.expected_content_ref.verifies(content.as_bytes()) {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    request.event_id,
                ));
            }
            Ok(ResolvedResultContent {
                event_id: request.event_id,
                content,
                content_ref: request.expected_content_ref.clone(),
                verification: SourceVerification::VERIFIED,
            })
        })();
        results.push(resolved);
    }
    first.source_access.revalidate_jsonl(first.event_id)?;
    Ok(results)
}

pub(super) fn decoded_native_record_id(decoded: &DecodedJunieRecordSet) -> Option<String> {
    let first = decoded.entries.first()?.0;
    let last = decoded.entries.last()?.0;
    let target = match decoded.target {
        JunieRecordSetTarget::UserPrompt => "user-prompt".to_owned(),
        JunieRecordSetTarget::AssistantMessage => "message".to_owned(),
        JunieRecordSetTarget::StepOutput(index) => format!("step-output-{index}"),
    };
    Some(format!("junie-records-{first}-{last}-{target}"))
}

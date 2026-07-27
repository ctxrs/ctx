//! Verified command/tool-result recovery from structured provider snapshots.

use std::time::Instant;

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::complete_content::{
    verified_content_route_matches, CompleteContentBodyDigest, CompleteContentError,
    CompleteContentErrorKind, CompleteContentHashAuthority, CompleteContentSourceFamily,
    CompleteMessageRequest, ResolvedResultContent, ResultContentRequest, ResultContentResolver,
    SourceVerification, VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::provider::normalization::provider_message_id;
use crate::provider::providers::{
    continue_cli::{continue_tool_result_body, continue_tool_result_native_id},
    openhands::decode_openhands_event_value,
    task_json::{task_json_provider, task_json_result_content},
};

use super::{
    contracts::{decode_structured_result_locator, StructuredLocator},
    source_access::{
        error, parse_bounded_json, task_json_records, validate_json_shape,
        whole_json_path_candidate, StructuredSourceSnapshot, TaskJsonRecord,
    },
    verification::{digest_bytes, ResolutionBudget},
    StructuredCompleteContentResolver, STRUCTURED_RESULT_CONTENT_LOCATOR_KIND,
};

impl ResultContentResolver for StructuredCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Structured
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        matches!(
            (provider, source_format),
            (CaptureProvider::Continue, "continue_cli_sessions_json")
                | (CaptureProvider::OpenHands, "openhands_file_events")
                | (CaptureProvider::RovoDev, "rovodev_session_json_tree")
                | (CaptureProvider::Cline, "cline_task_directory_json")
                | (CaptureProvider::RooCode, "roo_task_directory_json")
        )
    }

    fn resolve_results(
        &self,
        requests: &[ResultContentRequest],
    ) -> Vec<std::result::Result<ResolvedResultContent, CompleteContentError>> {
        let resolved = requests.first().map_or_else(
            || Ok(Vec::new()),
            |first| {
                if first.provider == CaptureProvider::Continue {
                    self.resolve_continue_result_group(requests)
                } else {
                    self.resolve_result_group(requests)
                }
            },
        );
        match resolved {
            Ok(results) => results,
            Err(error) => requests
                .iter()
                .map(|request| Err(CompleteContentError::new(error.kind, request.event_id)))
                .collect(),
        }
    }
}

impl StructuredCompleteContentResolver {
    fn resolve_result_group(
        &self,
        requests: &[ResultContentRequest],
    ) -> std::result::Result<
        Vec<std::result::Result<ResolvedResultContent, CompleteContentError>>,
        CompleteContentError,
    > {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if !<Self as ResultContentResolver>::supports(self, first.provider, &first.source_format) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                first.event_id,
            ));
        }
        let mut previous = None;
        for request in requests {
            let coordinate = (
                request.source_record_ordinal,
                request.source_record_subrecord_index,
            );
            if request.provider != first.provider
                || request.source_format != first.source_format
                || request.source_access != first.source_access
                || request.source_access.family() != CompleteContentSourceFamily::Structured
                || request.source_family != CompleteContentSourceFamily::Structured
                || previous.is_some_and(|prior| prior >= coordinate)
            {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    request.event_id,
                ));
            }
            previous = Some(coordinate);
        }
        let locators = requests
            .iter()
            .map(StructuredLocator::for_result_request)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let shims = requests.iter().map(result_request_shim).collect::<Vec<_>>();
        let deadline = Instant::now() + self.bounds.deadline;
        let mut budget = ResolutionBudget::new(self.bounds, deadline);
        let snapshot = first.source_access.structured_snapshot(first.event_id)?;
        snapshot.validate_bounds(self.bounds, &shims[0])?;
        let mut output = Vec::with_capacity(requests.len());
        for ((request, shim), locator) in requests.iter().zip(&shims).zip(&locators) {
            let resolved = resolve_result_one(request, shim, locator, snapshot, &mut budget)
                .and_then(|resolved| verified_result_content(request, resolved));
            output.push(resolved);
        }
        Ok(output)
    }

    fn resolve_continue_result_group(
        &self,
        requests: &[ResultContentRequest],
    ) -> std::result::Result<
        Vec<std::result::Result<ResolvedResultContent, CompleteContentError>>,
        CompleteContentError,
    > {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let mut previous = None;
        let mut locators = Vec::with_capacity(requests.len());
        for request in requests {
            let coordinate = (
                request.source_record_ordinal,
                request.source_record_subrecord_index,
            );
            if request.provider != CaptureProvider::Continue
                || request.source_format != "continue_cli_sessions_json"
                || request.source_family != CompleteContentSourceFamily::Structured
                || request.source_access != first.source_access
                || request.source_access.family() != CompleteContentSourceFamily::Structured
                || previous.is_some_and(|prior| prior >= coordinate)
                || !verified_content_route_matches(
                    &request.content_profile,
                    request.provider,
                    &request.source_format,
                    request.source_family,
                    VerifiedContentRole::ResultBody,
                    request.source_locator.kind(),
                )
            {
                return Err(result_error(
                    request,
                    CompleteContentErrorKind::ContentVerificationFailed,
                ));
            }
            let locator = StructuredResultLocator::for_request(request)?;
            if locators
                .first()
                .is_some_and(|first: &StructuredResultLocator| {
                    first.record_digest != locator.record_digest
                })
            {
                return Err(result_error(
                    request,
                    CompleteContentErrorKind::ContentVerificationFailed,
                ));
            }
            locators.push(locator);
            previous = Some(coordinate);
        }

        let shim = result_request_shim(first);
        let deadline = Instant::now() + self.bounds.deadline;
        let mut budget = ResolutionBudget::new(self.bounds, deadline);
        let snapshot = first.source_access.structured_snapshot(first.event_id)?;
        snapshot.validate_bounds(self.bounds, &shim)?;
        let expected_record_digest = &locators[0].record_digest;
        let mut saw_candidate = false;
        let mut matched = None;
        for file in snapshot.files() {
            if !whole_json_path_candidate(CaptureProvider::Continue, file.path()) {
                continue;
            }
            saw_candidate = true;
            if digest_bytes(file.bytes()) == expected_record_digest.as_str() {
                matched = Some(file.bytes());
                break;
            }
        }
        let bytes = matched.ok_or_else(|| {
            result_error(
                first,
                if saw_candidate {
                    CompleteContentErrorKind::SourceChanged
                } else {
                    CompleteContentErrorKind::SourceMissing
                },
            )
        })?;
        let session = parse_bounded_json(&shim, bytes, &mut budget)?;
        Ok(requests
            .iter()
            .zip(locators.iter())
            .map(|(request, locator)| resolve_continue_result(request, locator, &session))
            .collect())
    }
}

#[derive(Debug)]
struct StructuredResultLocator {
    record_digest: CompleteContentBodyDigest,
    history_item: u32,
    tool_state: u32,
}

impl StructuredResultLocator {
    fn for_request(
        request: &ResultContentRequest,
    ) -> std::result::Result<Self, CompleteContentError> {
        if request.source_locator.kind() != STRUCTURED_RESULT_CONTENT_LOCATOR_KIND {
            return Err(result_error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        let (provider, ordinal, source_subrecord, history_item, tool_state, native_id) =
            decode_structured_result_locator(request.source_locator.value()).ok_or_else(|| {
                result_error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
        if provider != request.provider
            || ordinal != request.source_record_ordinal
            || source_subrecord != request.source_record_subrecord_index
            || native_id != request.expected_native_record_id
        {
            return Err(result_error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        Ok(Self {
            record_digest: request.expected_record_digest.clone(),
            history_item,
            tool_state,
        })
    }
}

fn resolve_continue_result(
    request: &ResultContentRequest,
    locator: &StructuredResultLocator,
    session: &Value,
) -> std::result::Result<ResolvedResultContent, CompleteContentError> {
    let item = session
        .get("history")
        .and_then(Value::as_array)
        .and_then(|items| items.get(locator.history_item as usize))
        .ok_or_else(|| result_error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let state = item
        .get("toolCallStates")
        .and_then(Value::as_array)
        .and_then(|states| states.get(locator.tool_state as usize))
        .ok_or_else(|| result_error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let native_id =
        continue_tool_result_native_id(item, locator.history_item, state, locator.tool_state);
    if native_id != request.expected_native_record_id {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let content = continue_tool_result_body(state)
        .ok_or_else(|| result_error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if content.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
        || !request.expected_content_ref.verifies(content.as_bytes())
    {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

fn result_error(
    request: &ResultContentRequest,
    kind: CompleteContentErrorKind,
) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}
#[derive(Debug)]
struct StructuredResolvedResult {
    content: String,
    native_id: String,
}

fn resolve_result_one(
    request: &ResultContentRequest,
    shim: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<StructuredResolvedResult, CompleteContentError> {
    debug_assert_eq!(request.provider, locator.provider);
    let resolved = match request.provider {
        CaptureProvider::RovoDev => {
            resolve_whole_json_result(shim, locator, snapshot, budget, rovodev_result)
        }
        CaptureProvider::OpenHands => resolve_openhands_result(shim, locator, snapshot, budget),
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            resolve_task_json_result(shim, locator, snapshot, budget)
        }
        _ => Err(error(shim, CompleteContentErrorKind::HydrationUnsupported)),
    }?;
    if resolved.native_id != locator.native_id {
        return Err(error(
            shim,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved)
}

fn verified_result_content(
    request: &ResultContentRequest,
    resolved: StructuredResolvedResult,
) -> std::result::Result<ResolvedResultContent, CompleteContentError> {
    if resolved.content.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentTooLarge,
            request.event_id,
        ));
    }
    if !request
        .expected_content_ref
        .verifies(resolved.content.as_bytes())
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content: resolved.content,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

type WholeJsonResultExtractor =
    fn(
        &CompleteMessageRequest,
        &StructuredLocator,
        &Value,
    ) -> std::result::Result<StructuredResolvedResult, CompleteContentError>;

fn resolve_whole_json_result(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
    extract: WholeJsonResultExtractor,
) -> std::result::Result<StructuredResolvedResult, CompleteContentError> {
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        if !whole_json_path_candidate(request.provider, path) {
            continue;
        }
        saw_candidate = true;
        let bytes = file.bytes();
        if digest_bytes(bytes) != locator.record_digest.as_str() {
            continue;
        }
        let value = parse_bounded_json(request, bytes, budget)?;
        return extract(request, locator, &value);
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn rovodev_result(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    session: &Value,
) -> std::result::Result<StructuredResolvedResult, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let messages = session
        .get("message_history")
        .or_else(|| session.pointer("/session_context/message_history"))
        .or_else(|| session.get("messages"))
        .or_else(|| session.pointer("/conversation/messages"))
        .and_then(Value::as_array)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let message = messages
        .get(locator.subrecord as usize)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let content = crate::provider::providers::rovodev::rovodev_result_content(message)
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    Ok(StructuredResolvedResult {
        content,
        native_id: provider_message_id(message, u64::from(locator.subrecord)),
    })
}

fn resolve_openhands_result(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<StructuredResolvedResult, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || !path
                .components()
                .any(|part| part.as_os_str() == "v1_conversations")
        {
            continue;
        }
        saw_candidate = true;
        let bytes = file.bytes();
        if digest_bytes(bytes) != locator.record_digest.as_str() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(bytes)
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        validate_json_shape(request, &value, budget, 0)?;
        let decoded = decode_openhands_event_value(path, value).map_err(|decode_error| {
            error(
                request,
                if decode_error.is_too_large() {
                    CompleteContentErrorKind::ContentTooLarge
                } else {
                    CompleteContentErrorKind::ContentVerificationFailed
                },
            )
        })?;
        let content = crate::provider::providers::openhands::openhands_result_content(&decoded)
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        return Ok(StructuredResolvedResult {
            content,
            native_id: decoded.event_id().to_owned(),
        });
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn resolve_task_json_result(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<StructuredResolvedResult, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let spec = task_json_provider(request.provider);
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let source = if file_name == spec.api_file {
            "api_conversation_history"
        } else if file_name == spec.ui_file {
            "ui_messages"
        } else if spec.fallback_api_file == Some(file_name) {
            "claude_messages"
        } else {
            continue;
        };
        saw_candidate = true;
        let bytes = file.bytes();
        for record in task_json_records(request, bytes, budget)? {
            let TaskJsonRecord {
                native_index,
                bytes: record_bytes,
                value: raw,
            } = record;
            if digest_bytes(record_bytes) != locator.record_digest.as_str() {
                continue;
            }
            let content = task_json_result_content(&raw, source).ok_or_else(|| {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
            let native_id = crate::provider::providers::task_json::task_json_string_field(
                &raw,
                &["id", "uuid", "messageId"],
            )
            .unwrap_or_else(|| format!("{source}-{native_index}"));
            let suffix = format!(":{source}:{native_id}");
            if !locator.native_id.ends_with(&suffix) || locator.native_id.len() <= suffix.len() {
                return Err(error(
                    request,
                    CompleteContentErrorKind::ContentVerificationFailed,
                ));
            }
            return Ok(StructuredResolvedResult {
                content,
                native_id: locator.native_id.clone(),
            });
        }
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn result_request_shim(request: &ResultContentRequest) -> CompleteMessageRequest {
    CompleteMessageRequest {
        event_id: request.event_id,
        provider: request.provider,
        source_format: request.source_format.clone(),
        source_access: request.source_access.clone(),
        source_family: Some(CompleteContentSourceFamily::Structured),
        content_profile: request.content_profile.clone(),
        source_locator: Some(request.source_locator.clone()),
        provider_session_id: None,
        source_record_ordinal: request.source_record_ordinal,
        source_record_subrecord_index: request.source_record_subrecord_index,
        expected_provider_event_hash: String::new(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(request.expected_native_record_id.clone()),
        expected_record_digest: Some(request.expected_record_digest.clone()),
        expected_content_ref: Some(request.expected_content_ref.clone()),
        indexed_text: String::new(),
        indexed_limit_chars: 0,
    }
}

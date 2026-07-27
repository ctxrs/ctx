use ctx_history_core::CaptureProvider;
use serde_json::Value;

#[cfg(test)]
use super::JsonlRange;
use super::{
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily,
    JsonlCompleteContentResolver,
};
use crate::complete_content::{
    verified_content_route_matches, verified_content_route_supported, VerifiedContentRole,
};
use crate::complete_content::{
    ResolvedResultContent, ResultContentRequest, ResultContentResolver, SourceVerification,
};
use crate::provider::codex::events::codex_result_content;
use crate::provider::providers::native_jsonl::{
    native_jsonl_event_id,
    result_content::{extract_native_jsonl_result_content, native_jsonl_result_content_profile},
};
use crate::{
    CLAUDE_PROJECTS_SOURCE_FORMAT, CODEX_SESSION_SOURCE_FORMAT, KIMI_CODE_CLI_SOURCE_FORMAT,
    MISTRAL_VIBE_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT,
};

impl JsonlCompleteContentResolver {
    /// Resolves one coordinate-ordered JSONL source batch without changing
    /// complete-message CLI eligibility. Source-level failures fail the batch;
    /// record/body verification failures remain per-item results.
    pub fn resolve_results(
        &self,
        requests: &[ResultContentRequest],
    ) -> Vec<Result<ResolvedResultContent, CompleteContentError>> {
        match self.resolve_result_group(requests) {
            Ok(results) => results,
            Err(error) => requests
                .iter()
                .map(|request| Err(CompleteContentError::new(error.kind, request.event_id)))
                .collect(),
        }
    }

    fn resolve_result_group(
        &self,
        requests: &[ResultContentRequest],
    ) -> Result<Vec<Result<ResolvedResultContent, CompleteContentError>>, CompleteContentError>
    {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if first.provider == CaptureProvider::Mux {
            return Ok(super::mux::resolve_results(requests));
        }
        if first.provider == CaptureProvider::Junie {
            return Ok(super::junie::resolve_results(requests));
        }
        let mut prior_position = None;
        for request in requests {
            let position = (
                request.source_record_ordinal,
                request.source_record_subrecord_index,
            );
            let expected_native_record_id = request
                .source_record_ordinal
                .checked_add(1)
                .map(|line| format!("line-{line}"));
            if request.provider != first.provider
                || request.source_format != first.source_format
                || request.source_family != CompleteContentSourceFamily::Jsonl
                || !verified_content_route_matches(
                    &request.content_profile,
                    request.provider,
                    &request.source_format,
                    request.source_family,
                    VerifiedContentRole::ResultBody,
                    request.source_locator.kind(),
                )
                || request.source_access != first.source_access
                || request.source_access.family() != CompleteContentSourceFamily::Jsonl
                || request.source_record_subrecord_index != 0
                || (request.provider == CaptureProvider::Codex
                    && expected_native_record_id.as_deref()
                        != Some(request.expected_native_record_id.as_str()))
                || prior_position.is_some_and(|prior| prior >= position)
            {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    request.event_id,
                ));
            }
            prior_position = Some(position);
        }
        let decoded_locators = requests
            .iter()
            .map(|request| {
                super::DecodedJsonlLocator::decode(&request.source_locator).ok_or_else(|| {
                    CompleteContentError::new(
                        CompleteContentErrorKind::HydrationUnsupported,
                        request.event_id,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let exact_binding = first.source_access.exact_jsonl_binding().cloned();
        let mut contents = Vec::with_capacity(requests.len());
        for (request, decoded) in requests.iter().zip(&decoded_locators) {
            let resolved = (|| {
                if decoded.binding.as_ref() != exact_binding.as_ref() {
                    return Err(CompleteContentError::new(
                        CompleteContentErrorKind::SourceChanged,
                        request.event_id,
                    ));
                }
                let record = request.source_access.read_jsonl_record(
                    decoded.range.byte_start,
                    decoded.range.byte_end_exclusive,
                    &request.expected_record_digest,
                    request.event_id,
                )?;
                resolve_result_record(request, &record)
            })();
            contents.push(resolved);
        }
        first.source_access.revalidate_jsonl(first.event_id)?;
        Ok(contents)
    }
}

impl ResultContentResolver for JsonlCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Jsonl
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        verified_content_route_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::ResultBody,
        )
    }

    fn resolve_results(
        &self,
        requests: &[ResultContentRequest],
    ) -> Vec<Result<ResolvedResultContent, CompleteContentError>> {
        JsonlCompleteContentResolver::resolve_results(self, requests)
    }
}

fn resolve_result_record(
    request: &ResultContentRequest,
    record: &[u8],
) -> Result<ResolvedResultContent, CompleteContentError> {
    let value = serde_json::from_slice::<Value>(record).map_err(|_| {
        CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        )
    })?;
    let line_number = usize::try_from(request.source_record_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            )
        })?;
    let resolved = if request.provider == CaptureProvider::Codex
        && request.source_format == CODEX_SESSION_SOURCE_FORMAT
    {
        value
            .get("payload")
            .and_then(codex_result_content)
            .map(std::borrow::Cow::into_owned)
            .map(|content| (content, format!("line-{line_number}")))
    } else if native_jsonl_result_content_profile(request.provider)
        == Some(request.content_profile.as_str())
    {
        extract_native_jsonl_result_content(&request.content_profile, &value)
            .ok()
            .flatten()
            .map(|content| {
                (
                    content,
                    native_jsonl_event_id(request.provider, &value, line_number),
                )
            })
    } else {
        result_content_and_id(
            request.provider,
            &request.source_format,
            &value,
            line_number,
        )
    };
    let (content, native_record_id) = resolved.ok_or_else(|| {
        CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        )
    })?;
    if native_record_id != request.expected_native_record_id {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
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
}

pub(crate) fn result_content_and_id(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    line_number: usize,
) -> Option<(String, String)> {
    let content = if provider == CaptureProvider::Pi
        && source_format == crate::provider::providers::pi::PI_SOURCE_FORMAT
    {
        crate::provider::providers::pi::pi_result_content(value)
    } else if provider == CaptureProvider::Claude && source_format == CLAUDE_PROJECTS_SOURCE_FORMAT
    {
        crate::provider::providers::claude::claude_result_content(value)
    } else if provider == CaptureProvider::OpenClaw && source_format == OPENCLAW_SOURCE_FORMAT {
        crate::provider::providers::openclaw::openclaw_result_content(value)
    } else if provider == CaptureProvider::KimiCodeCli
        && source_format == KIMI_CODE_CLI_SOURCE_FORMAT
    {
        crate::provider::providers::kimi::kimi_result_content(value)
    } else if provider == CaptureProvider::MistralVibe
        && source_format == MISTRAL_VIBE_SOURCE_FORMAT
    {
        crate::provider::providers::mistral_vibe::mistral_vibe_result_content(value)
    } else {
        None
    }?;
    let native_record_id = match provider {
        CaptureProvider::Pi | CaptureProvider::OpenClaw => value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned),
        CaptureProvider::Claude => value
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned),
        CaptureProvider::KimiCodeCli => {
            let record_type = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            Some(format!(
                "{}:{}",
                record_type,
                value
                    .get("time")
                    .and_then(Value::as_i64)
                    .map(|time| time.to_string())
                    .unwrap_or_else(|| line_number.to_string())
            ))
        }
        CaptureProvider::MistralVibe => value
            .get("message_id")
            .or_else(|| value.get("tool_call_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                let role = value
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                Some(format!("{role}:line-{line_number}"))
            }),
        _ => None,
    }
    .unwrap_or_else(|| format!("line-{line_number}"));
    Some((content, native_record_id))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_core::ContentRef;
    use serde_json::{json, Value};
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::complete_content::{
        AuthorizedSourceRoute, CompleteContentSourceLocator, SourceAccessBroker, SourceSnapshot,
    };
    use crate::provider::providers::native_jsonl::result_content::TABNINE_RESULT_PROFILE;
    use crate::provider::providers::native_jsonl::result_content::{
        extract_native_jsonl_result_content, native_jsonl_result_content_profile,
    };

    struct ResultCase {
        provider: CaptureProvider,
        source_format: &'static str,
        record: Value,
        expected: &'static str,
    }

    fn cases() -> [ResultCase; 5] {
        [
            ResultCase {
                provider: CaptureProvider::Gemini,
                source_format: crate::GEMINI_CLI_SOURCE_FORMAT,
                record: json!({"id":"gemini-result","type":"gemini","toolCalls":[{"result":{"content":"gemini reopened"}}]}),
                expected: "gemini reopened",
            },
            ResultCase {
                provider: CaptureProvider::Tabnine,
                source_format: crate::TABNINE_CLI_SOURCE_FORMAT,
                record: json!({"id":"tabnine-result","type":"tabnine","toolCalls":[{"result":"tabnine reopened"}]}),
                expected: "tabnine reopened",
            },
            ResultCase {
                provider: CaptureProvider::FactoryAiDroid,
                source_format: crate::FACTORY_DROID_SOURCE_FORMAT,
                record: json!({"id":"droid-result","type":"message","content":[{"type":"tool_result","content":"droid reopened"}]}),
                expected: "droid reopened",
            },
            ResultCase {
                provider: CaptureProvider::CopilotCli,
                source_format: crate::COPILOT_CLI_SOURCE_FORMAT,
                record: json!({"id":"copilot-result","type":"tool.execution_complete","data":{"result":{"content":"copilot reopened"}}}),
                expected: "copilot reopened",
            },
            ResultCase {
                provider: CaptureProvider::QwenCode,
                source_format: crate::QWEN_CODE_SOURCE_FORMAT,
                record: json!({"id":"qwen-result","type":"tool_result","toolCallResult":{"output":"qwen reopened"}}),
                expected: "qwen reopened",
            },
        ]
    }

    fn request_for(
        case: &ResultCase,
    ) -> (tempfile::TempDir, std::path::PathBuf, ResultContentRequest) {
        let directory = tempdir().unwrap();
        let source_path = directory.path().join("result.jsonl");
        let record = serde_json::to_vec(&case.record).unwrap();
        let mut file_bytes = record.clone();
        file_bytes.push(b'\n');
        fs::write(&source_path, &file_bytes).unwrap();
        let profile = native_jsonl_result_content_profile(case.provider).unwrap();
        let extracted = extract_native_jsonl_result_content(profile, &case.record)
            .unwrap()
            .unwrap();
        assert_eq!(extracted, case.expected);
        let end = u64::try_from(file_bytes.len()).unwrap();
        let event_id = Uuid::new_v4();
        let snapshot = SourceSnapshot {
            size_bytes: Some(end),
            ..SourceSnapshot::default()
        };
        let source_access = SourceAccessBroker::new()
            .admit(
                AuthorizedSourceRoute {
                    source_id: Uuid::new_v4(),
                    provider: case.provider,
                    source_format: case.source_format.to_owned(),
                    family: CompleteContentSourceFamily::Jsonl,
                    raw_source_path: source_path.clone(),
                    source_root: Some(directory.path().to_path_buf()),
                    source_identity: Some(format!("{}:result-source", case.provider.as_str())),
                    source_snapshot: snapshot,
                },
                event_id,
            )
            .unwrap();
        let request = ResultContentRequest {
            event_id,
            provider: case.provider,
            source_format: case.source_format.to_owned(),
            source_access,
            source_family: CompleteContentSourceFamily::Jsonl,
            content_profile: profile.to_owned(),
            source_locator: CompleteContentSourceLocator::new(
                super::super::JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
                JsonlRange {
                    byte_start: 0,
                    byte_end_exclusive: end,
                }
                .encode()
                .to_vec(),
            )
            .unwrap(),
            source_record_ordinal: 0,
            source_record_subrecord_index: 0,
            expected_native_record_id: native_jsonl_event_id(case.provider, &case.record, 1),
            expected_record_digest: super::super::digest_bytes(&record),
            expected_content_ref: ContentRef::from_bytes(case.expected.as_bytes()).unwrap(),
        };
        (directory, source_path, request)
    }

    #[test]
    fn native_result_profiles_reopen_exact_source_content() {
        for case in cases() {
            let (_directory, _source_path, request) = request_for(&case);
            let resolved = JsonlCompleteContentResolver::new()
                .resolve_results(std::slice::from_ref(&request))
                .pop()
                .unwrap()
                .unwrap();
            assert_eq!(resolved.event_id, request.event_id);
            assert_eq!(resolved.content, case.expected, "{:?}", case.provider);
            assert_eq!(resolved.content_ref, request.expected_content_ref);
            assert!(resolved.verification.is_verified());
        }
    }

    #[test]
    fn native_result_resolution_rejects_native_id_and_profile_mismatches() {
        let case = &cases()[0];
        let (_directory, _source_path, request) = request_for(case);

        let mut wrong_native_id = request.clone();
        wrong_native_id.expected_native_record_id = "different-native-id".to_owned();
        let error = JsonlCompleteContentResolver::new()
            .resolve_results(&[wrong_native_id])
            .pop()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.kind,
            CompleteContentErrorKind::ContentVerificationFailed
        );

        let mut wrong_profile = request;
        wrong_profile.content_profile = TABNINE_RESULT_PROFILE.to_owned();
        let error = JsonlCompleteContentResolver::new()
            .resolve_results(&[wrong_profile])
            .pop()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.kind,
            CompleteContentErrorKind::ContentVerificationFailed
        );
    }

    #[test]
    fn shared_jsonl_result_range_allows_append_but_rejects_addressed_rewrite() {
        let case = &cases()[0];
        let (directory, source_path, mut request) = request_for(case);
        let mut source = fs::read(&source_path).unwrap();
        source.extend_from_slice(
            br#"{"id":"later-record","type":"user","content":"append is allowed"}"#,
        );
        source.push(b'\n');
        fs::write(&source_path, &source).unwrap();
        request.source_access = SourceAccessBroker::new()
            .admit(
                AuthorizedSourceRoute {
                    source_id: Uuid::new_v4(),
                    provider: case.provider,
                    source_format: case.source_format.to_owned(),
                    family: CompleteContentSourceFamily::Jsonl,
                    raw_source_path: source_path.clone(),
                    source_root: Some(directory.path().to_path_buf()),
                    source_identity: Some(format!("{}:result-source", case.provider.as_str())),
                    source_snapshot: SourceSnapshot {
                        size_bytes: request
                            .source_locator
                            .value()
                            .get(8..)
                            .and_then(|bytes| bytes.try_into().ok())
                            .map(u64::from_be_bytes),
                        ..SourceSnapshot::default()
                    },
                },
                request.event_id,
            )
            .unwrap();

        let appended = JsonlCompleteContentResolver::new()
            .resolve_results(std::slice::from_ref(&request))
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(appended.content, case.expected);

        let changed_byte = source
            .iter()
            .position(|byte| *byte == b'g')
            .expect("fixture contains a mutable JSON string byte");
        source[changed_byte] = b'h';
        fs::write(&source_path, &source).unwrap();
        let error = JsonlCompleteContentResolver::new()
            .resolve_results(&[request])
            .pop()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    }
}

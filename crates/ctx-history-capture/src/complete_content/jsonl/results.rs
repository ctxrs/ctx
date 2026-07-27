use ctx_history_core::CaptureProvider;
use serde_json::Value;

use super::{
    ensure_no_links, open_frozen_source, read_record, revalidate_open_source, selected_source_path,
    CompleteContentError, CompleteContentErrorKind, CompleteContentHashAuthority,
    CompleteContentSourceFamily, CompleteMessageRequest, JsonlCompleteContentResolver, JsonlRange,
};
use crate::complete_content::{ResolvedResultContent, ResultContentRequest, SourceVerification};
use crate::provider::codex::events::codex_result_content;
use crate::CODEX_SESSION_SOURCE_FORMAT;

impl JsonlCompleteContentResolver {
    /// Resolves one coordinate-ordered Codex source batch without changing
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
        let mut prior_position = None;
        for request in requests {
            let position = (
                request.source_record_ordinal,
                request.source_record_subrecord_index,
            );
            if request.provider != CaptureProvider::Codex
                || request.source_format != CODEX_SESSION_SOURCE_FORMAT
                || request.raw_source_path != first.raw_source_path
                || request.source_root != first.source_root
                || request.source_identity != first.source_identity
                || request.source_identity.as_deref().is_none_or(str::is_empty)
                || request.source_record_subrecord_index != 0
                || prior_position.is_some_and(|prior| prior >= position)
            {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    request.event_id,
                ));
            }
            prior_position = Some(position);
        }
        let shims = requests.iter().map(result_request_shim).collect::<Vec<_>>();
        let selected_path = selected_source_path(&shims[0])?;
        ensure_no_links(&selected_path, &shims[0])?;
        let (mut file, frozen) = open_frozen_source(&selected_path, &shims[0])?;
        let mut contents = Vec::with_capacity(requests.len());
        for (request, shim) in requests.iter().zip(&shims) {
            let resolved = (|| {
                let range = JsonlRange::decode(&request.source_locator).ok_or_else(|| {
                    CompleteContentError::new(
                        CompleteContentErrorKind::HydrationUnsupported,
                        request.event_id,
                    )
                })?;
                let record = read_record(&mut file, &frozen, range, shim)?;
                resolve_result_record(request, &record)
            })();
            contents.push(resolved);
        }
        revalidate_open_source(&file, &selected_path, &frozen, &shims[0])?;
        Ok(contents)
    }
}

fn result_request_shim(request: &ResultContentRequest) -> CompleteMessageRequest {
    CompleteMessageRequest {
        event_id: request.event_id,
        provider: request.provider,
        source_format: request.source_format.clone(),
        raw_source_path: request.raw_source_path.clone(),
        source_root: request.source_root.clone(),
        source_identity: request.source_identity.clone(),
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        source_locator: Some(request.source_locator.clone()),
        source_snapshot: request.source_snapshot.clone(),
        provider_session_id: None,
        source_record_ordinal: request.source_record_ordinal,
        source_record_subrecord_index: request.source_record_subrecord_index,
        expected_provider_event_hash: String::new(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: None,
        expected_record_digest: Some(request.expected_record_digest.clone()),
        expected_body_digest: None,
        indexed_text: String::new(),
        indexed_limit_chars: 0,
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
    let content = value
        .get("payload")
        .and_then(codex_result_content)
        .ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            )
        })?;
    if !request.expected_content_ref.verifies(content.as_bytes()) {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content: content.into_owned(),
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

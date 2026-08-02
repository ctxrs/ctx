use super::GeminiAdapterAbstention;
use crate::provider::providers::gemini::nativepath::file_invocation::{
    GeminiFileInvocationExtraction, GeminiFileInvocationOverflow,
};
use crate::repository_attribution::AttributionInput;
use ctx_history_core::{RepositoryAbstentionReason, RepositoryEvidenceKind};

pub(in super::super) fn apply_gemini_file_invocation_extraction(
    input: &mut AttributionInput,
    adapter_abstentions: &mut Vec<GeminiAdapterAbstention>,
    extraction: Result<GeminiFileInvocationExtraction, GeminiFileInvocationOverflow>,
) {
    match extraction {
        Ok(extraction) => {
            input
                .repository_file_invocation_evidence
                .extend(extraction.evidence);
            if extraction.abstained_target_bearing_calls {
                adapter_abstentions.push((
                    RepositoryEvidenceKind::FileActivity,
                    RepositoryAbstentionReason::Unsupported,
                    "gemini_file_invocation_schema_not_proven",
                ));
            }
        }
        Err(_) => {
            // The adapter attempted strict invocation extraction. Preserve that
            // fact even though the all-or-nothing evidence set exceeded a bound,
            // so attribution cannot reinterpret the empty strict set as
            // permission to fall back to the session CWD.
            input.provider_native_context_ambiguous = true;
            adapter_abstentions.push((
                RepositoryEvidenceKind::FileActivity,
                RepositoryAbstentionReason::CandidateLimitExceeded,
                "gemini_file_invocation_evidence_overflow",
            ));
        }
    }
}

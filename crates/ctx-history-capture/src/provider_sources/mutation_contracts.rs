use ctx_history_core::CaptureProvider;

use super::types::ProviderFileMutationContract;

pub fn provider_file_mutation_contract(
    provider: CaptureProvider,
    source_format: &str,
) -> ProviderFileMutationContract {
    use ProviderFileMutationContract::{AppendOnlyNewlineDelimited, WholeReplacement};

    match (provider, source_format) {
        (CaptureProvider::Codex, "codex_session_jsonl_tree" | "codex_session_jsonl")
        | (CaptureProvider::Pi, "pi_session_jsonl")
        | (CaptureProvider::Claude, "claude_projects_jsonl_tree")
        | (CaptureProvider::Tabnine, "tabnine_cli_chat_recording_jsonl") => {
            AppendOnlyNewlineDelimited
        }
        // Codex history can grow by append, but a tail alone cannot reconstruct
        // each affected session's earliest started_at. Keep it replacement-only.
        (CaptureProvider::Codex, "codex_history_jsonl") => WholeReplacement,
        _ => WholeReplacement,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::provider_sources::PROVIDER_IMPORT_REVISIONS;

    #[test]
    fn every_registered_format_has_the_expected_mutation_contract() {
        let append_only = PROVIDER_IMPORT_REVISIONS
            .iter()
            .filter(|entry| {
                provider_file_mutation_contract(entry.provider, entry.source_format)
                    == ProviderFileMutationContract::AppendOnlyNewlineDelimited
            })
            .map(|entry| (entry.provider.as_str(), entry.source_format))
            .collect::<BTreeSet<_>>();

        assert_eq!(
            append_only,
            BTreeSet::from([
                (CaptureProvider::Codex.as_str(), "codex_session_jsonl_tree"),
                (CaptureProvider::Codex.as_str(), "codex_session_jsonl"),
                (CaptureProvider::Pi.as_str(), "pi_session_jsonl"),
                (
                    CaptureProvider::Claude.as_str(),
                    "claude_projects_jsonl_tree"
                ),
                (
                    CaptureProvider::Tabnine.as_str(),
                    "tabnine_cli_chat_recording_jsonl"
                ),
            ])
        );
    }

    #[test]
    fn unknown_and_gemini_formats_default_to_replacement() {
        assert_eq!(
            provider_file_mutation_contract(
                CaptureProvider::Gemini,
                "gemini_cli_chat_recording_jsonl"
            ),
            ProviderFileMutationContract::WholeReplacement
        );
        assert_eq!(
            provider_file_mutation_contract(CaptureProvider::Codex, "future_format"),
            ProviderFileMutationContract::WholeReplacement
        );
    }

    #[test]
    fn codex_history_stays_replacement_only_to_preserve_session_start() {
        assert_eq!(
            provider_file_mutation_contract(CaptureProvider::Codex, "codex_history_jsonl"),
            ProviderFileMutationContract::WholeReplacement
        );
    }
}

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PromptLine {
    session_id: String,
    ts: i64,
    text: String,
}

mod source_backed;

pub(crate) use source_backed::{
    observe_codex_prompt_history_source_backed_explicit_v0,
    scan_codex_prompt_history_source_backed_v0, CodexPromptHistorySourceBackedDispositionV0,
    CodexPromptHistorySourceBackedInputV0, CodexPromptHistorySourceBackedResolverV0,
};

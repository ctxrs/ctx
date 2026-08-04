use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PromptLine {
    session_id: String,
    ts: i64,
    text: String,
}

mod source_backed;

pub(crate) use source_backed::{
    CodexPromptHistoryJsonlFamilyAdapterV0, CodexPromptHistorySourceBackedInputV0,
};

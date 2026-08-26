//! OpenClaw source-backed legacy JSONL capture.

use ctx_history_capture_model::normalization::provider_local_preview;

use super::normalization;

mod routes;
mod source_backed;

use routes::*;

pub(crate) use source_backed::{
    openclaw_source_backed_adapter_v0, openclaw_source_backed_adapter_v0_with_source_root_lineage,
};

const OPENCLAW_IDENTITY_TEXT_MAX_CHARS: usize = 16_000;

pub(super) fn qualify_session_id(agent_id: Option<&str>, session_id: &str) -> String {
    let session_id = capped_text(session_id);
    match agent_id {
        Some(agent_id) if !session_id.contains('/') => format!("{agent_id}/{session_id}"),
        _ => session_id,
    }
}

pub(super) fn capped_text(value: &str) -> String {
    provider_local_preview(value, OPENCLAW_IDENTITY_TEXT_MAX_CHARS).0
}

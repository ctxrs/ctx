//! OpenClaw source-backed legacy JSONL capture.

use crate::provider::normalization::provider_local_preview;

use super::{normalization, openclaw_output_metadata};

mod routes;
mod source_backed;

use routes::*;

pub(crate) use source_backed::openclaw_source_backed_adapter_v0;

pub(super) fn qualify_session_id(agent_id: Option<&str>, session_id: &str) -> String {
    let session_id = capped_text(session_id);
    match agent_id {
        Some(agent_id) if !session_id.contains('/') => format!("{agent_id}/{session_id}"),
        _ => session_id,
    }
}

pub(super) fn capped_text(value: &str) -> String {
    provider_local_preview(value, crate::PROVIDER_MAX_TEXT_CHARS).0
}

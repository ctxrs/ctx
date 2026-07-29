use std::{fs::Metadata, path::Path};

use ctx_history_core::{EventRole, EventType};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    provider::normalization::{provider_role, provider_value_text},
    Result,
};

use super::OpenClawSessionObservation;

const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-source-revision-v1\0";

pub(crate) fn source_from_admitted(
    path: &Path,
    transcript_metadata: &Metadata,
    index: Option<(&Metadata, &[u8])>,
    path_identity: String,
) -> Result<(String, String)> {
    let observation =
        OpenClawSessionObservation::from_admitted(path.to_path_buf(), transcript_metadata, index)?;
    Ok((observation.source_revision(), path_identity))
}

pub(super) fn exact_source_revision_digest(source_revision: &str) -> [u8; 32] {
    domain_digest(SOURCE_REVISION_DIGEST_DOMAIN, source_revision)
}

fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

pub(crate) fn message_record(value: &Value, line_number: usize) -> Option<(String, String)> {
    let row_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let message = value.get("message").unwrap_or(value);
    let role = message
        .get("role")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" if role != Some(EventRole::Tool) => EventType::Message,
        "message" => EventType::ToolOutput,
        _ => EventType::Notice,
    };
    (event_type == EventType::Message).then(|| {
        let native_record_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{line_number}"));
        let text = message
            .get("content")
            .or_else(|| message.get("text"))
            .or_else(|| message.get("output"))
            .and_then(provider_value_text)
            .unwrap_or_default();
        (text, native_record_id)
    })
}

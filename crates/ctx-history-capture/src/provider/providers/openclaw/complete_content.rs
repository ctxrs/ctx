use std::{fs::Metadata, path::Path};

use ctx_history_core::{CaptureProvider, ContentRef, EventRole, EventType};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        COMPLETE_CONTENT_MAX_BODY_BYTES,
    },
    provider::normalization::{
        provider_explicit_result_value_text, provider_role, provider_value_text,
    },
    CaptureError, Result, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    OpenClawSessionObservation,
};

const EXACT_JSONL_LOCATOR_KIND: &str = "jsonl-exact-range-v1";
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-source-revision-v1\0";
const PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";

pub(crate) fn source_from_admitted(
    path: &Path,
    transcript_metadata: &Metadata,
    index: Option<(&Metadata, &[u8])>,
    path_identity: String,
) -> Result<(String, String)> {
    let observation = OpenClawSessionObservation::from_admitted(
        path.to_path_buf(),
        transcript_metadata,
        index,
    )?;
    Ok((observation.source_revision(), path_identity))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_native_path_locators(
    event_type: EventType,
    metadata: &mut Value,
    row: &Value,
    line_number: usize,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    source_revision: &str,
    path_identity: &str,
) -> Result<()> {
    if byte_start >= byte_end_exclusive {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw NativePath locator range is empty",
        ));
    }
    if event_type == EventType::Message {
        if let Some((text, native_record_id)) = message_record(row, line_number) {
            if text.chars().count() > PROVIDER_MAX_TEXT_CHARS
                && text.len() <= COMPLETE_CONTENT_MAX_BODY_BYTES
            {
                attach_locator(
                    metadata,
                    &text,
                    &native_record_id,
                    record_bytes,
                    byte_start,
                    byte_end_exclusive,
                    source_revision,
                    path_identity,
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn attach_locator(
    metadata: &mut Value,
    content: &str,
    native_record_id: &str,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    source_revision: &str,
    path_identity: &str,
) -> Result<bool> {
    let Some(profile) = verified_content_profile(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Ok(false);
    };
    let Some(content_ref) = ContentRef::from_bytes(content.as_bytes()) else {
        return Ok(false);
    };
    let locator_value = exact_locator_value(
        byte_start,
        byte_end_exclusive,
        source_revision,
        path_identity,
    );
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(false);
    };
    attach_verified_content_locator(metadata, locator).ok_or(CaptureError::SystemInvariant(
        "OpenClaw NativePath verified-content locator is malformed",
    ))?;
    Ok(true)
}

fn exact_locator_value(
    byte_start: u64,
    byte_end_exclusive: u64,
    source_revision: &str,
    path_identity: &str,
) -> [u8; 80] {
    let mut value = [0_u8; 80];
    value[..8].copy_from_slice(&byte_start.to_be_bytes());
    value[8..16].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    value[16..48].copy_from_slice(&exact_source_revision_digest(source_revision));
    value[48..80].copy_from_slice(&domain_digest(PATH_IDENTITY_DIGEST_DOMAIN, path_identity));
    value
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

/// Extracts explicit output from an OpenClaw native JSONL tool message.
pub(crate) fn result_content(row: &Value) -> Option<String> {
    if row.get("type").and_then(Value::as_str).unwrap_or("message") != "message" {
        return None;
    }
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    if role != Some(EventRole::Tool) {
        return None;
    }
    message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_explicit_result_value_text)
}

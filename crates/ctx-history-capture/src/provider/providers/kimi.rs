use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::{stable_capture_uuid, ProviderAdapterContext, Result, KIMI_CODE_CLI_SOURCE_FORMAT};

mod event;
mod layout;
pub(crate) mod native_path;
mod source;

#[cfg(test)]
mod tests;

use source::KimiWireObservation;

fn kimi_admission_scope_revision(context: &ProviderAdapterContext) -> String {
    kimi_admission_scope_revision_for_display(context.source_root_display())
}

fn kimi_admission_scope_revision_for_display(source_root: Option<String>) -> String {
    stable_capture_uuid(
        &format!(
            "provider={};source_format={};source_root={:?}",
            CaptureProvider::KimiCodeCli.as_str(),
            KIMI_CODE_CLI_SOURCE_FORMAT,
            source_root,
        ),
        "kimi-admission-scope",
    )
    .to_string()
}

pub(crate) fn kimi_complete_content_record(
    value: &serde_json::Value,
    line_number: usize,
) -> Option<(String, String)> {
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let event_type = event::kimi_event_type(record_type, value);
    (event_type == ctx_history_core::EventType::Message).then(|| {
        let native_record_id =
            event::kimi_legacy_provider_event_hash(record_type, value, line_number);
        (
            event::kimi_event_text(record_type, value, event_type),
            native_record_id,
        )
    })
}

pub(crate) fn kimi_complete_content_normalized_payload(
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    let record_type = value.get("type").and_then(serde_json::Value::as_str)?;
    let event_type = event::kimi_event_type(record_type, value);
    (event_type == ctx_history_core::EventType::Message)
        .then(|| event::kimi_normalized_event_payload(record_type, value, event_type))
}

pub(crate) fn kimi_complete_content_auxiliary_paths(
    path: &Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    layout::complete_content_auxiliary_paths(path)
}

pub(crate) fn kimi_complete_content_source_from_admitted(
    path: &Path,
    source_root: Option<&Path>,
    canonical_path: std::path::PathBuf,
    wire_metadata: &std::fs::Metadata,
    state: Option<(&std::fs::Metadata, &[u8])>,
    index: Option<(&std::fs::Metadata, &[u8])>,
    path_identity: String,
) -> Result<(String, String)> {
    let observation =
        KimiWireObservation::read_from_admitted(path, canonical_path, wire_metadata, state, index)?;
    let admission_scope_revision = kimi_admission_scope_revision_for_display(Some(
        source_root.unwrap_or(path).display().to_string(),
    ));
    Ok((
        observation.complete_content_revision(&admission_scope_revision),
        path_identity,
    ))
}

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::normalization::{
    provider_capped_json, provider_local_preview, provider_timestamp_seconds_to_datetime,
};
use crate::{CaptureError, ProviderImportSummary, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::source::MuxSessionSource;
use super::{MUX_MAX_FAILURE_BYTES, MUX_MAX_ID_BYTES};

#[derive(Debug, Clone)]
pub(super) struct MuxBoundedSessionMetadata {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) started_at: String,
    pub(super) cwd: Option<String>,
    pub(super) model: Option<String>,
    // The bounded metadata preview remains staging-Pro/session diagnostic data.
    #[allow(dead_code)]
    pub(super) metadata: Value,
    pub(super) metadata_revision: String,
    pub(super) metadata_failure: Option<String>,
}

pub(super) fn mux_bounded_session_metadata_from_bytes(
    source: &MuxSessionSource,
    metadata_revision: &str,
    imported_at: DateTime<Utc>,
    bytes: Option<&[u8]>,
) -> Result<MuxBoundedSessionMetadata> {
    let mut summary = ProviderImportSummary::default();
    let metadata = match bytes {
        None => Value::Null,
        Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                push_provider_import_failure(
                    &mut summary,
                    0,
                    "Mux metadata.json must contain a JSON object".to_owned(),
                );
                Value::Null
            }
            Err(error) => {
                push_provider_import_failure(
                    &mut summary,
                    0,
                    format!("invalid Mux metadata.json: {error}"),
                );
                Value::Null
            }
        },
    };
    mux_bounded_session_metadata_from_value(
        source,
        metadata_revision,
        imported_at,
        metadata,
        summary,
    )
}

fn mux_bounded_session_metadata_from_value(
    source: &MuxSessionSource,
    metadata_revision: &str,
    imported_at: DateTime<Utc>,
    metadata: Value,
    summary: ProviderImportSummary,
) -> Result<MuxBoundedSessionMetadata> {
    let provider_session_id = bounded_mux_id(
        mux_string_pointer(&metadata, &["/workspaceId", "/sessionId"])
            .unwrap_or_else(|| source.provider_session_id.clone()),
        &source.session_dir,
        "workspace id",
    )?;
    let parent_provider_session_id = mux_string_pointer(
        &metadata,
        &[
            "/parentWorkspaceId",
            "/parentTaskId",
            "/parentSessionId",
            "/parent_session_id",
        ],
    )
    .or_else(|| source.parent_provider_session_id.clone())
    .map(|value| bounded_mux_id(value, &source.session_dir, "parent workspace id"))
    .transpose()?;
    let root_provider_session_id = mux_string_pointer(
        &metadata,
        &["/rootWorkspaceId", "/rootTaskId", "/rootSessionId"],
    )
    .or_else(|| parent_provider_session_id.clone())
    .map(|value| bounded_mux_id(value, &source.session_dir, "root workspace id"))
    .transpose()?;
    let bounded_text = |value: Option<String>| {
        value.map(|text| provider_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS).0)
    };
    let started_at = ["/createdAt", "/createdAtMs"]
        .iter()
        .find_map(|pointer| metadata.pointer(pointer).and_then(mux_value_timestamp))
        .unwrap_or(imported_at);
    let cwd = bounded_text(mux_string_pointer(
        &metadata,
        &["/projectPath", "/workspacePath", "/cwd", "/repoPath"],
    ));
    let model = bounded_text(mux_string_pointer(&metadata, &["/model"]));
    let metadata_failure = summary
        .failures
        .first()
        .map(|failure| bounded_mux_failure(failure.error.clone()));
    let metadata = json!({
        "workspaceId": provider_session_id.clone(),
        "parentWorkspaceId": parent_provider_session_id.clone(),
        "rootWorkspaceId": root_provider_session_id.clone(),
        "createdAt": started_at.to_rfc3339(),
        "projectPath": cwd.clone(),
        "model": model.clone(),
        "preview": provider_capped_json(&metadata, PROVIDER_MAX_PREVIEW_CHARS),
    });
    Ok(MuxBoundedSessionMetadata {
        provider_session_id,
        parent_provider_session_id,
        root_provider_session_id,
        started_at: started_at.to_rfc3339(),
        cwd,
        model,
        metadata,
        metadata_revision: metadata_revision.to_owned(),
        metadata_failure,
    })
}

pub(super) fn bounded_mux_id(value: String, path: &Path, label: &'static str) -> Result<String> {
    if value.len() > MUX_MAX_ID_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: match label {
                "workspace id" => "Mux workspace id exceeds the supported size",
                "parent workspace id" => "Mux parent workspace id exceeds the supported size",
                _ => "Mux root workspace id exceeds the supported size",
            },
        });
    }
    Ok(value)
}

pub(super) fn bounded_mux_failure(mut error: String) -> String {
    if error.len() <= MUX_MAX_FAILURE_BYTES {
        return error;
    }
    let mut boundary = MUX_MAX_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

pub(super) fn mux_string_pointer(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(super) fn mux_value_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(raw) => parse_rfc3339_utc(raw).or_else(|| {
            raw.parse::<f64>()
                .ok()
                .and_then(provider_timestamp_seconds_to_datetime)
        }),
        Value::Number(number) => number
            .as_f64()
            .and_then(provider_timestamp_seconds_to_datetime),
        _ => None,
    }
}

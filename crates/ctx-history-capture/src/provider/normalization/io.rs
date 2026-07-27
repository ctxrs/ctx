use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::common::io::{ensure_regular_provider_transcript_file, read_text_file_limited};
use crate::{CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

pub(crate) fn provider_optional_regular_file(path: &Path) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            ensure_regular_provider_transcript_file(path)?;
            Ok(Some(path.to_path_buf()))
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript files are rejected",
            })
        }
        Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider sidecar paths must be regular files",
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn read_provider_json_file(path: &Path, label: &str) -> Result<Value> {
    let raw = read_text_file_limited(path, MAX_PROVIDER_JSONL_LINE_BYTES, label)?;
    let value: Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} must contain a JSON object"
        )));
    }
    Ok(value)
}

use std::{
    fs::{self, File},
    io::{BufRead, Read},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

pub(crate) fn collect_jsonl_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        if root.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            ensure_regular_provider_transcript_file(root)?;
            paths.push(root.to_path_buf());
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_jsonl_paths(&path, paths)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            ensure_regular_provider_transcript_file(&path)?;
            paths.push(path);
        }
    }
    Ok(())
}

pub(crate) fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked provider transcript files are rejected",
        });
    }
    if !file_type.is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript paths must be regular files",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(path)?;
    Ok(())
}

pub(crate) fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript path components are rejected",
            });
        }
    }
    Ok(())
}

pub(crate) fn read_text_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = file.take((max_bytes as u64).saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} exceeds max bytes ({max_bytes})"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|err| CaptureError::InvalidPayload(format!("{label} is not valid UTF-8: {err}")))
}

pub(crate) fn read_provider_jsonl_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<bool> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total > 0);
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
                reader.consume(bytes_to_consume);
                return Err(provider_jsonl_line_too_large());
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            return Ok(true);
        }

        let bytes_to_consume = available.len();
        if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
            reader.consume(bytes_to_consume);
            discard_provider_jsonl_line(reader)?;
            return Err(provider_jsonl_line_too_large());
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
        total = total.saturating_add(bytes_to_consume);
    }
}

pub(crate) fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let bytes_to_consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let found_newline = available
            .get(bytes_to_consume.saturating_sub(1))
            .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        if found_newline {
            return Ok(());
        }
    }
}

pub(crate) fn provider_jsonl_line_too_large() -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "provider JSONL line exceeds max bytes ({MAX_PROVIDER_JSONL_LINE_BYTES})"
    ))
}

pub(crate) fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    let text = read_text_file_limited(path, max_bytes, label)?;
    serde_json::from_str(&text).map_err(CaptureError::from)
}

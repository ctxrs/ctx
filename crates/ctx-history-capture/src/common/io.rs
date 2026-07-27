use std::{
    fs::{self, File},
    io::{BufRead, Read},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{CaptureError, ProviderImportSummary, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

pub(crate) fn collect_jsonl_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if provider_metadata_is_link_like(&metadata) {
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
        let metadata = fs::symlink_metadata(&path)?;
        if provider_metadata_is_link_like(&metadata) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path,
                reason: "linked provider transcript path components are rejected",
            });
        }
        let file_type = metadata.file_type();
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
    if provider_metadata_is_link_like(&metadata) {
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

#[cfg(target_os = "windows")]
fn ensure_supported_windows_provider_path_prefix(path: &Path) -> Result<()> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Ok(());
    };
    if !matches!(
        prefix.kind(),
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _)
    ) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "unsupported Windows provider transcript path prefixes are rejected",
        });
    }
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "drive-relative provider transcript paths are rejected",
        });
    }
    Ok(())
}

pub(crate) fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    ensure_supported_windows_provider_path_prefix(path)?;

    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        #[cfg(target_os = "windows")]
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        if current.as_os_str().is_empty() {
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if provider_metadata_is_link_like(&metadata) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript path components are rejected",
            });
        }
    }
    Ok(())
}

pub(crate) fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

pub(crate) fn provider_metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(target_os = "windows"))]
    false
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderJsonlLineRead {
    Eof,
    Line { bytes: usize },
    Oversized { bytes: usize },
}

pub(crate) fn read_provider_jsonl_record_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    line_number: &mut usize,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    loop {
        match read_provider_jsonl_line_or_skip_oversized(reader, buffer)? {
            ProviderJsonlLineRead::Eof => return Ok(false),
            ProviderJsonlLineRead::Line { .. } => {
                *line_number = line_number.saturating_add(1);
                return Ok(true);
            }
            ProviderJsonlLineRead::Oversized { .. } => {
                *line_number = line_number.saturating_add(1);
                summary.skipped += 1;
                summary.skipped_events += 1;
            }
        }
    }
}

pub(crate) fn read_provider_jsonl_line_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<ProviderJsonlLineRead> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total > 0 {
                ProviderJsonlLineRead::Line { bytes: total }
            } else {
                ProviderJsonlLineRead::Eof
            });
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
                reader.consume(bytes_to_consume);
                buffer.clear();
                return Ok(ProviderJsonlLineRead::Oversized {
                    bytes: total.saturating_add(bytes_to_consume),
                });
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            return Ok(ProviderJsonlLineRead::Line {
                bytes: total.saturating_add(bytes_to_consume),
            });
        }

        let bytes_to_consume = available.len();
        if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
            reader.consume(bytes_to_consume);
            let discarded = discard_provider_jsonl_line(reader)?;
            buffer.clear();
            return Ok(ProviderJsonlLineRead::Oversized {
                bytes: total
                    .saturating_add(bytes_to_consume)
                    .saturating_add(discarded),
            });
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
        total = total.saturating_add(bytes_to_consume);
    }
}

pub(crate) fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<usize> {
    let mut discarded = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(discarded);
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
        discarded = discarded.saturating_add(bytes_to_consume);
        if found_newline {
            return Ok(discarded);
        }
    }
}

pub(crate) fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    let text = read_text_file_limited(path, max_bytes, label)?;
    serde_json::from_str(&text).map_err(CaptureError::from)
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;

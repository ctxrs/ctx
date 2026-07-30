use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::{CaptureError, Result};

pub(super) const MUX_MAX_DIRECTORY_DEPTH: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct MuxSessionSource {
    pub(super) session_dir: PathBuf,
    pub(super) chat_path: Option<PathBuf>,
    pub(super) partial_path: Option<PathBuf>,
    pub(super) metadata_path: Option<PathBuf>,
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
}

pub(super) fn mux_session_source_from_dir(dir: &Path) -> Result<Option<MuxSessionSource>> {
    let chat_path = mux_optional_regular_file(&dir.join("chat.jsonl"))?;
    let partial_path = mux_optional_regular_file(&dir.join("partial.json"))?;
    if chat_path.is_none() && partial_path.is_none() {
        return Ok(None);
    }
    let metadata_path = mux_optional_regular_file(&dir.join("metadata.json"))?;
    let provider_session_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: dir.to_path_buf(),
            reason: "Mux session directory is missing a workspace id",
        })?;
    let parent_provider_session_id = mux_parent_session_id_from_path(dir);
    Ok(Some(MuxSessionSource {
        session_dir: dir.to_path_buf(),
        chat_path,
        partial_path,
        metadata_path,
        provider_session_id,
        parent_provider_session_id,
    }))
}

fn mux_optional_regular_file(path: &Path) -> Result<Option<PathBuf>> {
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
            reason: "Mux transcript files must be regular files",
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn mux_parent_session_id_from_path(dir: &Path) -> Option<String> {
    let parent = dir.parent()?;
    if parent.file_name().and_then(|name| name.to_str()) != Some("subagent-transcripts") {
        return None;
    }
    parent
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn visit_mux_session_sources(
    root: &Path,
    visit: &mut dyn FnMut(MuxSessionSource) -> Result<()>,
) -> Result<usize> {
    visit_mux_session_sources_at_depth(root, visit, 0)
}

fn visit_mux_session_sources_at_depth(
    root: &Path,
    visit: &mut dyn FnMut(MuxSessionSource) -> Result<()>,
    depth: usize,
) -> Result<usize> {
    if depth > MUX_MAX_DIRECTORY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Mux session directory nesting exceeds the supported limit",
        });
    }
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
        ensure_regular_provider_transcript_file(root)?;
        if matches!(
            root.file_name().and_then(|name| name.to_str()),
            Some("chat.jsonl" | "partial.json")
        ) {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = mux_session_source_from_dir(session_dir)? {
                    visit(source)?;
                    return Ok(1);
                }
            }
        }
        return Ok(0);
    }
    if !file_type.is_dir() {
        return Ok(0);
    }

    let mut visited = 0_usize;
    if let Some(source) = mux_session_source_from_dir(root)? {
        visit(source)?;
        visited = 1;
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            visited = visited.saturating_add(visit_mux_session_sources_at_depth(
                &entry.path(),
                visit,
                depth.saturating_add(1),
            )?);
        }
    }
    Ok(visited)
}

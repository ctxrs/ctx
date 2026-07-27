use std::{
    fs::{self, Metadata},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::{fnv1a64, CaptureError, Result};

use super::{MUX_CAPTURE_REVISION, MUX_POLICY_REVISION};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MuxFrozenFile {
    pub(super) length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl MuxFrozenFile {
    pub(super) fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn revision_component(&self, output: &mut String) {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        output.push_str(&format!(
            "{}\0{side}{seconds}.{nanos:09}\0{}\0{:?}\0{:?}\n",
            self.length, self.readonly, self.device, self.inode,
        ));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MuxFileObservation {
    pub(super) canonical_path: PathBuf,
    pub(super) content: MuxFrozenFile,
    metadata: Option<MuxFrozenFile>,
}

impl MuxFileObservation {
    pub(super) fn read(path: &Path, metadata_path: Option<&Path>) -> Result<Self> {
        Ok(Self {
            canonical_path: fs::canonicalize(path)?,
            content: MuxFrozenFile::read(path)?,
            metadata: metadata_path.map(MuxFrozenFile::read).transpose()?,
        })
    }

    pub(super) fn source_revision(&self, kind: &str) -> String {
        let mut input = format!(
            "mux-{kind}-v1\0capture={MUX_CAPTURE_REVISION}\0policy={MUX_POLICY_REVISION}\ncontent\n"
        );
        self.content.revision_component(&mut input);
        input.push_str("metadata\n");
        match &self.metadata {
            Some(metadata) => metadata.revision_component(&mut input),
            None => input.push_str("missing\n"),
        }
        format!("mux-{kind}-v1:fnv1a64:{:016x}", fnv1a64(input.as_bytes()))
    }

    pub(super) fn metadata_revision(&self) -> String {
        let mut input = "mux-metadata-v1\n".to_owned();
        match &self.metadata {
            Some(metadata) => metadata.revision_component(&mut input),
            None => input.push_str("missing\n"),
        }
        format!("mux-metadata-v1:fnv1a64:{:016x}", fnv1a64(input.as_bytes()))
    }

    pub(super) fn revalidate(&self, path: &Path, metadata_path: Option<&Path>) -> Result<bool> {
        let content = match MuxFrozenFile::read(path) {
            Ok(content) => content,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let metadata = match metadata_path.map(MuxFrozenFile::read).transpose() {
            Ok(metadata) => metadata,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(content == self.content
            && metadata == self.metadata
            && fs::canonicalize(path)? == self.canonical_path)
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn revision_component_preserves_exact_field_and_byte_order() {
        let frozen = MuxFrozenFile {
            length: 17,
            modified: UNIX_EPOCH + Duration::new(23, 45),
            readonly: true,
            device: Some(7),
            inode: Some(11),
        };
        let mut encoded = String::new();

        frozen.revision_component(&mut encoded);

        assert_eq!(
            encoded.as_bytes(),
            b"17\0+23.000000045\0true\0Some(7)\0Some(11)\n"
        );

        let observation = MuxFileObservation {
            canonical_path: PathBuf::new(),
            content: frozen,
            metadata: None,
        };
        assert_eq!(
            observation.source_revision("chat-jsonl"),
            "mux-chat-jsonl-v1:fnv1a64:f003519cea356bc5"
        );
        assert_eq!(
            observation.metadata_revision(),
            "mux-metadata-v1:fnv1a64:ac13c47bd7db561b"
        );
    }
}

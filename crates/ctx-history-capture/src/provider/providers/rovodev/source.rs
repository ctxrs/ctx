use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::provider::normalization::{provider_optional_regular_file, read_provider_json_file};
use crate::{fnv1a64, CaptureError, Result};

use super::{ROVODEV_CAPTURE_REVISION, ROVODEV_POLICY_REVISION};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RovoDevFrozenFile {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl RovoDevFrozenFile {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            path: path.to_path_buf(),
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
            "{:?}\0{}\0{side}{seconds}.{nanos:09}\0{}\0{:?}\0{:?}\n",
            self.path.as_os_str(),
            self.length,
            self.readonly,
            self.device,
            self.inode,
        ));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RovoDevSessionObservation {
    canonical_path: PathBuf,
    context_file: RovoDevFrozenFile,
    metadata_file: Option<RovoDevFrozenFile>,
}

impl RovoDevSessionObservation {
    pub(super) fn read(source: &RovoDevSessionSource) -> Result<Self> {
        Ok(Self {
            canonical_path: fs::canonicalize(&source.context_path)?,
            context_file: RovoDevFrozenFile::read(&source.context_path)?,
            metadata_file: source
                .metadata_path
                .as_deref()
                .map(RovoDevFrozenFile::read)
                .transpose()?,
        })
    }

    pub(super) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(super) fn context_path(&self) -> &Path {
        &self.context_file.path
    }

    pub(super) fn context_length(&self) -> u64 {
        self.context_file.length
    }

    pub(super) fn source_revision(&self) -> String {
        let mut input = format!(
            "rovodev-session-file-v1\0capture={ROVODEV_CAPTURE_REVISION}\0policy={ROVODEV_POLICY_REVISION}\n"
        );
        self.context_file.revision_component(&mut input);
        match &self.metadata_file {
            Some(file) => file.revision_component(&mut input),
            None => input.push_str("metadata\0missing\n"),
        }
        format!(
            "rovodev-session-file-v1:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }

    pub(super) fn revalidate(&self, source: &RovoDevSessionSource) -> Result<bool> {
        let context_file = match RovoDevFrozenFile::read(&source.context_path) {
            Ok(file) => file,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let current_metadata_path =
            match provider_optional_regular_file(&source.session_dir.join("metadata.json")) {
                Ok(path) => path,
                Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
                Err(error) => return Err(error),
            };
        if current_metadata_path != source.metadata_path {
            return Ok(false);
        }
        let metadata_file = match current_metadata_path.as_deref() {
            Some(path) => match RovoDevFrozenFile::read(path) {
                Ok(file) => Some(file),
                Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(false);
                }
                Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
                Err(error) => return Err(error),
            },
            None => None,
        };
        Ok(context_file == self.context_file
            && metadata_file == self.metadata_file
            && fs::canonicalize(&source.context_path)? == self.canonical_path)
    }
}

#[derive(Debug, Clone)]
pub(super) struct RovoDevSessionSource {
    pub(super) session_dir: PathBuf,
    pub(super) context_path: PathBuf,
    pub(super) metadata_path: Option<PathBuf>,
    pub(super) provider_session_id: String,
}

fn rovodev_session_source_from_dir(dir: &Path) -> Result<Option<RovoDevSessionSource>> {
    let context_path = dir.join("session_context.json");
    if !context_path.is_file() {
        return Ok(None);
    }
    ensure_regular_provider_transcript_file(&context_path)?;
    let metadata_path = provider_optional_regular_file(&dir.join("metadata.json"))?;
    let provider_session_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: dir.to_path_buf(),
            reason: "Rovo Dev session directory is missing a session id",
        })?;
    Ok(Some(RovoDevSessionSource {
        session_dir: dir.to_path_buf(),
        context_path,
        metadata_path,
        provider_session_id,
    }))
}

pub(super) fn read_rovodev_metadata(source: &RovoDevSessionSource) -> (Value, Option<String>) {
    match source.metadata_path.as_deref() {
        Some(path) => match read_provider_json_file(path, "Rovo Dev metadata.json") {
            Ok(value) => (value, None),
            Err(error) => (Value::Null, Some(error.to_string())),
        },
        None => (Value::Null, None),
    }
}

pub(super) fn visit_rovodev_session_sources(
    root: &Path,
    visit: &mut dyn FnMut(RovoDevSessionSource) -> Result<()>,
) -> Result<usize> {
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
        if root.file_name().and_then(|name| name.to_str()) == Some("session_context.json") {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = rovodev_session_source_from_dir(session_dir)? {
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
    if let Some(source) = rovodev_session_source_from_dir(root)? {
        visit(source)?;
        return Ok(1);
    }
    let mut visited = 0_usize;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            visited = visited.saturating_add(visit_rovodev_session_sources(&entry.path(), visit)?);
        }
    }
    Ok(visited)
}

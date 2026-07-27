use std::{
    fs::{self, Metadata},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::{fnv1a64, CaptureError, Result};

use super::{MISTRAL_VIBE_CAPTURE_REVISION, MISTRAL_VIBE_POLICY_REVISION};

pub(super) const MISTRAL_VIBE_MAX_DIRECTORY_DEPTH: usize = 128;
pub(super) const MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES: usize = 4_096;

#[derive(Debug, Clone)]
pub(super) struct MistralVibeSessionSource {
    pub(super) session_dir: PathBuf,
    pub(super) metadata_path: PathBuf,
    pub(super) messages_path: PathBuf,
}

fn mistral_vibe_session_source_from_dir(dir: &Path) -> Result<Option<MistralVibeSessionSource>> {
    let metadata_path = dir.join("meta.json");
    let messages_path = dir.join("messages.jsonl");
    if !metadata_path.is_file() || !messages_path.is_file() {
        return Ok(None);
    }
    ensure_regular_provider_transcript_file(&metadata_path)?;
    ensure_regular_provider_transcript_file(&messages_path)?;
    Ok(Some(MistralVibeSessionSource {
        session_dir: dir.to_path_buf(),
        metadata_path,
        messages_path,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MistralVibeFrozenFile {
    pub(super) length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl MistralVibeFrozenFile {
    fn read(path: &Path) -> Result<Self> {
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
pub(super) struct MistralVibeSessionObservation {
    metadata_file: MistralVibeFrozenFile,
    pub(super) messages_file: MistralVibeFrozenFile,
}

impl MistralVibeSessionObservation {
    pub(super) fn read(source: &MistralVibeSessionSource) -> Result<Self> {
        Ok(Self {
            metadata_file: MistralVibeFrozenFile::read(&source.metadata_path)?,
            messages_file: MistralVibeFrozenFile::read(&source.messages_path)?,
        })
    }

    pub(super) fn source_revision(&self) -> String {
        self.source_revision_for_revisions(
            MISTRAL_VIBE_CAPTURE_REVISION,
            MISTRAL_VIBE_POLICY_REVISION,
        )
    }

    pub(super) fn source_revision_for_revisions(
        &self,
        capture_revision: u32,
        policy_revision: u32,
    ) -> String {
        let mut input = format!(
            "mistral-vibe-session-v1\0capture={capture_revision}\0policy={policy_revision}\nmeta\n"
        );
        self.metadata_file.revision_component(&mut input);
        input.push_str("messages\n");
        self.messages_file.revision_component(&mut input);
        format!(
            "mistral-vibe-session-v1:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }

    pub(super) fn metadata_revision(&self) -> String {
        let mut input = "mistral-vibe-meta-v1\n".to_owned();
        self.metadata_file.revision_component(&mut input);
        format!(
            "mistral-vibe-meta-v1:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }
}

pub(crate) fn mistral_vibe_complete_content_revision_from_admitted(
    metadata: &Metadata,
    messages: &Metadata,
) -> Result<String> {
    let observation = MistralVibeSessionObservation {
        metadata_file: MistralVibeFrozenFile::from_metadata(metadata)?,
        messages_file: MistralVibeFrozenFile::from_metadata(messages)?,
    };
    Ok(observation.source_revision())
}

pub(super) fn visit_mistral_vibe_session_sources(
    root: &Path,
    visit: &mut dyn FnMut(MistralVibeSessionSource) -> Result<()>,
) -> Result<usize> {
    let mut remaining_entries = MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES;
    visit_mistral_vibe_session_sources_at_depth(root, visit, 0, &mut remaining_entries)
}

fn visit_mistral_vibe_session_sources_at_depth(
    root: &Path,
    visit: &mut dyn FnMut(MistralVibeSessionSource) -> Result<()>,
    depth: usize,
    remaining_entries: &mut usize,
) -> Result<usize> {
    if depth > MISTRAL_VIBE_MAX_DIRECTORY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Mistral Vibe session directory nesting exceeds the supported limit",
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
        if root.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl") {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = mistral_vibe_session_source_from_dir(session_dir)? {
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
    if let Some(source) = mistral_vibe_session_source_from_dir(root)? {
        visit(source)?;
        return Ok(1);
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if *remaining_entries == 0 {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason:
                    "Mistral Vibe session traversal exceeds the supported directory entry limit",
            });
        }
        *remaining_entries -= 1;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        directories.push(entry);
    }
    directories.sort_unstable_by_key(|entry| entry.file_name());
    let mut visited = 0_usize;
    for entry in directories {
        visited = visited.saturating_add(visit_mistral_vibe_session_sources_at_depth(
            &entry.path(),
            visit,
            depth.saturating_add(1),
            remaining_entries,
        )?);
    }
    Ok(visited)
}

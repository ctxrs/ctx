use std::{
    fs::{self, Metadata},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::{fnv1a64, CaptureError, Result};

use super::{
    session_tree::{
        bounded_junie_index_meta, junie_index_path_for_events, JunieIndexMeta, JunieSessionPath,
    },
    JUNIE_SOURCE_REVISION_SCHEMA,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JunieFrozenFileMetadata {
    pub(super) length: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
}

impl JunieFrozenFileMetadata {
    pub(super) fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "symlinked provider transcript files are rejected",
                })
            }
            Ok(metadata) if metadata.file_type().is_file() => {
                Self::from_metadata(&metadata).map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
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

    fn revision_component(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JunieSessionObservation {
    pub(super) canonical_path: PathBuf,
    pub(super) events_file: JunieFrozenFileMetadata,
    pub(super) index_file: Option<JunieFrozenFileMetadata>,
    pub(super) auxiliary_revision: u64,
}

impl JunieSessionObservation {
    pub(super) fn read(session_path: &JunieSessionPath) -> Result<Self> {
        let events_file = JunieFrozenFileMetadata::read(&session_path.events_path)?;
        let canonical_path = fs::canonicalize(&session_path.events_path)?;
        let index_path = junie_index_path_for_events(&session_path.events_path);
        let index_file = match index_path {
            Some(path) => JunieFrozenFileMetadata::read_optional(&path)?,
            None => None,
        };
        let auxiliary_revision = junie_index_meta_revision(&session_path.index_meta)?;
        Ok(Self {
            canonical_path,
            events_file,
            index_file,
            auxiliary_revision,
        })
    }

    pub(super) fn source_revision(&self) -> String {
        let index = self
            .index_file
            .as_ref()
            .map(JunieFrozenFileMetadata::revision_component)
            .unwrap_or_else(|| "absent".to_owned());
        let input = format!(
            "{JUNIE_SOURCE_REVISION_SCHEMA}\0events={}\0index={index}\0index-entry={:016x}",
            self.events_file.revision_component(),
            self.auxiliary_revision,
        );
        format!(
            "{JUNIE_SOURCE_REVISION_SCHEMA}:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }

    pub(super) fn revalidate(&self, session_path: &JunieSessionPath) -> Result<bool> {
        match Self::read(session_path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn junie_index_meta_revision(meta: &JunieIndexMeta) -> Result<u64> {
    let meta = bounded_junie_index_meta(meta);
    let value = json!({
        "session_id": meta.session_id,
        "created_at": meta.created_at,
        "updated_at": meta.updated_at,
        "task_name": meta.task_name,
        "project_dir": meta.project_dir,
        "raw": meta.raw,
    });
    Ok(fnv1a64(&serde_json::to_vec(&value)?))
}

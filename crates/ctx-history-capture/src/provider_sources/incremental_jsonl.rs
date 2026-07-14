use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        open_regular_provider_transcript_file, provider_jsonl_line_too_large,
        read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead,
    },
    ProviderImportSummary, Result,
};

const CHECKPOINT_VERSION: u32 = 1;
const SENTINEL_BYTES: u64 = 4 * 1024;
const CLAUDE_RESUME_STATE_VERSION: u32 = 1;
const TABNINE_RESUME_STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFileStableIdentity {
    Unix { device: u64, inode: u64 },
    Windows { volume: u64, file_index: u64 },
}

/// An O(delta) checkpoint for a provider-owned append-only log.
///
/// Append admission relies on the provider mutation contract: after a
/// checkpoint, the provider may only add newline-delimited bytes. Stable
/// identity, monotonic growth, and exact 4 KiB head/boundary sentinels reject
/// replacement, truncation, equal-length observations, and bounded accidental
/// rewrites. They are deliberately not described as authentication of every
/// old prefix byte: no portable file API can prove an arbitrary in-place
/// rewrite followed by an append without rereading O(prefix) bytes or trusting
/// a provider/filesystem change journal. A source that cannot establish the
/// append-only provider fact must use whole replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderJsonlAppendCheckpoint {
    pub version: u32,
    pub stable_identity: ProviderFileStableIdentity,
    pub committed_offset: u64,
    pub complete_line_count: u64,
    pub head_sha256: String,
    pub boundary_sha256: String,
    pub resume_state: Option<ProviderJsonlResumeState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "state",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ProviderJsonlResumeState {
    ClaudeProjects(ClaudeProjectsJsonlResumeState),
    TabnineCli(TabnineJsonlResumeState),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeProjectsJsonlResumeState {
    pub version: u32,
    pub authoritative_session_id: String,
    pub authoritative_started_at: DateTime<Utc>,
}

impl ClaudeProjectsJsonlResumeState {
    pub fn new(authoritative_session_id: String, authoritative_started_at: DateTime<Utc>) -> Self {
        Self {
            version: CLAUDE_RESUME_STATE_VERSION,
            authoritative_session_id,
            authoritative_started_at,
        }
    }

    pub fn current_version() -> u32 {
        CLAUDE_RESUME_STATE_VERSION
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabnineJsonlResumeState {
    pub version: u32,
    pub authoritative_session_id: String,
    pub authoritative_started_at: DateTime<Utc>,
}

impl TabnineJsonlResumeState {
    pub fn new(authoritative_session_id: String, authoritative_started_at: DateTime<Utc>) -> Self {
        Self {
            version: TABNINE_RESUME_STATE_VERSION,
            authoritative_session_id,
            authoritative_started_at,
        }
    }

    pub fn current_version() -> u32 {
        TABNINE_RESUME_STATE_VERSION
    }
}

impl ProviderJsonlResumeState {
    pub fn encode_persisted_json(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn decode_persisted_json(
        value: &str,
    ) -> std::result::Result<Self, ProviderJsonlReplacementReason> {
        let state: Self = serde_json::from_str(value)
            .map_err(|_| ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)?;
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> std::result::Result<(), ProviderJsonlReplacementReason> {
        let (version, current_version, session_id) = match self {
            Self::ClaudeProjects(state) => (
                state.version,
                ClaudeProjectsJsonlResumeState::current_version(),
                state.authoritative_session_id.as_str(),
            ),
            Self::TabnineCli(state) => (
                state.version,
                TabnineJsonlResumeState::current_version(),
                state.authoritative_session_id.as_str(),
            ),
        };
        if version != current_version {
            return Err(ProviderJsonlReplacementReason::UnsupportedAdapterResumeStateVersion);
        }
        if session_id.trim().is_empty() {
            return Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible);
        }
        Ok(())
    }
}

impl ProviderJsonlAppendCheckpoint {
    pub fn current_version() -> u32 {
        CHECKPOINT_VERSION
    }
}

impl ProviderFileStableIdentity {
    pub fn to_storage_key(&self) -> String {
        match self {
            Self::Unix { device, inode } => format!("unix:{device}:{inode}"),
            Self::Windows { volume, file_index } => {
                format!("windows:{volume}:{file_index}")
            }
        }
    }

    pub fn from_storage_key(value: &str) -> Option<Self> {
        let mut fields = value.split(':');
        let kind = fields.next()?;
        let first = fields.next()?.parse().ok()?;
        let second = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        match kind {
            "unix" => Some(Self::Unix {
                device: first,
                inode: second,
            }),
            "windows" => Some(Self::Windows {
                volume: first,
                file_index: second,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderJsonlOpenMode {
    WholeReplacement,
    AppendCapableReplacement,
    Append(ProviderJsonlAppendCheckpoint),
    LegacyCodexPrefixCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderJsonlReplacementReason {
    #[error("the checkpoint was produced by the legacy Codex prefix scheme")]
    LegacyCodexPrefixCheckpoint,
    #[error("the checkpoint version is not supported")]
    UnsupportedCheckpointVersion,
    #[error("the platform could not establish a stable file identity")]
    StableIdentityUnavailable,
    #[error("the file identity changed")]
    StableIdentityChanged,
    #[error("the file shrank below the committed append boundary")]
    FileShrank,
    #[error("the observation ended exactly at the committed append boundary")]
    EqualLengthObservation,
    #[error("the committed append boundary is not newline aligned")]
    BoundaryNotNewlineAligned,
    #[error("the file head no longer matches the checkpoint")]
    HeadHashMismatch,
    #[error("the bytes around the committed append boundary no longer match the checkpoint")]
    BoundaryHashMismatch,
    #[error("an append-only provider file introduced an additional session header")]
    AdditionalSessionHeader,
    #[error("the pinned provider file no longer has a valid authoritative first-row header")]
    AuthoritativeHeaderInvalid,
    #[error("the append checkpoint is missing required adapter resume state")]
    AdapterResumeStateMissing,
    #[error("the append checkpoint resume state is incompatible with the provider format")]
    AdapterResumeStateIncompatible,
    #[error("the append checkpoint adapter resume-state version is unsupported")]
    UnsupportedAdapterResumeStateVersion,
    #[error("an append-capable provider file changed its authoritative session identity")]
    AuthoritativeSessionChanged,
    #[error("checkpoint certification could not read file metadata")]
    CheckpointMetadataIo,
    #[error("checkpoint certification could not validate the observed path identity")]
    CheckpointPathIdentityIo,
    #[error("checkpoint certification could not read the sentinel hashes")]
    CheckpointHashIo,
    #[error("checkpoint certification could not restore the pinned reader position")]
    CheckpointSeekIo,
    #[error("the requested semantic checkpoint exceeds the reader's committed boundary")]
    CheckpointBoundaryInvalid,
    #[error("checkpoint certification encountered an unexpected I/O failure")]
    CheckpointUnexpectedIo,
    #[error("checkpoint certification encountered an unexpected permanent failure")]
    CheckpointUnexpectedPermanentFailure,
    #[error("materialization committed but provider import maintenance remains incomplete")]
    CommittedMaintenanceIncomplete,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ProviderJsonlOpenDecision {
    Ready(ProviderJsonlReader),
    ReplacementRequired(ProviderJsonlReplacementReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderJsonlRecordRead {
    Eof,
    Record {
        bytes: usize,
        line_number: u64,
        newline_terminated: bool,
    },
    Oversized {
        bytes: usize,
        line_number: u64,
        newline_terminated: bool,
    },
    DeferredPartial {
        bytes: usize,
        line_number: u64,
        oversized: bool,
        newline_terminated: bool,
    },
}

#[derive(Debug)]
pub struct ProviderJsonlReader {
    path: PathBuf,
    reader: BufReader<File>,
    stable_identity: Option<ProviderFileStableIdentity>,
    defer_unterminated_tail: bool,
    initial_offset: u64,
    initial_complete_line_count: u64,
    current_offset: u64,
    committed_offset: u64,
    complete_line_count: u64,
    deferred_partial: bool,
    read_limit: Option<u64>,
    validated_checkpoint: Option<ProviderJsonlAppendCheckpoint>,
    #[cfg(test)]
    injected_checkpoint_failure: Option<InjectedCheckpointFailure>,
}

#[cfg(test)]
#[derive(Debug)]
enum InjectedCheckpointFailure {
    Decision(ProviderJsonlReplacementReason),
    Error(crate::CaptureError),
}

pub fn open_provider_jsonl(
    path: impl AsRef<Path>,
    mode: ProviderJsonlOpenMode,
) -> Result<ProviderJsonlOpenDecision> {
    open_provider_jsonl_with_identity(path.as_ref(), mode, stable_file_identity)
}

fn open_provider_jsonl_with_identity(
    path: &Path,
    mode: ProviderJsonlOpenMode,
    identity: impl Fn(&File, &std::fs::Metadata) -> Option<ProviderFileStableIdentity>,
) -> Result<ProviderJsonlOpenDecision> {
    if matches!(mode, ProviderJsonlOpenMode::LegacyCodexPrefixCheckpoint) {
        return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
            ProviderJsonlReplacementReason::LegacyCodexPrefixCheckpoint,
        ));
    }

    let mut file = open_regular_provider_transcript_file(path)?;
    let metadata = file.metadata()?;
    let stable_identity = identity(&file, &metadata);
    let validated_checkpoint = match &mode {
        ProviderJsonlOpenMode::Append(checkpoint) => Some(checkpoint.clone()),
        _ => None,
    };
    let (defer_unterminated_tail, start_offset, complete_line_count) = match mode {
        ProviderJsonlOpenMode::WholeReplacement => (false, 0, 0),
        ProviderJsonlOpenMode::AppendCapableReplacement => (true, 0, 0),
        ProviderJsonlOpenMode::Append(checkpoint) => {
            if checkpoint.version != CHECKPOINT_VERSION {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::UnsupportedCheckpointVersion,
                ));
            }
            let Some(actual_identity) = stable_identity.as_ref() else {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::StableIdentityUnavailable,
                ));
            };
            if actual_identity != &checkpoint.stable_identity {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::StableIdentityChanged,
                ));
            }
            if metadata.len() < checkpoint.committed_offset {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::FileShrank,
                ));
            }
            // The coordinator owns exact unchanged-observation elision. Once
            // capture is invoked, equality is ambiguous: it can be an
            // unchanged file or an equal-length rewrite outside the bounded
            // sentinels, but it is never evidence of an append delta.
            if metadata.len() == checkpoint.committed_offset {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::EqualLengthObservation,
                ));
            }
            if checkpoint.committed_offset > 0 {
                let Some(boundary_byte) =
                    read_range_exact(&mut file, checkpoint.committed_offset - 1, 1)?
                else {
                    return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::FileShrank,
                    ));
                };
                if boundary_byte != b"\n" {
                    return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::BoundaryNotNewlineAligned,
                    ));
                }
            }
            let Some(head_sha256) = hash_head_exact(&mut file, checkpoint.committed_offset)? else {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::FileShrank,
                ));
            };
            if head_sha256 != checkpoint.head_sha256 {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::HeadHashMismatch,
                ));
            }
            let Some(boundary_sha256) =
                hash_boundary_exact(&mut file, checkpoint.committed_offset)?
            else {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::FileShrank,
                ));
            };
            if boundary_sha256 != checkpoint.boundary_sha256 {
                return Ok(ProviderJsonlOpenDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::BoundaryHashMismatch,
                ));
            }
            (
                true,
                checkpoint.committed_offset,
                checkpoint.complete_line_count,
            )
        }
        ProviderJsonlOpenMode::LegacyCodexPrefixCheckpoint => unreachable!(),
    };
    file.seek(SeekFrom::Start(start_offset))?;

    Ok(ProviderJsonlOpenDecision::Ready(ProviderJsonlReader {
        path: path.to_path_buf(),
        reader: BufReader::new(file),
        stable_identity,
        defer_unterminated_tail,
        initial_offset: start_offset,
        initial_complete_line_count: complete_line_count,
        current_offset: start_offset,
        committed_offset: start_offset,
        complete_line_count,
        deferred_partial: false,
        read_limit: None,
        validated_checkpoint,
        #[cfg(test)]
        injected_checkpoint_failure: None,
    }))
}

impl ProviderJsonlReader {
    pub fn open_replacement(path: impl AsRef<Path>) -> Result<Self> {
        match open_provider_jsonl(path, ProviderJsonlOpenMode::WholeReplacement)? {
            ProviderJsonlOpenDecision::Ready(reader) => Ok(reader),
            ProviderJsonlOpenDecision::ReplacementRequired(_) => {
                unreachable!("whole-replacement readers do not validate an old checkpoint")
            }
        }
    }

    pub fn open_append_capable_replacement(path: impl AsRef<Path>) -> Result<Self> {
        match open_provider_jsonl(path, ProviderJsonlOpenMode::AppendCapableReplacement)? {
            ProviderJsonlOpenDecision::Ready(reader) => Ok(reader),
            ProviderJsonlOpenDecision::ReplacementRequired(_) => {
                unreachable!("append-capable replacement does not validate an old checkpoint")
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn committed_offset(&self) -> u64 {
        self.committed_offset
    }

    pub fn complete_line_count(&self) -> u64 {
        self.complete_line_count
    }

    pub fn has_deferred_partial(&self) -> bool {
        self.deferred_partial
    }

    /// Reads the bounded first complete record from the pinned file handle, then
    /// restores the reader to its validated import position without advancing
    /// checkpoint state.
    pub fn read_first_complete_record(&mut self) -> Result<Option<Vec<u8>>> {
        let import_position = self.current_offset;
        self.reader.seek(SeekFrom::Start(0))?;
        let mut line = Vec::new();
        let read = read_provider_jsonl_line_or_skip_oversized(&mut self.reader, &mut line);
        let restore = self.reader.seek(SeekFrom::Start(import_position));
        restore?;

        match read? {
            ProviderJsonlLineRead::Eof => Ok(None),
            ProviderJsonlLineRead::Line {
                newline_terminated: true,
                ..
            } => Ok(Some(line)),
            ProviderJsonlLineRead::Line {
                newline_terminated: false,
                ..
            } => Ok(None),
            ProviderJsonlLineRead::Oversized { .. } => Err(provider_jsonl_line_too_large()),
        }
    }

    /// Rewinds an append-capable replacement reader after a bounded validation
    /// pass. Append readers cannot rewind into their committed prefix.
    pub fn restart_append_capable_replacement(&mut self) -> Result<()> {
        if self.initial_offset != 0 || !self.defer_unterminated_tail {
            return Err(crate::CaptureError::SystemInvariant(
                "only append-capable replacement readers may restart",
            ));
        }
        self.restart_import_position()
    }

    pub fn restart_import_position(&mut self) -> Result<()> {
        self.reader.seek(SeekFrom::Start(self.initial_offset))?;
        self.current_offset = self.initial_offset;
        self.committed_offset = self.initial_offset;
        self.complete_line_count = self.initial_complete_line_count;
        self.deferred_partial = false;
        Ok(())
    }

    /// Freezes parsing at the complete newline boundary reached by a validation
    /// pass. Bytes appended afterward remain for the next file observation.
    pub fn freeze_at_current_complete_boundary(&mut self) {
        self.read_limit = Some(self.committed_offset);
    }

    pub fn read_record(&mut self, buffer: &mut Vec<u8>) -> Result<ProviderJsonlRecordRead> {
        let read = if let Some(read_limit) = self.read_limit {
            if self.current_offset >= read_limit {
                return Ok(ProviderJsonlRecordRead::Eof);
            }
            let remaining = read_limit - self.current_offset;
            let mut bounded = Read::take(Read::by_ref(&mut self.reader), remaining);
            read_provider_jsonl_line_or_skip_oversized(&mut bounded, buffer)?
        } else {
            read_provider_jsonl_line_or_skip_oversized(&mut self.reader, buffer)?
        };
        let (bytes, newline_terminated, oversized) = match read {
            ProviderJsonlLineRead::Eof => return Ok(ProviderJsonlRecordRead::Eof),
            ProviderJsonlLineRead::Line {
                bytes,
                newline_terminated,
            } => (bytes, newline_terminated, false),
            ProviderJsonlLineRead::Oversized {
                bytes,
                newline_terminated,
            } => (bytes, newline_terminated, true),
        };
        self.current_offset = self.current_offset.saturating_add(bytes as u64);
        let line_number = self.complete_line_count.saturating_add(1);

        if !newline_terminated && self.defer_unterminated_tail {
            self.deferred_partial = true;
            buffer.clear();
            return Ok(ProviderJsonlRecordRead::DeferredPartial {
                bytes,
                line_number,
                oversized,
                newline_terminated,
            });
        }
        if newline_terminated {
            self.complete_line_count = line_number;
            self.committed_offset = self.current_offset;
        }

        if oversized {
            Ok(ProviderJsonlRecordRead::Oversized {
                bytes,
                line_number,
                newline_terminated,
            })
        } else {
            Ok(ProviderJsonlRecordRead::Record {
                bytes,
                line_number,
                newline_terminated,
            })
        }
    }

    pub fn read_record_or_skip_oversized(
        &mut self,
        buffer: &mut Vec<u8>,
        line_number: &mut usize,
        summary: &mut ProviderImportSummary,
    ) -> Result<bool> {
        loop {
            match self.read_record(buffer)? {
                ProviderJsonlRecordRead::Eof | ProviderJsonlRecordRead::DeferredPartial { .. } => {
                    return Ok(false);
                }
                ProviderJsonlRecordRead::Record {
                    line_number: current,
                    ..
                } => {
                    *line_number = usize::try_from(current).unwrap_or(usize::MAX);
                    return Ok(true);
                }
                ProviderJsonlRecordRead::Oversized {
                    line_number: current,
                    ..
                } => {
                    *line_number = usize::try_from(current).unwrap_or(usize::MAX);
                    summary.skipped += 1;
                    summary.skipped_events += 1;
                }
            }
        }
    }

    pub fn safe_checkpoint(
        &mut self,
    ) -> Result<std::result::Result<ProviderJsonlAppendCheckpoint, ProviderJsonlReplacementReason>>
    {
        self.checkpoint_at(self.committed_offset, self.complete_line_count)
    }

    pub fn checkpoint_at(
        &mut self,
        committed_offset: u64,
        complete_line_count: u64,
    ) -> Result<std::result::Result<ProviderJsonlAppendCheckpoint, ProviderJsonlReplacementReason>>
    {
        self.checkpoint_at_with_identity(
            committed_offset,
            complete_line_count,
            stable_file_identity,
        )
    }

    #[cfg(test)]
    pub(crate) fn inject_checkpoint_failure(&mut self, reason: ProviderJsonlReplacementReason) {
        self.injected_checkpoint_failure = Some(InjectedCheckpointFailure::Decision(reason));
    }

    #[cfg(test)]
    pub(crate) fn inject_checkpoint_error(&mut self, error: crate::CaptureError) {
        self.injected_checkpoint_failure = Some(InjectedCheckpointFailure::Error(error));
    }

    fn checkpoint_at_with_identity(
        &mut self,
        committed_offset: u64,
        complete_line_count: u64,
        identity: impl Fn(&File, &std::fs::Metadata) -> Option<ProviderFileStableIdentity>,
    ) -> Result<std::result::Result<ProviderJsonlAppendCheckpoint, ProviderJsonlReplacementReason>>
    {
        #[cfg(test)]
        if let Some(failure) = self.injected_checkpoint_failure.take() {
            return match failure {
                InjectedCheckpointFailure::Decision(reason) => Ok(Err(reason)),
                InjectedCheckpointFailure::Error(error) => Err(error),
            };
        }
        if committed_offset > self.committed_offset
            || complete_line_count > self.complete_line_count
        {
            return Ok(Err(
                ProviderJsonlReplacementReason::CheckpointBoundaryInvalid,
            ));
        }
        let Some(stable_identity) = self.stable_identity.clone() else {
            return Ok(Err(
                ProviderJsonlReplacementReason::StableIdentityUnavailable,
            ));
        };
        let metadata = match self.reader.get_ref().metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                return Ok(Err(ProviderJsonlReplacementReason::CheckpointMetadataIo));
            }
        };
        if let Err(reason) =
            validate_stable_identity(&stable_identity, identity(self.reader.get_ref(), &metadata))
        {
            return Ok(Err(reason));
        }
        match validate_path_identity(&self.path, &stable_identity, &identity) {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Ok(Err(reason)),
            Err(_) => {
                return Ok(Err(
                    ProviderJsonlReplacementReason::CheckpointPathIdentityIo,
                ));
            }
        }
        if metadata.len() < committed_offset {
            return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
        }

        let logical_position = self.current_offset;
        let validated_checkpoint = self.validated_checkpoint.clone();
        let hashes = {
            let file = self.reader.get_mut();
            checkpoint_hashes(file, committed_offset, validated_checkpoint.as_ref())
        };
        if self.reader.seek(SeekFrom::Start(logical_position)).is_err() {
            return Ok(Err(ProviderJsonlReplacementReason::CheckpointSeekIo));
        }
        let (head_sha256, boundary_sha256) = match hashes {
            Err(_) => return Ok(Err(ProviderJsonlReplacementReason::CheckpointHashIo)),
            Ok(Ok(hashes)) => hashes,
            Ok(Err(reason)) => return Ok(Err(reason)),
        };

        let metadata = match self.reader.get_ref().metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                return Ok(Err(ProviderJsonlReplacementReason::CheckpointMetadataIo));
            }
        };
        if let Err(reason) =
            validate_stable_identity(&stable_identity, identity(self.reader.get_ref(), &metadata))
        {
            return Ok(Err(reason));
        }
        match validate_path_identity(&self.path, &stable_identity, &identity) {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Ok(Err(reason)),
            Err(_) => {
                return Ok(Err(
                    ProviderJsonlReplacementReason::CheckpointPathIdentityIo,
                ));
            }
        }
        if metadata.len() < committed_offset {
            return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
        }

        Ok(Ok(ProviderJsonlAppendCheckpoint {
            version: CHECKPOINT_VERSION,
            stable_identity,
            committed_offset,
            complete_line_count,
            head_sha256,
            boundary_sha256,
            resume_state: self
                .validated_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.resume_state.clone()),
        }))
    }
}

include!("incremental_jsonl/identity.rs");

#[cfg(test)]
include!("incremental_jsonl/tests.rs");

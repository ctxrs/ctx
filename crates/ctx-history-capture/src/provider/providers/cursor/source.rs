use std::{
    fs::{File, Metadata},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{provider::provider_path_identity, CaptureError, Result};

use super::{
    checkpoint::CursorCheckpoint,
    layout::CursorTranscriptPath,
    parser::{
        scan_cursor_reader, CursorParserStats, CursorRecordRejection, CursorRejectionKind,
        CursorRejectionSummary,
    },
    projection::{CursorNativeEvent, CursorNativeSession},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorObservedTime {
    pub(crate) before_epoch: bool,
    pub(crate) seconds: u64,
    pub(crate) nanos: u32,
}

impl CursorObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorFileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorSourceObservation {
    pub(crate) path: PathBuf,
    pub(crate) locator_identity: String,
    pub(crate) proposed_source_identity: String,
    pub(crate) native_session_id: String,
    pub(crate) length: u64,
    /// Private control-plane proof; never published as an event or result hash.
    pub(crate) content_sha256: [u8; 32],
    pub(crate) modified: CursorObservedTime,
    pub(crate) changed: Option<CursorObservedTime>,
    pub(crate) readonly: bool,
    pub(crate) file_identity: Option<CursorFileIdentity>,
}

#[derive(Debug, Clone)]
pub(crate) struct CursorFrozenSource {
    transcript: CursorTranscriptPath,
    observation: CursorSourceObservation,
}

impl CursorFrozenSource {
    pub(crate) fn transcript(&self) -> &CursorTranscriptPath {
        &self.transcript
    }

    pub(crate) fn observation(&self) -> &CursorSourceObservation {
        &self.observation
    }

    fn open(&self) -> Result<File> {
        let (file, observed) = open_observed_cursor_file(
            self.transcript.source_file(),
            &self.observation.path,
            &self.observation.native_session_id,
        )?;
        if !observed.same_strong_snapshot(&self.observation) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(file)
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        if !observation_from_metadata(
            &self.observation.path,
            &self.observation.native_session_id,
            self.transcript.source_file().metadata(),
            self.observation.content_sha256,
        )?
        .same_strong_snapshot(&self.observation)
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.transcript
            .source_file()
            .revalidate()
            .map_err(|_| CaptureError::SourceChangedDuringCapture)
    }
}

pub(crate) fn freeze_cursor_source(
    transcript: &CursorTranscriptPath,
) -> Result<CursorFrozenSource> {
    let (_, observation) = open_observed_cursor_file(
        transcript.source_file(),
        transcript.path(),
        transcript.native_session_id(),
    )?;
    Ok(CursorFrozenSource {
        transcript: transcript.clone(),
        observation,
    })
}

fn observation_from_metadata(
    path: &Path,
    native_session_id: &str,
    metadata: &Metadata,
    content_sha256: [u8; 32],
) -> Result<CursorSourceObservation> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let locator_identity = provider_path_identity(path)?;
    #[cfg(unix)]
    let (file_identity, changed) = (
        Some(CursorFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }),
        unix_changed_time(metadata),
    );
    #[cfg(not(unix))]
    let (file_identity, changed) = (None, None);
    Ok(CursorSourceObservation {
        path: path.to_path_buf(),
        proposed_source_identity: format!("cursor-native-path-v1:{locator_identity}"),
        locator_identity,
        native_session_id: native_session_id.to_owned(),
        length: metadata.len(),
        content_sha256,
        modified: CursorObservedTime::from_system_time(metadata.modified()?),
        changed,
        readonly: metadata.permissions().readonly(),
        file_identity,
    })
}

pub(crate) fn cursor_complete_content_source_revision(
    observation: &CursorSourceObservation,
) -> String {
    cursor_complete_content_revision(
        observation.length,
        observation.modified,
        observation.changed,
        observation.readonly,
        observation.file_identity,
    )
}

pub(crate) fn cursor_complete_content_source_from_admitted(
    metadata: &Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let (file_identity, changed) = (
        Some(CursorFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }),
        unix_changed_time(metadata),
    );
    #[cfg(not(unix))]
    let (file_identity, changed) = (None, None);
    Ok((
        cursor_complete_content_revision(
            metadata.len(),
            CursorObservedTime::from_system_time(metadata.modified()?),
            changed,
            metadata.permissions().readonly(),
            file_identity,
        ),
        path_identity,
    ))
}

fn cursor_complete_content_revision(
    length: u64,
    modified: CursorObservedTime,
    changed: Option<CursorObservedTime>,
    readonly: bool,
    file_identity: Option<CursorFileIdentity>,
) -> String {
    let changed = changed.map_or_else(|| "none".to_owned(), cursor_observed_time_stamp);
    format!(
        "cursor-complete-content-source-v1:length={length};modified={};changed={changed};readonly={readonly};device={};inode={}",
        cursor_observed_time_stamp(modified),
        file_identity.map_or_else(|| "none".to_owned(), |identity| identity.device.to_string()),
        file_identity.map_or_else(|| "none".to_owned(), |identity| identity.inode.to_string()),
    )
}

fn cursor_observed_time_stamp(time: CursorObservedTime) -> String {
    format!(
        "{}{}.{:09}",
        if time.before_epoch { '-' } else { '+' },
        time.seconds,
        time.nanos
    )
}

impl CursorSourceObservation {
    fn same_strong_snapshot(&self, other: &Self) -> bool {
        self.path == other.path
            && self.locator_identity == other.locator_identity
            && self.native_session_id == other.native_session_id
            && self.length == other.length
            && self.content_sha256 == other.content_sha256
    }
}

fn open_observed_cursor_file(
    opened: &crate::common::io::OpenedProviderSourceFile,
    path: &Path,
    native_session_id: &str,
) -> Result<(File, CursorSourceObservation)> {
    let mut file = opened.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let before = file.metadata()?;
    let content_sha256 = hash_cursor_file(&mut file)?;
    let after = file.metadata()?;
    let before_observation =
        observation_from_metadata(path, native_session_id, &before, content_sha256)?;
    let after_observation =
        observation_from_metadata(path, native_session_id, &after, content_sha256)?;
    if before_observation != after_observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    opened
        .revalidate()
        .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
    file.seek(SeekFrom::Start(0))?;
    Ok((file, after_observation))
}

fn hash_cursor_file(file: &mut File) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn unix_changed_time(metadata: &Metadata) -> Option<CursorObservedTime> {
    use std::os::unix::fs::MetadataExt;

    let seconds = metadata.ctime();
    let nanos = metadata.ctime_nsec();
    if !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    if seconds >= 0 {
        Some(CursorObservedTime {
            before_epoch: false,
            seconds: seconds as u64,
            nanos: nanos as u32,
        })
    } else {
        Some(CursorObservedTime {
            before_epoch: true,
            seconds: seconds.unsigned_abs(),
            nanos: nanos as u32,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceRejection {
    pub(crate) physical_line: u64,
    pub(crate) kind: CursorRejectionKind,
    pub(crate) observed_bytes: u64,
}

impl From<CursorRecordRejection> for CursorSourceRejection {
    fn from(value: CursorRecordRejection) -> Self {
        Self {
            physical_line: value.physical_line,
            kind: value.kind,
            observed_bytes: value.observed_bytes,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorSourceRejections {
    pub(crate) total: u64,
    pub(crate) samples: Vec<CursorSourceRejection>,
}

impl From<CursorRejectionSummary> for CursorSourceRejections {
    fn from(value: CursorRejectionSummary) -> Self {
        Self {
            total: value.total,
            samples: value.samples.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceGeneration {
    pub(crate) observation: CursorSourceObservation,
    pub(crate) session: Option<CursorNativeSession>,
    pub(crate) rejections: CursorSourceRejections,
    pub(crate) checkpoint: CursorCheckpoint,
    pub(crate) stats: CursorParserStats,
}

pub(crate) fn scan_cursor_source(
    frozen: &CursorFrozenSource,
    emit: &mut dyn FnMut(CursorNativeEvent) -> Result<()>,
) -> Result<CursorSourceGeneration> {
    let file = frozen.open()?;
    let mut reader = BufReader::new(file);
    let parsed = scan_cursor_reader(&mut reader, emit)?;
    frozen.revalidate()?;
    let has_retained_events = parsed.stats.retained_messages > 0
        || parsed.stats.retained_summaries > 0
        || parsed.stats.retained_notices > 0
        || parsed.stats.retained_tool_calls > 0;
    let session = (has_retained_events
        || parsed.checkpoint.session.started_at.is_some()
        || parsed.checkpoint.session.title.is_some())
    .then(|| CursorNativeSession {
        native_session_id: frozen.observation.native_session_id.clone(),
        project: frozen.transcript.project().to_path_buf(),
        started_at: parsed.checkpoint.session.started_at,
        ended_at: parsed.checkpoint.session.ended_at,
        title: parsed.checkpoint.session.title.clone(),
    });
    Ok(CursorSourceGeneration {
        observation: frozen.observation.clone(),
        session,
        rejections: parsed.rejections.into(),
        checkpoint: parsed.checkpoint,
        stats: parsed.stats,
    })
}

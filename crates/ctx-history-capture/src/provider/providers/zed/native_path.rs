//! Provider-owned Zed NativePath parsing and Store publication.
//!
//! The provider-private scan feeds the Zed Store vertical directly. Exact temporary
//! output evidence remains available for live Pro hydration.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    complete_content::CompleteContentBodyDigest,
    provider_sources::{
        open_sqlite_source_snapshot, SqliteSourceAccessError, SqliteSourceComponent,
        SqliteSourceReadSnapshot,
    },
    CaptureError,
};

mod decode;
mod dto;
mod output;
mod publication;
mod query;
pub(crate) mod source_backed;
mod staging;
mod vertical;

// Consumed by the provider registration seam once Zed is enabled there.
#[allow(unused_imports)]
pub(super) use source_backed::{
    hydrate_zed_locator_v0, ingest_zed_source_backed_v0, ZedHydratedRecordV0, ZedLocatorResolverV0,
    ZedSourceBackedCountersV0, ZedSourceBackedErrorV0, ZedSourceBackedIngestReceiptV0,
    ZedSourceBackedResultV0,
};
pub(super) use vertical::import_zed_nativepath;

use dto::{
    ZedNativeCounters, ZedNativeGenerationAuthority, ZedNativeIncomplete,
    ZedNativeIncompleteReason, ZedNativeScanOutcome, ZedNativeSink, ZedNativeSourceAuthority,
    ZedNativeSourceSelection,
};
#[cfg(test)]
use dto::{
    ZedNativeEvent, ZedNativeMessageIdentity, ZedNativePage, ZedNativeRejection,
    ZedNativeRejectionKind, ZedNativeSession, ZED_NATIVE_PAGE_MAX_BYTES, ZED_NATIVE_PAGE_MAX_UNITS,
};
use query::scan_zed_native_snapshot;

const ZED_SNAPSHOT_ACQUISITION_ATTEMPTS: usize = 3;
const ZED_SOURCE_INVALID_REASON: &str = "Zed SQLite source must be a regular non-symlink file";
const ZED_SIDECAR_INVALID_REASON: &str = "Zed SQLite sidecar must be a regular non-symlink file";

#[derive(Debug, Error)]
pub(crate) enum ZedNativePathError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("I/O error while preparing Zed NativePath source: {0}")]
    Io(#[from] io::Error),
    #[error("SQLite error while preparing Zed NativePath source: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("system I/O error during {operation}: {source}")]
    SystemIo {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("system SQLite error during {operation}: {source}")]
    SystemSqlite {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error(transparent)]
    SqliteSourceAccess(#[from] SqliteSourceAccessError),
    #[error("Zed NativePath source has an unsupported schema: {0}")]
    UnsupportedSchema(String),
}

pub(super) type ZedNativeResult<T> = std::result::Result<T, ZedNativePathError>;

pub(crate) struct ZedImmutableSqliteSnapshot {
    observed: ZedAdmittedSqliteFamily,
    connection: Option<SqliteSourceReadSnapshot>,
    pub(crate) physical_locator: String,
    pub(crate) snapshot_revision: String,
}

pub(crate) enum ZedSnapshotAcquisition {
    Acquired(Box<ZedImmutableSqliteSnapshot>),
    Incomplete { physical_locator: String },
}

impl ZedImmutableSqliteSnapshot {
    pub(super) fn connection(&self) -> ZedNativeResult<&Connection> {
        self.connection
            .as_ref()
            .ok_or_else(|| {
                ZedNativePathError::Capture(CaptureError::SystemInvariant(
                    "Zed SQLite snapshot was queried after finish",
                ))
            })?
            .connection()
            .map_err(Into::into)
    }

    /// Ends SQLite's pinned transaction and then certifies every retained
    /// DB-family handle and the named parent-directory route.
    pub(super) fn finish(&mut self) -> ZedNativeResult<()> {
        let snapshot = self.connection.take().ok_or_else(|| {
            ZedNativePathError::Capture(CaptureError::SystemInvariant(
                "Zed SQLite snapshot was finished more than once",
            ))
        })?;
        match snapshot.finish() {
            Ok(_) => {}
            Err(
                SqliteSourceAccessError::SourceChanged
                | SqliteSourceAccessError::ConnectionIdentityMismatch,
            ) => return Err(CaptureError::SourceChangedDuringCapture.into()),
            Err(error) => return Err(error.into()),
        }
        self.observed.revalidate()
    }
}

#[derive(Debug)]
struct ZedAdmittedSqliteFamily {
    root: ProviderSourceRoot,
    database: ZedAdmittedSqliteComponent,
    wal: Option<ZedAdmittedSqliteComponent>,
    shared_memory: Option<ZedAdmittedSqliteComponent>,
    rollback_journal: Option<ZedAdmittedSqliteComponent>,
}

#[derive(Debug)]
struct ZedAdmittedSqliteComponent {
    file: OpenedProviderSourceFile,
    evidence: ZedComponentEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ZedComponentEvidence {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    change_token: [u8; 32],
}

impl ZedComponentEvidence {
    fn read(file: &OpenedProviderSourceFile, is_wal: bool) -> ZedNativeResult<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata();
        let change_token = zed_component_change_token(file, is_wal)?;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            change_token,
        })
    }

    fn revision_component(&self) -> String {
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "length={};modified={sign}{seconds}.{nanos:09};readonly={};device={};inode={};change={}",
            self.length,
            self.readonly,
            zed_optional_device(self),
            zed_optional_inode(self),
            zed_hex_digest(&self.change_token),
        )
    }
}

const ZED_ORDINARY_FILE_TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";
const ZED_SQLITE_COMPONENT_TOKEN_DOMAIN: &[u8] = b"ctx-provider-sqlite-component-v1\0";
const ZED_SQLITE_HEADER_BYTES: usize = 100;
const ZED_SQLITE_WAL_HEADER_BYTES: usize = 32;
const ZED_SQLITE_WAL_FRAME_HEADER_BYTES: usize = 24;

fn zed_component_change_token(
    file: &OpenedProviderSourceFile,
    is_wal: bool,
) -> ZedNativeResult<[u8; 32]> {
    let prefix_len = usize::try_from(file.len().min(ZED_SQLITE_HEADER_BYTES as u64))
        .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
    let prefix = file.read_exact_range(0, prefix_len, ZED_SQLITE_HEADER_BYTES)?;
    let mut hasher = Sha256::new();
    hasher.update(ZED_SQLITE_COMPONENT_TOKEN_DOMAIN);
    hasher.update(file.len().to_le_bytes());
    hasher.update(zed_ordinary_file_token(file));
    hasher.update(&prefix);
    if is_wal {
        if let Some(frame_header) = zed_wal_last_frame_header(file, &prefix)? {
            hasher.update(frame_header);
        }
    }
    file.revalidate()?;
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn zed_ordinary_file_token(file: &OpenedProviderSourceFile) -> [u8; 32] {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata();
    let mut platform = Sha256::new();
    platform.update(ZED_ORDINARY_FILE_TOKEN_DOMAIN);
    platform.update(b"unix\0");
    platform.update(metadata.dev().to_le_bytes());
    platform.update(metadata.ino().to_le_bytes());
    platform.update(metadata.ctime().to_le_bytes());
    platform.update(metadata.ctime_nsec().to_le_bytes());
    let platform: [u8; 32] = platform.finalize().into();

    let mut combined = Sha256::new();
    combined.update(ZED_ORDINARY_FILE_TOKEN_DOMAIN);
    combined.update(b"platform\0");
    combined.update(platform);
    combined.finalize().into()
}

#[cfg(not(unix))]
fn zed_ordinary_file_token(file: &OpenedProviderSourceFile) -> [u8; 32] {
    let mut combined = Sha256::new();
    combined.update(ZED_ORDINARY_FILE_TOKEN_DOMAIN);
    combined.update(b"portable\0");
    combined.update(file.len().to_le_bytes());
    combined.finalize().into()
}

fn zed_wal_last_frame_header(
    file: &OpenedProviderSourceFile,
    prefix: &[u8],
) -> ZedNativeResult<Option<Vec<u8>>> {
    if prefix.len() < ZED_SQLITE_WAL_HEADER_BYTES {
        return Ok(None);
    }
    let raw_page_size = u32::from_be_bytes(prefix[8..12].try_into().map_err(|_| {
        CaptureError::InvalidPayload("invalid SQLite WAL page-size header".to_owned())
    })?);
    let page_size = match raw_page_size {
        1 => 65_536_u64,
        512..=65_536 if raw_page_size.is_power_of_two() => u64::from(raw_page_size),
        _ => return Ok(None),
    };
    let frame_size = page_size.saturating_add(ZED_SQLITE_WAL_FRAME_HEADER_BYTES as u64);
    let frames_bytes = file
        .len()
        .saturating_sub(ZED_SQLITE_WAL_HEADER_BYTES as u64);
    if frames_bytes < frame_size || !frames_bytes.is_multiple_of(frame_size) {
        return Ok(None);
    }
    let offset = file.len().saturating_sub(frame_size);
    file.read_exact_range(
        offset,
        ZED_SQLITE_WAL_FRAME_HEADER_BYTES,
        ZED_SQLITE_WAL_FRAME_HEADER_BYTES,
    )
    .map(Some)
    .map_err(Into::into)
}

#[cfg(unix)]
fn zed_optional_device(evidence: &ZedComponentEvidence) -> String {
    evidence.device.to_string()
}

#[cfg(not(unix))]
fn zed_optional_device(_evidence: &ZedComponentEvidence) -> String {
    "none".to_owned()
}

#[cfg(unix)]
fn zed_optional_inode(evidence: &ZedComponentEvidence) -> String {
    evidence.inode.to_string()
}

#[cfg(not(unix))]
fn zed_optional_inode(_evidence: &ZedComponentEvidence) -> String {
    "none".to_owned()
}

fn zed_hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn scan_zed_nativepath(
    selection: &ZedNativeSourceSelection,
    sink: &mut dyn ZedNativeSink,
) -> ZedNativeResult<ZedNativeScanOutcome> {
    scan_zed_nativepath_with_finalizer(selection, sink, || Ok(()))
}

pub(super) fn decode_complete_message(
    row: &super::thread::ZedThreadRow,
    message_ordinal: u64,
    record_digest: CompleteContentBodyDigest,
) -> ZedNativeResult<Option<super::ZedNativePathCompleteMessage>> {
    Ok(
        decode_complete_message_with_identity(row, message_ordinal, record_digest)?
            .map(|resolved| resolved.message),
    )
}

struct ZedResolvedCompleteMessage {
    message: super::ZedNativePathCompleteMessage,
    native_message_id: Option<String>,
    native_message_ordinal: u64,
    native_sub_ordinal: u32,
}

fn decode_complete_message_with_identity(
    row: &super::thread::ZedThreadRow,
    message_ordinal: u64,
    record_digest: CompleteContentBodyDigest,
) -> ZedNativeResult<Option<ZedResolvedCompleteMessage>> {
    let updated_at = super::thread::zed_required_timestamp(&row.updated_at, "updated_at")?;
    let decoded =
        match decode::decode_zed_native_payload(&row.id, &row.data_type, &row.data, updated_at)? {
            decode::ZedDecodeOutcome::Decoded(decoded) => decoded,
            decode::ZedDecodeOutcome::Rejected(failure) => {
                return Err(CaptureError::InvalidPayload(failure.reason).into());
            }
        };
    let mut resolved = None;
    decoded.emit_events(0, &mut |draft| {
        if draft.message_ordinal != message_ordinal {
            return Ok(());
        }
        let complete_text = draft.body.clone();
        let event =
            dto::ZedNativeEvent::from_draft(row.rowid, &row.id, draft, record_digest.clone())?;
        let provider_event_index = event
            .native_order
            .message_ordinal
            .checked_mul(2)
            .and_then(|value| value.checked_add(u64::from(event.native_order.sub_ordinal)))
            .ok_or(CaptureError::SystemInvariant(
                "Zed provider event index overflowed",
            ))?;
        let cursor = event
            .payload
            .get("cursor")
            .and_then(serde_json::Value::as_str)
            .ok_or(CaptureError::SystemInvariant(
                "Zed normalized payload has no cursor",
            ))?
            .to_owned();
        let native_message_id = match &event.identity.message {
            dto::ZedNativeMessageIdentity::ProviderId { value, .. } => Some(value.clone()),
            dto::ZedNativeMessageIdentity::MessageOrdinal(_) => None,
        };
        resolved = Some(ZedResolvedCompleteMessage {
            native_message_id,
            native_message_ordinal: event.native_order.message_ordinal,
            native_sub_ordinal: event.native_order.sub_ordinal,
            message: super::ZedNativePathCompleteMessage {
                provider_event_index,
                legacy_provider_event_hash: event.legacy_content_hash,
                cursor,
                event_type: event.event_type,
                payload: event.payload,
                complete_text,
            },
        });
        Ok(())
    })?;
    Ok(resolved)
}

pub(super) fn into_capture_error(error: ZedNativePathError) -> CaptureError {
    match error {
        ZedNativePathError::Capture(error) => error,
        ZedNativePathError::Io(error) => CaptureError::Io(error),
        ZedNativePathError::Sqlite(error) => CaptureError::Sqlite(error),
        ZedNativePathError::SystemIo { operation, source } => {
            CaptureError::SystemIo { operation, source }
        }
        ZedNativePathError::SystemSqlite { operation, source } => CaptureError::SystemIo {
            operation,
            source: io::Error::other(source),
        },
        ZedNativePathError::SqliteSourceAccess(error) => CaptureError::SystemIo {
            operation: "accessing a root-authorized Zed SQLite source",
            source: io::Error::other(error),
        },
        ZedNativePathError::UnsupportedSchema(reason) => CaptureError::UnsupportedSchema(reason),
    }
}

fn scan_zed_nativepath_with_finalizer(
    selection: &ZedNativeSourceSelection,
    sink: &mut dyn ZedNativeSink,
    before_final_revalidation: impl FnOnce() -> ZedNativeResult<()>,
) -> ZedNativeResult<ZedNativeScanOutcome> {
    let path = selection.selected_path();
    let mut snapshot = match acquire_immutable_snapshot(path)? {
        ZedSnapshotAcquisition::Acquired(snapshot) => *snapshot,
        ZedSnapshotAcquisition::Incomplete { physical_locator } => {
            return Ok(ZedNativeScanOutcome::Incomplete(Box::new(
                ZedNativeIncomplete {
                    source_complete: false,
                    reason: ZedNativeIncompleteReason::SnapshotAcquisitionRace,
                    physical_locator,
                    pages_emitted: 0,
                    counters: ZedNativeCounters::default(),
                },
            )));
        }
    };

    let result = scan_zed_native_snapshot(
        snapshot.connection()?,
        &snapshot.physical_locator,
        &snapshot.snapshot_revision,
        sink,
    )?;
    before_final_revalidation()?;
    match snapshot.finish() {
        Ok(()) => {}
        Err(ZedNativePathError::Capture(CaptureError::SourceChangedDuringCapture)) => {
            return Ok(ZedNativeScanOutcome::Incomplete(Box::new(
                ZedNativeIncomplete {
                    source_complete: false,
                    reason: ZedNativeIncompleteReason::SourceChangedAfterScan,
                    physical_locator: snapshot.physical_locator,
                    pages_emitted: result.pages_emitted,
                    counters: result.counters,
                },
            )));
        }
        Err(error) => return Err(error),
    }

    Ok(ZedNativeScanOutcome::Complete(Box::new(
        ZedNativeGenerationAuthority {
            source_complete: true,
            zero_native_rows: result.counters.native_thread_rows == 0,
            zero_retained_events: result.counters.retained_events == 0,
            has_useful_content: result.counters.sessions_retained > 0
                || result.counters.retained_events > 0,
            source_authority: ZedNativeSourceAuthority::ExactDispatchedDatabase {
                path: selection.selected_path().to_path_buf(),
                inventory_observation_token: selection
                    .inventory_observation_token()
                    .map(str::to_owned),
            },
            physical_locator: snapshot.physical_locator,
            snapshot_revision: snapshot.snapshot_revision,
            capability_digest: result.capability_digest,
            source_integrity_digest: result.source_integrity_digest,
            core_generation_digest: result.core_generation_digest,
            output_index: result.output_index,
            pages_emitted: result.pages_emitted,
            counters: result.counters,
        },
    )))
}

impl ZedAdmittedSqliteFamily {
    fn open(path: &Path) -> ZedNativeResult<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Zed SQLite path has no authority parent",
            })?;
        let filename =
            path.file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Zed SQLite path has no file name",
                })?;
        let root = ProviderSourceRoot::open(parent)?;
        let database = ZedAdmittedSqliteComponent::open(
            &root,
            Path::new(filename),
            ZED_SOURCE_INVALID_REASON,
            false,
        )?;
        let wal = ZedAdmittedSqliteComponent::open_optional(
            &root,
            &zed_sidecar_relative_path(filename, "-wal"),
            ZED_SIDECAR_INVALID_REASON,
            true,
        )?;
        let shared_memory = ZedAdmittedSqliteComponent::open_optional(
            &root,
            &zed_sidecar_relative_path(filename, "-shm"),
            ZED_SIDECAR_INVALID_REASON,
            false,
        )?;
        let rollback_journal = ZedAdmittedSqliteComponent::open_optional(
            &root,
            &zed_sidecar_relative_path(filename, "-journal"),
            ZED_SIDECAR_INVALID_REASON,
            false,
        )?;
        let family = Self {
            root,
            database,
            wal,
            shared_memory,
            rollback_journal,
        };
        if !family.revalidate_bool()? {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(family)
    }

    fn revision_component(&self) -> String {
        format!(
            "database={};wal={};shm={};journal={}",
            self.database.evidence.revision_component(),
            zed_optional_revision_component(self.wal.as_ref()),
            zed_optional_revision_component(self.shared_memory.as_ref()),
            zed_optional_revision_component(self.rollback_journal.as_ref()),
        )
    }

    fn unsupported_stock_vfs_sidecar(&self) -> Option<SqliteSourceAccessError> {
        let (component, capability) = if self.wal.is_some() {
            (
                SqliteSourceComponent::Wal,
                "the stock Unix VFS cannot bind the opened WAL to the admitted Zed WAL handle",
            )
        } else if self.rollback_journal.is_some() {
            (
                SqliteSourceComponent::RollbackJournal,
                "rollback recovery is not permitted without a root-handle SQLite VFS",
            )
        } else if self.shared_memory.is_some() {
            (
                SqliteSourceComponent::SharedMemory,
                "the stock Unix VFS exposes no native SHM identity",
            )
        } else {
            return None;
        };
        Some(SqliteSourceAccessError::UnsupportedSidecarIdentity {
            component,
            capability,
        })
    }

    fn revalidate_bool(&self) -> ZedNativeResult<bool> {
        let result = (|| -> crate::Result<()> {
            self.database.file.revalidate()?;
            for component in self
                .wal
                .iter()
                .chain(self.shared_memory.iter())
                .chain(self.rollback_journal.iter())
            {
                component.file.revalidate()?;
            }
            self.root.revalidate()
        })();
        match result {
            Ok(()) => Ok(true),
            Err(CaptureError::InvalidProviderTranscriptPath { .. })
            | Err(CaptureError::SourceChangedDuringCapture) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn revalidate(&self) -> ZedNativeResult<()> {
        if self.revalidate_bool()? {
            Ok(())
        } else {
            Err(CaptureError::SourceChangedDuringCapture.into())
        }
    }
}

impl ZedAdmittedSqliteComponent {
    fn open(
        root: &ProviderSourceRoot,
        relative_path: &Path,
        invalid_reason: &'static str,
        is_wal: bool,
    ) -> ZedNativeResult<Self> {
        let file = root.open_file(relative_path).map_err(|error| match error {
            CaptureError::InvalidProviderTranscriptPath { path, .. } => {
                CaptureError::InvalidProviderTranscriptPath {
                    path,
                    reason: invalid_reason,
                }
            }
            error => error,
        })?;
        let evidence = ZedComponentEvidence::read(&file, is_wal)?;
        Ok(Self { file, evidence })
    }

    fn open_optional(
        root: &ProviderSourceRoot,
        relative_path: &Path,
        invalid_reason: &'static str,
        is_wal: bool,
    ) -> ZedNativeResult<Option<Self>> {
        match Self::open(root, relative_path, invalid_reason, is_wal) {
            Ok(component) => Ok(Some(component)),
            Err(ZedNativePathError::Capture(CaptureError::Io(error)))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

fn zed_optional_revision_component(component: Option<&ZedAdmittedSqliteComponent>) -> String {
    component.map_or_else(
        || "absent".to_owned(),
        |component| component.evidence.revision_component(),
    )
}

fn zed_sidecar_relative_path(filename: &std::ffi::OsStr, suffix: &str) -> PathBuf {
    let mut sidecar = filename.to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn zed_absolute_authority_path(path: &Path) -> ZedNativeResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn acquire_immutable_snapshot(path: &Path) -> ZedNativeResult<ZedSnapshotAcquisition> {
    let authority_path = zed_absolute_authority_path(path)?;
    let fallback_locator = authority_path.display().to_string();
    for _ in 0..ZED_SNAPSHOT_ACQUISITION_ATTEMPTS {
        let observed = match ZedAdmittedSqliteFamily::open(&authority_path) {
            Ok(observed) => observed,
            Err(ZedNativePathError::Capture(CaptureError::SourceChangedDuringCapture)) => continue,
            Err(error) => return Err(error),
        };
        if let Some(error) = observed.unsupported_stock_vfs_sidecar() {
            return Err(error.into());
        }
        let connection =
            match open_sqlite_source_snapshot(&authority_path, observed.database.file.file()) {
                Ok(connection) => connection,
                Err(
                    SqliteSourceAccessError::SourceChanged
                    | SqliteSourceAccessError::ConnectionIdentityMismatch,
                ) => continue,
                Err(error) => return Err(error.into()),
            };
        if !observed.revalidate_bool()? {
            continue;
        }
        let snapshot_revision = observed.revision_component();
        return Ok(ZedSnapshotAcquisition::Acquired(Box::new(
            ZedImmutableSqliteSnapshot {
                observed,
                connection: Some(connection),
                physical_locator: fallback_locator.clone(),
                snapshot_revision,
            },
        )));
    }
    Ok(ZedSnapshotAcquisition::Incomplete {
        physical_locator: fallback_locator,
    })
}

pub(super) fn revalidate_zed_snapshot_revision(
    path: &Path,
    expected_revision: &str,
) -> ZedNativeResult<bool> {
    let mut snapshot = match acquire_immutable_snapshot(path)? {
        ZedSnapshotAcquisition::Acquired(snapshot) => *snapshot,
        ZedSnapshotAcquisition::Incomplete { .. } => return Ok(false),
    };
    let matches = snapshot.snapshot_revision == expected_revision;
    snapshot.finish()?;
    Ok(matches)
}

#[cfg(test)]
#[path = "native_path/tests.rs"]
mod tests;

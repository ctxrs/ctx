//! Provider-owned Zed NativePath parsing and Store publication.
//!
//! The provider-private scan feeds the Zed Store vertical directly. Exact temporary
//! output evidence remains available for live Pro hydration.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{limits::Limit, Connection, OpenFlags};
use tempfile::TempDir;
use thiserror::Error;

use crate::provider::sqlite::ProviderSqliteSourceSnapshot;
use crate::{
    complete_content::CompleteContentBodyDigest, CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

mod decode;
mod dto;
mod output;
mod publication;
mod query;
mod source_backed;
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
    #[error("Zed NativePath source has an unsupported schema: {0}")]
    UnsupportedSchema(String),
}

pub(super) type ZedNativeResult<T> = std::result::Result<T, ZedNativePathError>;

pub(super) struct ZedImmutableSqliteSnapshot {
    pub(super) observed: ProviderSqliteSourceSnapshot,
    pub(super) connection: Connection,
    pub(super) physical_locator: String,
    pub(super) snapshot_revision: String,
    _directory: TempDir,
}

pub(super) enum ZedSnapshotAcquisition {
    Acquired(Box<ZedImmutableSqliteSnapshot>),
    Incomplete { physical_locator: String },
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
        ZedNativePathError::UnsupportedSchema(reason) => CaptureError::UnsupportedSchema(reason),
    }
}

fn scan_zed_nativepath_with_finalizer(
    selection: &ZedNativeSourceSelection,
    sink: &mut dyn ZedNativeSink,
    before_final_revalidation: impl FnOnce() -> ZedNativeResult<()>,
) -> ZedNativeResult<ZedNativeScanOutcome> {
    let path = selection.selected_path();
    let snapshot = match acquire_immutable_snapshot(path)? {
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
        &snapshot.connection,
        &snapshot.physical_locator,
        &snapshot.snapshot_revision,
        sink,
    )?;
    before_final_revalidation()?;
    if !snapshot.observed.revalidate(path)? {
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

pub(super) fn acquire_immutable_snapshot(path: &Path) -> ZedNativeResult<ZedSnapshotAcquisition> {
    let fallback_locator = path.display().to_string();
    for _ in 0..ZED_SNAPSHOT_ACQUISITION_ATTEMPTS {
        let observed = match ProviderSqliteSourceSnapshot::read(
            path,
            ZED_SOURCE_INVALID_REASON,
            ZED_SIDECAR_INVALID_REASON,
        ) {
            Ok(observed) => observed,
            Err(CaptureError::SourceChangedDuringCapture) => continue,
            Err(error) => return Err(error.into()),
        };
        let directory = tempfile::Builder::new()
            .prefix("ctx-zed-nativepath-")
            .tempdir()
            .map_err(|source| ZedNativePathError::SystemIo {
                operation: "creating a private Zed SQLite snapshot directory",
                source,
            })?;
        let filename =
            path.file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Zed SQLite path has no file name",
                })?;
        let snapshot_path = directory.path().join(filename);
        copy_snapshot_component(path, &snapshot_path, ZED_SOURCE_INVALID_REASON)?;

        // WAL and rollback journal bytes are authoritative. SHM is only an index; omitting
        // it makes SQLite rebuild private state in the temporary directory.
        for suffix in ["-wal", "-journal"] {
            let source = sqlite_sidecar_path(path, suffix);
            let destination = sqlite_sidecar_path(&snapshot_path, suffix);
            match copy_snapshot_component(&source, &destination, ZED_SIDECAR_INVALID_REASON) {
                Ok(()) => {}
                Err(ZedNativePathError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if !observed.revalidate(path)? {
            continue;
        }

        let connection = Connection::open_with_flags(
            &snapshot_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| ZedNativePathError::SystemSqlite {
            operation: "opening a private Zed SQLite snapshot",
            source,
        })?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
            CaptureError::InvalidPayload(format!(
                "Zed SQLite value byte limit is unrepresentable: \
                 {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
            ))
        })?;
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|source| ZedNativePathError::SystemSqlite {
                operation: "configuring a private Zed SQLite snapshot",
                source,
            })?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(|source| ZedNativePathError::SystemSqlite {
                operation: "configuring a private Zed SQLite snapshot",
                source,
            })?;
        let physical_locator = fs::canonicalize(path)?.display().to_string();
        let snapshot_revision = observed.revision_component();
        return Ok(ZedSnapshotAcquisition::Acquired(Box::new(
            ZedImmutableSqliteSnapshot {
                observed,
                connection,
                physical_locator,
                snapshot_revision,
                _directory: directory,
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
    let observed = ProviderSqliteSourceSnapshot::read(
        path,
        ZED_SOURCE_INVALID_REASON,
        ZED_SIDECAR_INVALID_REASON,
    )?;
    Ok(observed.revision_component() == expected_revision && observed.revalidate(path)?)
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn copy_snapshot_component(
    source: &Path,
    destination: &Path,
    invalid_reason: &'static str,
) -> ZedNativeResult<()> {
    let mut input = open_snapshot_component_without_following(source)?;
    let metadata = input.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: source.to_path_buf(),
            reason: invalid_reason,
        }
        .into());
    }
    let mut output = File::create(destination).map_err(|source| ZedNativePathError::SystemIo {
        operation: "creating a private Zed SQLite snapshot",
        source,
    })?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|source| ZedNativePathError::SystemIo {
                operation: "writing a private Zed SQLite snapshot",
                source,
            })?;
    }
    Ok(())
}

#[cfg(unix)]
fn open_snapshot_component_without_following(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(target_os = "windows")]
fn open_snapshot_component_without_following(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_snapshot_component_without_following(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(test)]
#[path = "native_path/tests.rs"]
mod tests;

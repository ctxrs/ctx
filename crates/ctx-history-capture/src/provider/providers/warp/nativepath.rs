use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{limits::Limit, Connection, OpenFlags};
use tempfile::TempDir;

use super::schema::WarpSqliteSchema;
use crate::provider::sqlite::ProviderSqliteSourceSnapshot;
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

mod decode;
mod lifecycle;
mod publication;
mod query;

pub(super) use lifecycle::{
    WarpNativePersistedState, WarpNativePreparationAction, WarpNativePreparationInputs,
    WarpNativeSourceFailure, WarpNativeSourceFailureKind, WarpNativeSourceIdentity,
    WARP_NATIVE_PARSER_REVISION, WARP_NATIVE_POLICY_REVISION,
};
#[allow(unused_imports)]
// Provider-owned facts consumed by the production Store sink and tests.
pub(super) use publication::{
    WarpNativeCounters, WarpNativeEvent, WarpNativeEventIdentity, WarpNativeFrontier,
    WarpNativeFrontierPhase, WarpNativeHierarchyEdge, WarpNativeIncomplete,
    WarpNativeIncompleteReason, WarpNativeMessageIdentity, WarpNativeOrder,
    WarpNativeOutputRejection, WarpNativeOutputRejectionKind, WarpNativePage,
    WarpNativePageIdentity, WarpNativeProOutputPage, WarpNativeProOutputPageIdentity,
    WarpNativeProOutputPageReceipt, WarpNativeProfile, WarpNativeRejection,
    WarpNativeRejectionKind, WarpNativeScanOutcome, WarpNativeSession, WarpNativeSink,
    WarpNativeSourceAuthority, WARP_NATIVE_PAGE_MAX_BYTES, WARP_NATIVE_PAGE_MAX_ROWS,
};
use query::scan_warp_native_snapshot;

const WARP_SOURCE_INVALID_REASON: &str = "Warp SQLite source must be a regular non-symlink file";
const WARP_SIDECAR_INVALID_REASON: &str = "Warp SQLite sidecar must be a regular non-symlink file";

struct WarpImmutableSqliteSnapshot {
    connection: Connection,
    canonical_route: PathBuf,
    source_identity: WarpNativeSourceIdentity,
    physical_locator: String,
    snapshot_revision: String,
    _directory: TempDir,
}

enum WarpSnapshotAcquisition {
    Acquired(Box<WarpImmutableSqliteSnapshot>),
    Incomplete { physical_locator: String },
}

pub(in super::super) struct WarpNativePreparedSource {
    pub(in super::super) inputs: WarpNativePreparationInputs,
    snapshot: WarpImmutableSqliteSnapshot,
    schema: WarpSqliteSchema,
}

#[cfg(test)]
impl WarpNativePreparedSource {
    fn snapshot_directory(&self) -> &Path {
        self.snapshot._directory.path()
    }
}

pub(in super::super) enum WarpNativePreparationOutcome {
    ExactNoOp {
        #[allow(dead_code)]
        inputs: Box<WarpNativePreparationInputs>,
        persisted_state: Box<WarpNativePersistedState>,
    },
    Ready(Box<WarpNativePreparedSource>),
    Incomplete(WarpNativeSourceFailure),
    Failed(WarpNativeSourceFailure),
}

/// Freezes the exact provider generation for the provider's Store runner.
/// A terminal exact generation can avoid opening SQLite; every other
/// path receives an immutable DB+sidecar snapshot or a typed non-authoritative
/// failure.
pub(in super::super) fn prepare_warp_nativepath_lifecycle(
    path: &Path,
    previous: &[WarpNativePersistedState],
) -> WarpNativePreparationOutcome {
    let observed = match ProviderSqliteSourceSnapshot::read(
        path,
        WARP_SOURCE_INVALID_REASON,
        WARP_SIDECAR_INVALID_REASON,
    ) {
        Ok(observed) => observed,
        Err(error) => return preparation_failure(path, error, false),
    };
    let source_identity = match read_warp_source_identity(path) {
        Ok(identity) => identity,
        Err(error) => return preparation_failure(path, error, false),
    };
    let canonical_route = match fs::canonicalize(path) {
        Ok(route) => route,
        Err(error) => return preparation_failure(path, error.into(), false),
    };
    let snapshot_revision = observed.revision_component();
    let exact_previous = previous
        .iter()
        .filter(|state| {
            state.is_supported()
                && state.canonical_route == canonical_route
                && state.source_identity == source_identity
                && state.snapshot_revision == snapshot_revision
                && state.source_identity.supports_exact_replay()
        })
        .collect::<Vec<_>>();
    if let [state] = exact_previous.as_slice() {
        if state.checkpoint_is_terminal() {
            let still_exact = observed.revalidate(path).unwrap_or(false)
                && read_warp_source_identity(path)
                    .is_ok_and(|current_identity| current_identity == source_identity)
                && observed.revalidate(path).unwrap_or(false);
            if still_exact {
                return WarpNativePreparationOutcome::ExactNoOp {
                    inputs: Box::new(WarpNativePreparationInputs {
                        canonical_route,
                        source_identity,
                        snapshot_revision,
                        capability_digest: state.capability_digest.clone(),
                        parser_revision: WARP_NATIVE_PARSER_REVISION,
                        policy_revision: WARP_NATIVE_POLICY_REVISION,
                        action: WarpNativePreparationAction::ExactNoOp,
                        resume_frontier: None,
                    }),
                    persisted_state: Box::new((*state).clone()),
                };
            }
        }
    }

    let snapshot = match acquire_immutable_snapshot(path) {
        Ok(WarpSnapshotAcquisition::Acquired(snapshot)) => *snapshot,
        Ok(WarpSnapshotAcquisition::Incomplete { physical_locator }) => {
            return WarpNativePreparationOutcome::Incomplete(WarpNativeSourceFailure {
                kind: WarpNativeSourceFailureKind::SourceChanged,
                canonical_route: PathBuf::from(physical_locator),
                detail: "Warp source changed during immutable SQLite snapshot acquisition"
                    .to_owned(),
            });
        }
        Err(error) => return preparation_failure(path, error, false),
    };
    let schema = match WarpSqliteSchema::detect(&snapshot.connection) {
        Ok(schema) => schema,
        Err(error) => return preparation_failure(&snapshot.canonical_route, error, true),
    };
    if let Err(error) = validate_snapshot_cursor_compatibility(&snapshot.connection) {
        return preparation_failure(&snapshot.canonical_route, error, true);
    }
    let resume_frontier = exact_previous
        .first()
        .filter(|state| {
            !state.checkpoint_is_terminal()
                && snapshot.canonical_route == state.canonical_route
                && snapshot.source_identity == state.source_identity
                && snapshot.snapshot_revision == state.snapshot_revision
                && schema.capability_digest == state.capability_digest
        })
        .map(|state| state.checkpoint_frontier().clone());
    let action = if resume_frontier.is_some() {
        WarpNativePreparationAction::ResumeExactSnapshot
    } else {
        WarpNativePreparationAction::AuthoritativeScan
    };
    let inputs = WarpNativePreparationInputs {
        canonical_route: snapshot.canonical_route.clone(),
        source_identity: snapshot.source_identity.clone(),
        snapshot_revision: snapshot.snapshot_revision.clone(),
        capability_digest: schema.capability_digest.clone(),
        parser_revision: WARP_NATIVE_PARSER_REVISION,
        policy_revision: WARP_NATIVE_POLICY_REVISION,
        action,
        resume_frontier,
    };
    if let Err(error) = validate_resume_frontier_compatibility(
        &snapshot.connection,
        inputs.resume_frontier.as_ref(),
    ) {
        return preparation_failure(&snapshot.canonical_route, error, true);
    }
    let initial_frontier = inputs.resume_frontier.clone().unwrap_or_default();
    if let Err(error) = inputs.persisted_state_at(initial_frontier) {
        return preparation_failure(&snapshot.canonical_route, error, true);
    }
    WarpNativePreparationOutcome::Ready(Box::new(WarpNativePreparedSource {
        inputs,
        snapshot,
        schema,
    }))
}

fn preparation_failure(
    path: &Path,
    error: CaptureError,
    schema_stage: bool,
) -> WarpNativePreparationOutcome {
    let failure = WarpNativeSourceFailure::from_capture(path, error, schema_stage);
    if failure.kind == WarpNativeSourceFailureKind::SourceChanged {
        WarpNativePreparationOutcome::Incomplete(failure)
    } else {
        WarpNativePreparationOutcome::Failed(failure)
    }
}

pub(in super::super) fn scan_prepared_warp_nativepath(
    prepared: WarpNativePreparedSource,
    profile: WarpNativeProfile,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeScanOutcome> {
    scan_acquired_warp_nativepath(
        prepared.snapshot,
        prepared.schema,
        prepared.inputs,
        profile,
        sink,
    )
}

#[allow(dead_code)]
pub(super) fn scan_warp_nativepath(
    path: &Path,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeScanOutcome> {
    scan_warp_nativepath_with_profile(path, WarpNativeProfile::CoreOnly, sink)
}

#[allow(dead_code)]
pub(super) fn scan_warp_nativepath_with_profile(
    path: &Path,
    profile: WarpNativeProfile,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeScanOutcome> {
    scan_warp_nativepath_with_certification_hook(path, profile, sink, || Ok(()))
}

#[allow(dead_code)]
fn scan_warp_nativepath_with_certification_hook(
    path: &Path,
    profile: WarpNativeProfile,
    sink: &mut dyn WarpNativeSink,
    before_certification: impl FnOnce() -> Result<()>,
) -> Result<WarpNativeScanOutcome> {
    let snapshot = match acquire_immutable_snapshot_with_hook(path, before_certification)? {
        WarpSnapshotAcquisition::Acquired(snapshot) => *snapshot,
        WarpSnapshotAcquisition::Incomplete { physical_locator } => {
            return Ok(WarpNativeScanOutcome::Incomplete(WarpNativeIncomplete {
                source_complete: false,
                reason: WarpNativeIncompleteReason::SnapshotCertificationRace,
                physical_locator,
                pages_emitted: 0,
                pro_output_pages_emitted: 0,
                counters: Default::default(),
            }));
        }
    };
    let schema = WarpSqliteSchema::detect(&snapshot.connection)?;
    validate_snapshot_cursor_compatibility(&snapshot.connection)?;
    let inputs = WarpNativePreparationInputs {
        canonical_route: snapshot.canonical_route.clone(),
        source_identity: snapshot.source_identity.clone(),
        snapshot_revision: snapshot.snapshot_revision.clone(),
        capability_digest: schema.capability_digest.clone(),
        parser_revision: WARP_NATIVE_PARSER_REVISION,
        policy_revision: WARP_NATIVE_POLICY_REVISION,
        action: WarpNativePreparationAction::AuthoritativeScan,
        resume_frontier: None,
    };
    scan_acquired_warp_nativepath(snapshot, schema, inputs, profile, sink)
}

fn validate_snapshot_cursor_compatibility(connection: &Connection) -> Result<()> {
    let invalid_rowid: bool = connection.query_row(
        "select exists(select 1 from agent_conversations where rowid <= 0)
             or exists(select 1 from agent_tasks where rowid <= 0)",
        [],
        |row| row.get(0),
    )?;
    if invalid_rowid {
        return Err(CaptureError::InvalidPayload(
            "Warp NativePath requires positive 64-bit source rowids for bounded restart cursors"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_resume_frontier_compatibility(
    connection: &Connection,
    frontier: Option<&WarpNativeFrontier>,
) -> Result<()> {
    let Some(frontier) = frontier else {
        return Ok(());
    };
    let (table, rowid) = match frontier.phase {
        WarpNativeFrontierPhase::Start => return Ok(()),
        WarpNativeFrontierPhase::Conversations => (
            "agent_conversations",
            frontier.last_conversation_rowid.ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp conversation resume frontier omitted its rowid".to_owned(),
                )
            })?,
        ),
        WarpNativeFrontierPhase::Tasks => (
            "agent_tasks",
            frontier.last_task_rowid.ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp task resume frontier omitted its rowid".to_owned(),
                )
            })?,
        ),
    };
    let exists: bool = connection.query_row(
        &format!("select exists(select 1 from {table} where rowid = ?1)"),
        [rowid],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(CaptureError::InvalidPayload(
            "Warp resume frontier does not exist in the certified immutable snapshot".to_owned(),
        ));
    }
    Ok(())
}

fn scan_acquired_warp_nativepath(
    snapshot: WarpImmutableSqliteSnapshot,
    schema: WarpSqliteSchema,
    inputs: WarpNativePreparationInputs,
    profile: WarpNativeProfile,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeScanOutcome> {
    let initial_frontier = inputs.resume_frontier.clone().unwrap_or_default();
    inputs.persisted_state_at(initial_frontier)?;
    validate_resume_frontier_compatibility(&snapshot.connection, inputs.resume_frontier.as_ref())?;
    let result = scan_warp_native_snapshot(
        &snapshot.connection,
        &schema,
        profile,
        inputs.resume_frontier.clone(),
        sink,
    )?;

    let zero_authoritative_rows = result.eof.frontier().completed_conversation_rows == 0
        && result.eof.frontier().completed_task_rows == 0;
    let has_useful_content = result.eof.frontier().completed_conversation_rows > 0
        || result.eof.frontier().retained_events > 0;
    let persisted_state = inputs.persisted_state_at_eof(result.eof)?;
    Ok(WarpNativeScanOutcome::Complete(WarpNativeSourceAuthority {
        source_complete: true,
        zero_authoritative_rows,
        has_useful_content,
        physical_locator: snapshot.physical_locator,
        snapshot_revision: snapshot.snapshot_revision,
        capability_digest: schema.capability_digest,
        source_integrity_digest: result.source_integrity_digest,
        core_generation_digest: result.core_generation_digest,
        persisted_state: Box::new(persisted_state),
        pages_emitted: result.pages_emitted,
        pro_output_pages_emitted: result.pro_output_pages_emitted,
        counters: result.counters,
    }))
}

fn acquire_immutable_snapshot(path: &Path) -> Result<WarpSnapshotAcquisition> {
    acquire_immutable_snapshot_with_hook(path, || Ok(()))
}

fn acquire_immutable_snapshot_with_hook(
    path: &Path,
    before_certification: impl FnOnce() -> Result<()>,
) -> Result<WarpSnapshotAcquisition> {
    let observed = ProviderSqliteSourceSnapshot::read(
        path,
        WARP_SOURCE_INVALID_REASON,
        WARP_SIDECAR_INVALID_REASON,
    )?;
    let source_identity = read_warp_source_identity(path)?;
    let canonical_route = fs::canonicalize(path)?;
    let physical_locator = canonical_route.display().to_string();
    let directory = tempfile::Builder::new()
        .prefix("ctx-warp-nativepath-")
        .tempdir()?;
    let filename = path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Warp SQLite path has no file name",
        })?;
    let snapshot_path = directory.path().join(filename);
    copy_snapshot_component(path, &snapshot_path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let source = sqlite_sidecar_path(path, suffix);
        let destination = sqlite_sidecar_path(&snapshot_path, suffix);
        match copy_snapshot_component(&source, &destination) {
            Ok(()) => {}
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    before_certification()?;
    let certified = observed.revalidate(path)?
        && read_warp_source_identity(path)? == source_identity
        && fs::canonicalize(path).is_ok_and(|route| route == canonical_route);
    if !certified {
        return Ok(WarpSnapshotAcquisition::Incomplete { physical_locator });
    }

    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "Warp SQLite value byte limit is unrepresentable: \
             {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
        ))
    })?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    let snapshot_revision = observed.revision_component();
    Ok(WarpSnapshotAcquisition::Acquired(Box::new(
        WarpImmutableSqliteSnapshot {
            connection,
            canonical_route,
            source_identity,
            physical_locator,
            snapshot_revision,
            _directory: directory,
        },
    )))
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn copy_snapshot_component(source: &Path, destination: &Path) -> Result<()> {
    let mut input = open_snapshot_component_without_following(source)?;
    let metadata = input.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: source.to_path_buf(),
            reason: WARP_SIDECAR_INVALID_REASON,
        });
    }
    let mut output = File::create(destination)?;
    io::copy(&mut input, &mut output)?;
    Ok(())
}

#[cfg(unix)]
fn open_snapshot_component_without_following(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}

#[cfg(target_os = "windows")]
fn open_snapshot_component_without_following(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_snapshot_component_without_following(path: &Path) -> Result<File> {
    Ok(File::open(path)?)
}

#[cfg(unix)]
fn read_warp_source_identity(path: &Path) -> Result<WarpNativeSourceIdentity> {
    use std::os::unix::fs::MetadataExt;

    let file = open_snapshot_component_without_following(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: WARP_SOURCE_INVALID_REASON,
        });
    }
    Ok(WarpNativeSourceIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(target_os = "windows")]
fn read_warp_source_identity(path: &Path) -> Result<WarpNativeSourceIdentity> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };

    let file = open_snapshot_component_without_following(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: WARP_SOURCE_INVALID_REASON,
        });
    }
    let mut id_info = FILE_ID_INFO::default();
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            &mut id_info as *mut FILE_ID_INFO as *mut std::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(WarpNativeSourceIdentity::Windows {
        volume_serial: id_info.VolumeSerialNumber,
        file_id: id_info.FileId.Identifier,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
fn read_warp_source_identity(path: &Path) -> Result<WarpNativeSourceIdentity> {
    let metadata = open_snapshot_component_without_following(path)?.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: WARP_SOURCE_INVALID_REASON,
        });
    }
    Ok(WarpNativeSourceIdentity::UnsupportedPlatform)
}

#[cfg(test)]
#[path = "nativepath/tests.rs"]
mod tests;

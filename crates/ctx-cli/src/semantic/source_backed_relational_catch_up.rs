use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result};
#[cfg(test)]
use ctx_history_core::SourceKey;
use ctx_history_index::{durable_atomic_replace_file, VerifiedIndex, MAX_SOURCE_EVENT_PAGE_ITEMS};
use ctx_history_relational::{
    CommittedCoreGeneration, RelationalProjectionError, RelationalProjectionMetadata,
    RelationalProjectionPlan, RelationalProjectionReceipt, RelationalProjectionRecord,
    RelationalProjectionStatus, SourceBackedRelationalProjection, RELATIONAL_MATERIALIZER_REVISION,
    RELATIONAL_PROJECTION_SCHEMA_VERSION,
};
use serde_json::Value;
use thiserror::Error;

use crate::source_sql::sql_compatibility_path;

use super::{
    paths_status::{
        daemon_jobs_path, open_or_create_pid_lock_file, read_daemon_job_status,
        write_daemon_job_status,
    },
    source_backed_refresh_coordinator::{
        daemon_cycle_verified_index, nonzero_duration_micros, open_verified_index,
        source_backed_index_root,
    },
};

const SOURCE_BACKED_RELATIONAL_STATUS_FILE: &str = "relational-catch-up.json";
const SOURCE_BACKED_RELATIONAL_LOCK_FILE: &str = "relational-catch-up.lock";

mod core_metadata;
mod record_stream;
mod status;

use core_metadata::{committed_generation, relational_source_metadata};
use record_stream::{RelationalRecordStream, RelationalSourceSelection};
use status::SourceBackedRelationalCatchUpStatus;

#[derive(Debug, Error)]
enum SourceBackedRelationalCatchUpError {
    #[error(
        "source_relational_generation_mismatch: expected Core generation {expected}, but exact index carries {actual}"
    )]
    GenerationMismatch { expected: String, actual: String },
    #[error("source_relational_index_unavailable: {0}")]
    IndexUnavailable(String),
    #[error("source_relational_metadata_invalid: {0}")]
    InvalidMetadata(String),
    #[error("source_relational_receipt_mismatch: {0}")]
    ReceiptMismatch(String),
    #[error("source_relational_projection_unavailable: {0}")]
    Projection(String),
    #[error("source_relational_publication_failed: {0}")]
    Publication(#[from] RelationalPublicationError),
}

impl SourceBackedRelationalCatchUpError {
    fn code(&self) -> &'static str {
        match self {
            Self::GenerationMismatch { .. } => "source_relational_generation_mismatch",
            Self::IndexUnavailable(_) => "source_relational_index_unavailable",
            Self::InvalidMetadata(_) => "source_relational_metadata_invalid",
            Self::ReceiptMismatch(_) => "source_relational_receipt_mismatch",
            Self::Projection(_) => "source_relational_projection_unavailable",
            Self::Publication(_) => "source_relational_publication_failed",
        }
    }

    fn projection(error: impl std::fmt::Display) -> Self {
        Self::Projection(error.to_string())
    }
}

#[derive(Debug, Error)]
enum RelationalPublicationError {
    #[error("inspect relational destination {}: {source}", path.display())]
    InspectDestination {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("seal prior relational projection {} before snapshot: {source}", path.display())]
    SealPrior {
        path: PathBuf,
        #[source]
        source: RelationalProjectionError,
    },
    #[error("synchronize sealed prior relational projection {}: {source}", path.display())]
    SyncPrior {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "copy sealed prior relational projection {} to candidate {}: {source}",
        source_path.display(), candidate_path.display()
    )]
    SnapshotPrior {
        source_path: PathBuf,
        candidate_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{operation} {}: {source}", path.display())]
    RemoveFile {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("seal relational candidate {} before publication: {source}", path.display())]
    SealCandidate {
        path: PathBuf,
        #[source]
        source: RelationalProjectionError,
    },
    #[error("synchronize sealed relational candidate {}: {source}", path.display())]
    SyncCandidate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("verify sealed relational candidate {} before publication: {detail}", path.display())]
    CandidateVerification { path: PathBuf, detail: String },
    #[error("verify existing relational projection {} before no-op success: {detail}", path.display())]
    ExistingVerification { path: PathBuf, detail: String },
    #[error("verify committed live relational catch-up {}: {detail}", path.display())]
    LiveVerification { path: PathBuf, detail: String },
    #[error(
        "atomically replace relational projection {} with {}: {source}; the destination was not replaced, so any prior projection remains visible",
        destination.display(), candidate.display()
    )]
    AtomicReplace {
        candidate: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "relational replacement of {} became visible but its durability barrier failed: {source}; retry to re-verify and complete publication",
        destination.display()
    )]
    PublishedDurability {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "could not determine whether relational replacement of {} became visible after {replace_error}: {source}; inspect the destination before retrying",
        destination.display()
    )]
    PublicationStateUnknown {
        destination: PathBuf,
        replace_error: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "reopen and verify published relational projection {} after replacement became visible: {detail}",
        path.display()
    )]
    PublishedVerification { path: PathBuf, detail: String },
}

pub(super) struct SourceBackedRelationalCatchUpRun {
    pub(super) status: Value,
    pub(super) did_work: bool,
}

struct ProjectionOutcome {
    receipt: RelationalProjectionReceipt,
    did_work: bool,
}

enum PreparedProjection {
    NoOp(RelationalProjectionReceipt),
    RebuildCandidate {
        projection: SourceBackedRelationalProjection,
        path: PathBuf,
    },
    LiveCatchUp(SourceBackedRelationalProjection),
}

struct RelationalCatchUpLock {
    file: fs::File,
}

impl RelationalCatchUpLock {
    fn acquire(data_root: &Path) -> Result<Self> {
        let jobs = daemon_jobs_path(data_root);
        fs::create_dir_all(&jobs)
            .with_context(|| format!("create relational catch-up jobs root {}", jobs.display()))?;
        ctx_history_core::platform_security::restrict_private_directory(&jobs)
            .with_context(|| format!("secure relational catch-up jobs root {}", jobs.display()))?;
        let path = jobs.join(SOURCE_BACKED_RELATIONAL_LOCK_FILE);
        let (file, _) = open_or_create_pid_lock_file(&path)
            .with_context(|| format!("open relational catch-up lock {}", path.display()))?;
        fs2::FileExt::lock_exclusive(&file)
            .with_context(|| format!("acquire relational catch-up lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for RelationalCatchUpLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(super) fn run_after_core_publication(
    data_root: &Path,
    core_generation_id: &str,
) -> Result<SourceBackedRelationalCatchUpRun> {
    let _lock = RelationalCatchUpLock::acquire(data_root)?;
    run_with(data_root, core_generation_id, project_exact_core_generation)
}

pub(super) fn generation_needs_catch_up(data_root: &Path, core_generation_id: &str) -> bool {
    !read_status(data_root).is_some_and(|status| status.is_completed_for(core_generation_id))
        || ready_projection_metadata(data_root, core_generation_id).is_none()
}

pub(super) fn status_generation(data_root: &Path) -> Option<String> {
    read_status(data_root).map(|status| status.core_generation_id)
}

pub(super) fn read_status_json(data_root: &Path) -> Option<Value> {
    read_daemon_job_status(&status_path(data_root))
}

pub(super) fn persist_status_json(data_root: &Path, status: &Value) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), status)
}

fn run_with<Project>(
    data_root: &Path,
    core_generation_id: &str,
    project: Project,
) -> Result<SourceBackedRelationalCatchUpRun>
where
    Project: FnOnce(
        &Path,
        &str,
    )
        -> std::result::Result<ProjectionOutcome, SourceBackedRelationalCatchUpError>,
{
    let prior = read_status(data_root);
    if prior
        .as_ref()
        .is_some_and(|status| status.is_completed_for(core_generation_id))
        && ready_projection_metadata(data_root, core_generation_id).is_some()
    {
        let Some(prior) = prior else {
            anyhow::bail!("completed relational status disappeared");
        };
        return Ok(SourceBackedRelationalCatchUpRun {
            status: prior.to_json()?,
            did_work: false,
        });
    }

    let frontier = projection_metadata(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let attempt_started = Instant::now();
    let pending = SourceBackedRelationalCatchUpStatus::pending(
        core_generation_id,
        attempts,
        frontier.as_ref(),
    );
    persist_status(data_root, &pending)?;

    match project(data_root, core_generation_id) {
        Ok(outcome) => {
            if let Err(error) =
                validate_receipt_core_generation(core_generation_id, &outcome.receipt)
            {
                let frontier = projection_metadata(data_root);
                let failed = pending
                    .error(error, frontier.as_ref())
                    .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
                persist_status(data_root, &failed)?;
                return Ok(SourceBackedRelationalCatchUpRun {
                    status: failed.to_json()?,
                    did_work: false,
                });
            }
            let completed = pending
                .completed(&outcome.receipt)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &completed)?;
            Ok(SourceBackedRelationalCatchUpRun {
                status: completed.to_json()?,
                did_work: outcome.did_work,
            })
        }
        Err(error) => {
            let frontier = projection_metadata(data_root);
            let failed = pending
                .error(error, frontier.as_ref())
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &failed)?;
            Ok(SourceBackedRelationalCatchUpRun {
                status: failed.to_json()?,
                did_work: false,
            })
        }
    }
}

fn project_exact_core_generation(
    data_root: &Path,
    core_generation_id: &str,
) -> std::result::Result<ProjectionOutcome, SourceBackedRelationalCatchUpError> {
    let index_root = source_backed_index_root(data_root);
    let index = match daemon_cycle_verified_index(data_root, core_generation_id) {
        Some(index) => index,
        None => Arc::new(open_verified_index(&index_root).map_err(|error| {
            SourceBackedRelationalCatchUpError::IndexUnavailable(format!(
                "open verified Core index {}: {error}",
                index_root.display()
            ))
        })?),
    };
    if index.generation_id() != core_generation_id {
        return Err(SourceBackedRelationalCatchUpError::GenerationMismatch {
            expected: core_generation_id.to_owned(),
            actual: index.generation_id().to_owned(),
        });
    }

    let generation = committed_generation(&index)?;
    let projection_path = sql_compatibility_path(data_root);
    let prepared = prepare_projection(&projection_path, &generation)?;
    match prepared {
        PreparedProjection::NoOp(receipt) => {
            validate_projection_receipt(&generation, &receipt)?;
            Ok(ProjectionOutcome {
                receipt,
                did_work: false,
            })
        }
        PreparedProjection::RebuildCandidate {
            mut projection,
            path: candidate_path,
        } => {
            match projection
                .plan_generation(&generation)
                .map_err(SourceBackedRelationalCatchUpError::projection)?
            {
                RelationalProjectionPlan::Rebuild => {}
                _ => {
                    return Err(SourceBackedRelationalCatchUpError::Projection(
                        "relational rebuild plan changed during one pinned run".to_owned(),
                    ));
                }
            }
            let records = relational_record_stream(
                &index,
                RelationalSourceSelection::All,
                &generation,
                MAX_SOURCE_EVENT_PAGE_ITEMS,
            );
            let receipt = projection
                .rebuild_stream(&generation, records)
                .map_err(SourceBackedRelationalCatchUpError::projection)?;
            validate_projection_receipt(&generation, &receipt)?;
            finish_candidate_publication(
                projection,
                &candidate_path,
                &projection_path,
                &generation,
                &receipt,
            )?;
            Ok(ProjectionOutcome {
                receipt,
                did_work: true,
            })
        }
        PreparedProjection::LiveCatchUp(mut projection) => {
            let changed_source_ids = match projection
                .plan_generation(&generation)
                .map_err(SourceBackedRelationalCatchUpError::projection)?
            {
                RelationalProjectionPlan::CatchUp { changed_source_ids } => changed_source_ids,
                _ => {
                    return Err(SourceBackedRelationalCatchUpError::Projection(
                        "relational catch-up plan changed during one pinned run".to_owned(),
                    ));
                }
            };
            let records = relational_record_stream(
                &index,
                RelationalSourceSelection::Changed(&changed_source_ids),
                &generation,
                MAX_SOURCE_EVENT_PAGE_ITEMS,
            );
            let receipt = projection
                .catch_up_stream(&generation, records)
                .map_err(SourceBackedRelationalCatchUpError::projection)?;
            validate_projection_receipt(&generation, &receipt)?;
            drop(projection);
            verify_projection_identity(&projection_path, &generation, &receipt).map_err(
                |detail| RelationalPublicationError::LiveVerification {
                    path: projection_path,
                    detail,
                },
            )?;
            Ok(ProjectionOutcome {
                receipt,
                did_work: true,
            })
        }
    }
}

fn validate_receipt_core_generation(
    expected: &str,
    receipt: &RelationalProjectionReceipt,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    if receipt.core_generation_id == expected {
        Ok(())
    } else {
        Err(SourceBackedRelationalCatchUpError::ReceiptMismatch(
            format!(
                "receipt carries Core generation {}, expected {expected}",
                receipt.core_generation_id
            ),
        ))
    }
}

fn validate_projection_receipt(
    generation: &CommittedCoreGeneration,
    receipt: &RelationalProjectionReceipt,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    validate_receipt_core_generation(&generation.generation_id, receipt)?;
    let source_count = u64::try_from(generation.sources.len()).map_err(|_| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(
            "Core source count does not fit in a relational receipt".to_owned(),
        )
    })?;
    let mut mismatches = Vec::new();
    compare_projection_field(
        &mut mismatches,
        "relational_schema_version",
        &receipt.relational_schema_version,
        &RELATIONAL_PROJECTION_SCHEMA_VERSION,
    );
    compare_projection_field(
        &mut mismatches,
        "materializer_revision",
        &receipt.materializer_revision,
        &RELATIONAL_MATERIALIZER_REVISION,
    );
    compare_projection_field(
        &mut mismatches,
        "source_count",
        &receipt.source_count,
        &source_count,
    );
    compare_projection_field(
        &mut mismatches,
        "event_count",
        &receipt.event_count,
        &generation.indexed_documents,
    );
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(SourceBackedRelationalCatchUpError::ReceiptMismatch(
            mismatches.join("; "),
        ))
    }
}

fn relational_record_stream<'a>(
    index: &'a VerifiedIndex,
    selection: RelationalSourceSelection<'a>,
    generation: &CommittedCoreGeneration,
    page_size: usize,
) -> impl Iterator<Item = std::result::Result<RelationalProjectionRecord, RelationalProjectionError>> + 'a
{
    let expected_sources = generation
        .sources
        .iter()
        .map(|metadata| {
            (
                metadata.source.identity().as_uuid().to_string(),
                metadata.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    // The page stream constructs metadata from this same verified manifest.
    // Replace only its revision identity with the aggregate-bound value.
    RelationalRecordStream::new(index, selection, page_size).map(move |record| match record? {
        RelationalProjectionRecord::BeginSource(actual) => {
            let source_id = actual.source.identity().as_uuid().to_string();
            let expected = expected_sources.get(&source_id).ok_or_else(|| {
                RelationalProjectionError::InvalidRecord(format!(
                    "source {source_id} is absent from the pinned Core generation"
                ))
            })?;
            Ok(RelationalProjectionRecord::BeginSource(Box::new(
                expected.clone(),
            )))
        }
        record => Ok(record),
    })
}

fn prepare_projection(
    path: &Path,
    generation: &CommittedCoreGeneration,
) -> std::result::Result<PreparedProjection, SourceBackedRelationalCatchUpError> {
    let exists =
        path.try_exists()
            .map_err(|source| RelationalPublicationError::InspectDestination {
                path: path.to_path_buf(),
                source,
            })?;
    if !exists {
        return open_empty_candidate(path);
    }

    match SourceBackedRelationalProjection::open_read_only(path) {
        Ok(projection) => {
            let plan = projection
                .plan_generation(generation)
                .map_err(SourceBackedRelationalCatchUpError::projection)?;
            match plan {
                RelationalProjectionPlan::NoOp(receipt) => {
                    drop(projection);
                    verify_projection_identity(path, generation, &receipt).map_err(|detail| {
                        RelationalPublicationError::ExistingVerification {
                            path: path.to_path_buf(),
                            detail,
                        }
                    })?;
                    Ok(PreparedProjection::NoOp(receipt))
                }
                RelationalProjectionPlan::Rebuild => {
                    drop(projection);
                    snapshot_prior_projection(path)
                }
                RelationalProjectionPlan::CatchUp { .. } => {
                    drop(projection);
                    // The relational crate publishes rows and active-generation
                    // metadata in one immediate SQLite transaction. Keep the
                    // live file as that atomic boundary; candidates are only
                    // for deterministic rebuild-and-replace.
                    let projection = SourceBackedRelationalProjection::open(path)
                        .map_err(SourceBackedRelationalCatchUpError::projection)?;
                    Ok(PreparedProjection::LiveCatchUp(projection))
                }
            }
        }
        Err(
            RelationalProjectionError::MissingSchema
            | RelationalProjectionError::UnsupportedSchema { .. }
            | RelationalProjectionError::IncompatibleState(_)
            | RelationalProjectionError::MissingStableView(_),
        ) => open_empty_candidate(path),
        Err(error) => Err(SourceBackedRelationalCatchUpError::projection(error)),
    }
}

fn open_empty_candidate(
    destination: &Path,
) -> std::result::Result<PreparedProjection, SourceBackedRelationalCatchUpError> {
    let candidate = candidate_projection_path(destination);
    reset_candidate_projection(&candidate)?;
    let projection = SourceBackedRelationalProjection::open(&candidate)
        .map_err(SourceBackedRelationalCatchUpError::projection)?;
    Ok(PreparedProjection::RebuildCandidate {
        projection,
        path: candidate,
    })
}

fn snapshot_prior_projection(
    destination: &Path,
) -> std::result::Result<PreparedProjection, SourceBackedRelationalCatchUpError> {
    let mut prior = SourceBackedRelationalProjection::open(destination).map_err(|source| {
        RelationalPublicationError::SealPrior {
            path: destination.to_path_buf(),
            source,
        }
    })?;
    prior
        .seal_for_replacement()
        .map_err(|source| RelationalPublicationError::SealPrior {
            path: destination.to_path_buf(),
            source,
        })?;
    drop(prior);
    remove_sqlite_sidecars(destination, "remove sealed prior relational sidecar")?;
    fs::File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|source| RelationalPublicationError::SyncPrior {
            path: destination.to_path_buf(),
            source,
        })?;

    let candidate = candidate_projection_path(destination);
    reset_candidate_projection(&candidate)?;
    fs::copy(destination, &candidate).map_err(|source| {
        RelationalPublicationError::SnapshotPrior {
            source_path: destination.to_path_buf(),
            candidate_path: candidate.clone(),
            source,
        }
    })?;
    let projection = SourceBackedRelationalProjection::open(&candidate)
        .map_err(SourceBackedRelationalCatchUpError::projection)?;
    Ok(PreparedProjection::RebuildCandidate {
        projection,
        path: candidate,
    })
}

fn candidate_projection_path(path: &Path) -> PathBuf {
    path_with_suffix(path, ".core-candidate")
}

fn reset_candidate_projection(
    path: &Path,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    for candidate in [
        path.to_path_buf(),
        sqlite_sidecar_path(path, "-wal"),
        sqlite_sidecar_path(path, "-shm"),
    ] {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RelationalPublicationError::RemoveFile {
                    operation: "reset disposable relational candidate",
                    path: candidate,
                    source: error,
                }
                .into());
            }
        }
    }
    Ok(())
}

fn remove_sqlite_sidecars(
    path: &Path,
    operation: &'static str,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    for sidecar in [
        sqlite_sidecar_path(path, "-wal"),
        sqlite_sidecar_path(path, "-shm"),
    ] {
        match fs::remove_file(&sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(RelationalPublicationError::RemoveFile {
                    operation,
                    path: sidecar,
                    source,
                }
                .into());
            }
        }
    }
    Ok(())
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    path_with_suffix(path, suffix)
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut suffixed = path.as_os_str().to_os_string();
    suffixed.push(suffix);
    PathBuf::from(suffixed)
}

fn finish_candidate_publication(
    projection: SourceBackedRelationalProjection,
    candidate: &Path,
    destination: &Path,
    generation: &CommittedCoreGeneration,
    receipt: &RelationalProjectionReceipt,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    finish_candidate_publication_with(
        projection,
        candidate,
        destination,
        generation,
        receipt,
        durable_atomic_replace_file,
    )
}

fn finish_candidate_publication_with<Replace>(
    mut projection: SourceBackedRelationalProjection,
    candidate: &Path,
    destination: &Path,
    generation: &CommittedCoreGeneration,
    receipt: &RelationalProjectionReceipt,
    replace: Replace,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError>
where
    Replace: FnOnce(&Path, &Path) -> io::Result<()>,
{
    projection.seal_for_replacement().map_err(|source| {
        RelationalPublicationError::SealCandidate {
            path: candidate.to_path_buf(),
            source,
        }
    })?;
    drop(projection);
    remove_sqlite_sidecars(candidate, "remove sealed relational candidate sidecar")?;
    fs::File::open(candidate)
        .and_then(|file| file.sync_all())
        .map_err(|source| RelationalPublicationError::SyncCandidate {
            path: candidate.to_path_buf(),
            source,
        })?;
    verify_projection_identity(candidate, generation, receipt).map_err(|detail| {
        RelationalPublicationError::CandidateVerification {
            path: candidate.to_path_buf(),
            detail,
        }
    })?;

    // A sealed prior main file remains complete without its WAL/SHM files. Do
    // this before the one atomic replacement so stale sidecars can never be
    // paired with the newly published SQLite header.
    remove_sqlite_sidecars(
        destination,
        "remove prior relational sidecar before publication",
    )?;
    if let Err(source) = replace(candidate, destination) {
        return match candidate.try_exists() {
            Ok(true) => Err(RelationalPublicationError::AtomicReplace {
                candidate: candidate.to_path_buf(),
                destination: destination.to_path_buf(),
                source,
            }
            .into()),
            Ok(false) => Err(RelationalPublicationError::PublishedDurability {
                destination: destination.to_path_buf(),
                source,
            }
            .into()),
            Err(state_source) => Err(RelationalPublicationError::PublicationStateUnknown {
                destination: destination.to_path_buf(),
                replace_error: source.to_string(),
                source: state_source,
            }
            .into()),
        };
    }

    verify_projection_identity(destination, generation, receipt).map_err(|detail| {
        RelationalPublicationError::PublishedVerification {
            path: destination.to_path_buf(),
            detail,
        }
        .into()
    })
}

fn verify_projection_identity(
    path: &Path,
    generation: &CommittedCoreGeneration,
    receipt: &RelationalProjectionReceipt,
) -> std::result::Result<(), String> {
    let projection = SourceBackedRelationalProjection::open_read_only(path)
        .map_err(|error| format!("open read-only projection: {error}"))?;
    let metadata = projection
        .metadata()
        .map_err(|error| format!("read projection metadata: {error}"))?;
    let mut mismatches = Vec::new();
    compare_projection_field(
        &mut mismatches,
        "receipt.core_generation_id",
        &receipt.core_generation_id,
        &generation.generation_id,
    );
    compare_projection_field(
        &mut mismatches,
        "receipt.relational_schema_version",
        &receipt.relational_schema_version,
        &RELATIONAL_PROJECTION_SCHEMA_VERSION,
    );
    compare_projection_field(
        &mut mismatches,
        "receipt.materializer_revision",
        &receipt.materializer_revision,
        &RELATIONAL_MATERIALIZER_REVISION,
    );
    compare_projection_field(
        &mut mismatches,
        "status",
        &metadata.status,
        &RelationalProjectionStatus::Ready,
    );
    compare_projection_field(
        &mut mismatches,
        "active_core_generation_id",
        &metadata.active_core_generation_id.as_deref(),
        &Some(generation.generation_id.as_str()),
    );
    compare_projection_field(
        &mut mismatches,
        "active_manifest_version",
        &metadata.active_manifest_version,
        &Some(generation.manifest_version),
    );
    compare_projection_field(
        &mut mismatches,
        "active_core_record_version",
        &metadata.active_core_record_version,
        &Some(generation.core_record_version),
    );
    compare_projection_field(
        &mut mismatches,
        "active_core_record_contract_fingerprint",
        &metadata.active_core_record_contract_fingerprint.as_deref(),
        &Some(generation.core_record_contract_fingerprint.as_str()),
    );
    compare_projection_field(
        &mut mismatches,
        "active_lexical_schema_version",
        &metadata.active_lexical_schema_version,
        &Some(generation.lexical_schema_version),
    );
    compare_projection_field(
        &mut mismatches,
        "active_policy_schema_hash",
        &metadata.active_policy_schema_hash.as_deref(),
        &Some(generation.policy_schema_hash.as_str()),
    );
    compare_projection_field(
        &mut mismatches,
        "active_materializer_revision",
        &metadata.active_materializer_revision,
        &Some(RELATIONAL_MATERIALIZER_REVISION),
    );
    compare_projection_field(
        &mut mismatches,
        "target_core_generation_id",
        &metadata.target_core_generation_id,
        &None,
    );
    compare_projection_field(
        &mut mismatches,
        "build_generation",
        &metadata.build_generation,
        &receipt.build_generation,
    );
    compare_projection_field(
        &mut mismatches,
        "source_count",
        &metadata.source_count,
        &receipt.source_count,
    );
    compare_projection_field(
        &mut mismatches,
        "session_count",
        &metadata.session_count,
        &receipt.session_count,
    );
    compare_projection_field(
        &mut mismatches,
        "event_count",
        &metadata.event_count,
        &receipt.event_count,
    );
    compare_projection_field(
        &mut mismatches,
        "repository_binding_count",
        &metadata.repository_binding_count,
        &receipt.repository_binding_count,
    );
    compare_projection_field(
        &mut mismatches,
        "file_touch_count",
        &metadata.file_touch_count,
        &receipt.file_touch_count,
    );
    compare_projection_field(
        &mut mismatches,
        "vcs_observation_count",
        &metadata.vcs_observation_count,
        &receipt.vcs_observation_count,
    );

    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches.join("; "))
    }
}

fn compare_projection_field<T>(mismatches: &mut Vec<String>, field: &str, actual: &T, expected: &T)
where
    T: std::fmt::Debug + PartialEq,
{
    if actual != expected {
        mismatches.push(format!("{field} is {actual:?}, expected {expected:?}"));
    }
}

fn status_name(status: RelationalProjectionStatus) -> &'static str {
    match status {
        RelationalProjectionStatus::Empty => "empty",
        RelationalProjectionStatus::Ready => "ready",
        RelationalProjectionStatus::Behind => "behind",
    }
}

fn projection_metadata(data_root: &Path) -> Option<RelationalProjectionMetadata> {
    SourceBackedRelationalProjection::open_read_only(sql_compatibility_path(data_root))
        .ok()
        .and_then(|projection| projection.metadata().ok())
}

fn ready_projection_metadata(
    data_root: &Path,
    core_generation_id: &str,
) -> Option<RelationalProjectionMetadata> {
    projection_metadata(data_root).filter(|metadata| {
        metadata.status == RelationalProjectionStatus::Ready
            && metadata.active_core_generation_id.as_deref() == Some(core_generation_id)
            && metadata.active_materializer_revision == Some(RELATIONAL_MATERIALIZER_REVISION)
            && metadata.target_core_generation_id.is_none()
    })
}

fn next_attempt(
    prior: Option<&SourceBackedRelationalCatchUpStatus>,
    core_generation_id: &str,
) -> u64 {
    prior
        .filter(|status| status.core_generation_id == core_generation_id)
        .map(|status| status.attempts.saturating_add(1))
        .unwrap_or(1)
}

fn status_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(SOURCE_BACKED_RELATIONAL_STATUS_FILE)
}

fn read_status(data_root: &Path) -> Option<SourceBackedRelationalCatchUpStatus> {
    read_daemon_job_status(&status_path(data_root))
        .and_then(|value| serde_json::from_value(value).ok())
}

fn persist_status(data_root: &Path, status: &SourceBackedRelationalCatchUpStatus) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), &status.to_json()?)
}

#[cfg(test)]
#[path = "source_backed_relational_catch_up_tests.rs"]
mod tests;

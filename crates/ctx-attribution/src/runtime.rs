//! Direct, local attribution execution over retained Core and a committed index.
use std::path::{Path, PathBuf};
mod freshness;

use ctx_attribution_index::FlatStore;
use ctx_attribution_model::{
    BlameDiagnostic, BlameRequest, BlameTarget, CoreMaterializationReceiptIdentity,
    HostedBlameResult, QuerySnapshotExpectation,
};
use ctx_history_snapshot_reader::{CoreSnapshot, SnapshotContract, load_active_generation_id};

use crate::diagnostic::{from_adapter_error, from_snapshot_error};
use crate::errors::AdapterError;
use crate::graph::segment_graph::{SegmentGraph, SegmentGraphError};
use crate::ingest::CoreProjectionStatus;
use crate::materializer::{SegmentMaterializer, StatusRequest};
use crate::{CoreMaterializationSyncOutcome, GitExecutable};

fn index_root(data_root: &Path) -> PathBuf {
    data_root.join("search").join("attribution")
}

fn open_query_graph(data_root: &Path) -> Result<SegmentGraph, BlameDiagnostic> {
    let root = index_root(data_root);
    let pinned = FlatStore::new(&root)
        .open_active(SegmentGraph::flat_open_policy())
        .map_err(|error| from_adapter_error(&AdapterError::from(SegmentGraphError::from(error))))?;
    // Graph-only commit/PR queries can still work without a local Git executable.
    Ok(SegmentGraph::from_pinned(
        pinned,
        GitExecutable::discover().ok(),
    ))
}

/// Executes a bounded query. The runtime, not transport, selects its exact Core expectation.
pub fn query(
    data_root: &Path,
    target: &BlameTarget,
    limit: u32,
    cursor: Option<&str>,
) -> Result<HostedBlameResult, BlameDiagnostic> {
    let graph = open_query_graph(data_root)?;
    let expectation = CoreMaterializationReceiptIdentity::from_receipt(graph.completed_receipt())
        .map_err(|error| crate::diagnostic::from_protocol_error(&error))?;
    let request = BlameRequest {
        target: target.clone(),
        limit,
        cursor: cursor.map(str::to_owned),
        expected_snapshot: QuerySnapshotExpectation::Core {
            receipt: expectation,
        },
    };
    query_pinned(data_root, &graph, &request)
}

/// Lower-level request entry for callers already carrying a canonical expectation.
pub fn blame(
    data_root: &Path,
    request: &BlameRequest,
) -> Result<HostedBlameResult, BlameDiagnostic> {
    let graph = open_query_graph(data_root)?;
    query_pinned(data_root, &graph, request)
}

fn query_pinned(
    data_root: &Path,
    graph: &SegmentGraph,
    request: &BlameRequest,
) -> Result<HostedBlameResult, BlameDiagnostic> {
    if graph.completed_receipt().materializer_revision
        != crate::core_materialization::CORE_MATERIALIZER_REVISION
    {
        return Err(crate::diagnostic::projection_diagnostic(
            ctx_attribution_model::CoreProjectionCurrentness::NeedsRebuild,
        )
        .expect("incompatible projection has a diagnostic"));
    }
    request
        .validate()
        .map_err(|error| crate::diagnostic::from_protocol_error(&error))?;
    let contract = SnapshotContract::current().map_err(|error| from_snapshot_error(&error))?;
    let snapshot = CoreSnapshot::open(
        data_root,
        &graph.completed_receipt().core_generation_id,
        &contract,
    )
    .map_err(|error| from_snapshot_error(&error))?;
    // This reader stays alive through resolution, query, and result validation.
    // Exact generation reads never fall forward to a newer active Core snapshot.
    if snapshot.contract().core_record_fingerprint
        != graph.completed_receipt().core_record_contract_fingerprint
    {
        return Err(crate::diagnostic::from_protocol_error(
            &ctx_attribution_model::ProtocolError::new(
                ctx_attribution_model::ErrorClass::ProtocolMismatch,
                "Core receipt contract differs from retained snapshot",
            ),
        ));
    }
    let active_before =
        load_active_generation_id(data_root).map_err(|error| from_snapshot_error(&error))?;
    let result = graph.blame_request(request).map_err(|error| {
        crate::diagnostic::with_search_action(
            from_adapter_error(&AdapterError::from(error)),
            &request.target,
        )
    });
    let active_after =
        load_active_generation_id(data_root).map_err(|error| from_snapshot_error(&error))?;
    freshness::complete(
        result,
        snapshot.generation_id(),
        active_before.as_deref(),
        active_after.as_deref(),
    )
}

/// Read-only readiness against the already committed Core frontier.
pub fn readiness(data_root: &Path) -> Result<CoreProjectionStatus, BlameDiagnostic> {
    let active =
        load_active_generation_id(data_root).map_err(|error| from_snapshot_error(&error))?;
    let mut observed = status(data_root, active.as_deref())?;
    if active.is_none()
        && observed.receipt.is_some()
        && observed.currentness == ctx_attribution_model::CoreProjectionCurrentness::Current
    {
        observed.currentness = ctx_attribution_model::CoreProjectionCurrentness::Stale;
        observed.materialized_coverage = ctx_attribution_model::MaterializedCoverage::Partial;
        observed.diagnostic = crate::diagnostic::projection_diagnostic(observed.currentness);
        observed.availability = Default::default();
        observed.local_repository_access = false;
    }
    Ok(observed)
}

/// Observes projection metadata without creating, repairing, or materializing an index.
pub fn status(
    data_root: &Path,
    core_generation_id: Option<&str>,
) -> Result<CoreProjectionStatus, BlameDiagnostic> {
    let mut status = SegmentMaterializer::read_only_projection_status(
        index_root(data_root),
        &StatusRequest {
            requested_core_generation_id: core_generation_id.map(str::to_owned),
        },
    )
    .map_err(|error| from_adapter_error(&AdapterError::from(error)))?;
    if GitExecutable::discover().is_err() {
        status.local_repository_access = false;
        status.availability.file_blame = false;
    }
    Ok(status)
}

/// Completes attribution from the exact snapshot retained by the caller.
/// Used identically by daemon startup/publication and explicit manual completion.
pub fn catch_up(
    data_root: &Path,
    snapshot: &CoreSnapshot,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<CoreMaterializationSyncOutcome> {
    if cancelled() {
        return Err(crate::materializer::SegmentMaterializerError::Cancelled.into());
    }
    let mut materializer = SegmentMaterializer::open_cancellable(
        index_root(data_root),
        crate::core_materialization::CORE_MATERIALIZER_REVISION,
        cancelled,
    )?;
    crate::catch_up::sync_generation_pinned_core(
        data_root,
        snapshot,
        &mut materializer,
        Some(cancelled),
    )
}

/// Reads active materializer progress without waiting, creating files, or inspecting index bodies.
pub fn materialization_progress(
    data_root: &Path,
) -> anyhow::Result<Option<crate::materializer::MaterializationProgress>> {
    crate::materializer::locking::OperationLock::read_progress(&index_root(data_root))
        .map_err(Into::into)
}

/// Delivers advisory progress at cooperative checkpoints, including writer-lock waits.
/// Observer failures cancel unfinished work; an already committed index stays committed.
pub fn catch_up_with_progress(
    data_root: &Path,
    snapshot: &CoreSnapshot,
    cancelled: &(dyn Fn() -> bool + Sync),
    report: &(dyn Fn(&crate::materializer::MaterializationProgress) -> anyhow::Result<()> + Sync),
) -> anyhow::Result<CoreMaterializationSyncOutcome> {
    use crate::materializer::{MaterializationPhase, MaterializationProgress};
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    if cancelled() {
        return Err(crate::materializer::SegmentMaterializerError::Cancelled.into());
    }

    let waiting = MaterializationProgress {
        phase: MaterializationPhase::WaitingForWriter,
        core_generation_id: Some(snapshot.generation_id().to_owned()),
        ..Default::default()
    };
    let observer = Mutex::new((None::<Instant>, None::<anyhow::Error>));
    let owns_writer = AtomicBool::new(false);
    let checkpoint = || {
        if cancelled() {
            return true;
        }
        let mut observer = observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if observer.1.is_some() {
            return true;
        }
        if observer
            .0
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(500))
        {
            observer.0 = Some(Instant::now());
            let progress = if owns_writer.load(Ordering::Relaxed) {
                materialization_progress(data_root)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
            } else {
                waiting.clone()
            };
            if let Err(error) = report(&progress) {
                observer.1 = Some(error);
                return true;
            }
        }
        false
    };
    let result = (|| {
        let mut materializer = SegmentMaterializer::open_cancellable(
            index_root(data_root),
            crate::core_materialization::CORE_MATERIALIZER_REVISION,
            &checkpoint,
        )?;
        owns_writer.store(true, Ordering::Relaxed);
        report(&materializer.progress())?;
        let outcome = crate::catch_up::sync_generation_pinned_core(
            data_root,
            snapshot,
            &mut materializer,
            Some(&checkpoint),
        )?;
        let mut progress = materializer.progress();
        progress.phase = MaterializationPhase::Complete;
        progress.core_generation_id = Some(snapshot.generation_id().to_owned());
        report(&progress)?;
        Ok(outcome)
    })();
    if let Some(error) = observer
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .1
    {
        return Err(error);
    }
    result
}

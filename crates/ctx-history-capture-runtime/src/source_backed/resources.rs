use std::{
    collections::BTreeSet,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc,
    },
};

use crate::{
    CoreRouteByteLease, CoreRouteResourceError, CoreRouteResourceKind, CoreRouteResources,
};
use ctx_history_capture_model::{CoreRecordBatchProgress, SharedAttemptHistoryProgress};

use super::{SourceBackedCurrentSourceProgressStage, SourceBackedReconciliationDemand};

const INTERMEDIATE_ACTIVITY_PARSING: u8 = 1;
const INTERMEDIATE_ACTIVITY_INDEX_WRITING: u8 = 2;

#[derive(Debug, Default)]
struct SourceBackedIntermediateActivity {
    generation: AtomicU64,
    stage: AtomicU8,
}

/// One best-effort route-local activity observation. It is deliberately not a
/// record, byte, session, message, or tool-call accounting surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceBackedIntermediateActivitySnapshot {
    pub(super) generation: u64,
    pub(super) stage: SourceBackedCurrentSourceProgressStage,
}

pub type SourceBackedRouteResourceKind = CoreRouteResourceKind;
pub type SourceBackedRouteByteReservation = CoreRouteByteLease;

/// Source-backed route context layered over the provider-neutral byte budget.
///
/// Hermes chooses incremental versus exhaustive reconciliation at the capture
/// boundary. The neutral runtime continues to own the exact shared byte and
/// worker budgets without importing that provider policy.
#[derive(Debug, Clone)]
pub struct SourceBackedRouteResources {
    core: CoreRouteResources,
    reconciliation_demand: SourceBackedReconciliationDemand,
    member_workset: Option<Arc<BTreeSet<PathBuf>>>,
    intermediate_activity: Arc<SourceBackedIntermediateActivity>,
    scan_cancellation: Option<Arc<AtomicBool>>,
    attempt_history_progress: SharedAttemptHistoryProgress,
}

impl SourceBackedRouteResources {
    pub fn production(leaf_worker_budget: usize) -> Self {
        Self::production_with_live_output_limit(
            leaf_worker_budget,
            crate::CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES,
        )
    }

    pub fn production_with_live_output_limit(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
    ) -> Self {
        Self {
            core: CoreRouteResources::production_with_live_output_limit(
                leaf_worker_budget,
                maximum_live_output_bytes,
            ),
            reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
            member_workset: None,
            intermediate_activity: Arc::new(SourceBackedIntermediateActivity::default()),
            scan_cancellation: None,
            attempt_history_progress: SharedAttemptHistoryProgress::default(),
        }
    }

    #[doc(hidden)]
    pub fn for_test(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self {
            core: CoreRouteResources::for_test(
                leaf_worker_budget,
                maximum_live_output_bytes,
                maximum_physical_scratch_bytes,
            ),
            reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
            member_workset: None,
            intermediate_activity: Arc::new(SourceBackedIntermediateActivity::default()),
            scan_cancellation: None,
            attempt_history_progress: SharedAttemptHistoryProgress::default(),
        }
    }

    pub(super) fn with_scan_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.scan_cancellation = Some(cancellation);
        self
    }

    #[doc(hidden)]
    pub fn with_attempt_history_progress(mut self, progress: SharedAttemptHistoryProgress) -> Self {
        self.attempt_history_progress = progress;
        self
    }

    /// Publishes scanner-owned facts without touching the coordinator callback,
    /// journal, or engine state lock.
    #[doc(hidden)]
    pub fn publish_parallel_page_progress(
        &self,
        completed_bytes: u64,
        progress: &CoreRecordBatchProgress,
    ) {
        self.attempt_history_progress
            .publish_parallel_page(completed_bytes, progress);
    }

    /// Reports cancellation of the parallel scan that owns these resources.
    /// Serial route resources have no bound cancellation signal.
    #[doc(hidden)]
    pub fn scan_cancelled(&self) -> bool {
        self.scan_cancellation
            .as_ref()
            .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
    }

    pub fn with_reconciliation_demand(mut self, demand: SourceBackedReconciliationDemand) -> Self {
        self.reconciliation_demand = demand;
        self
    }

    pub const fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.reconciliation_demand
    }

    pub fn with_member_workset(mut self, members: Option<BTreeSet<PathBuf>>) -> Self {
        self.member_workset = members.map(Arc::new);
        self
    }

    pub fn member_workset(&self) -> Option<&BTreeSet<PathBuf>> {
        self.member_workset.as_deref()
    }

    pub fn member_selected(&self, path: &Path) -> bool {
        self.member_workset()
            .is_some_and(|members| members.contains(path))
    }

    pub fn leaf_worker_budget(&self) -> usize {
        self.core.leaf_worker_budget()
    }

    pub fn maximum_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        self.core.maximum_bytes(kind)
    }

    pub fn core_output_batch_reservation_bytes(&self) -> u64 {
        self.core.core_output_batch_reservation_bytes()
    }

    /// Records only transient route activity. Scanner hot paths may call this
    /// without crossing into progress callbacks or durable accounting.
    #[doc(hidden)]
    pub fn record_intermediate_activity(&self, stage: SourceBackedCurrentSourceProgressStage) {
        let stage = match stage {
            SourceBackedCurrentSourceProgressStage::Parsing => INTERMEDIATE_ACTIVITY_PARSING,
            SourceBackedCurrentSourceProgressStage::IndexWriting => {
                INTERMEDIATE_ACTIVITY_INDEX_WRITING
            }
            SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            | SourceBackedCurrentSourceProgressStage::OnlineBackup
            | SourceBackedCurrentSourceProgressStage::LogicalFingerprint
            | SourceBackedCurrentSourceProgressStage::LogicalScan => return,
        };
        self.intermediate_activity
            .stage
            .store(stage, Ordering::Relaxed);
        self.intermediate_activity
            .generation
            .fetch_add(1, Ordering::Release);
    }

    #[doc(hidden)]
    pub(super) fn intermediate_activity_generation(&self) -> u64 {
        self.intermediate_activity
            .generation
            .load(Ordering::Acquire)
    }

    #[doc(hidden)]
    pub(super) fn intermediate_activity_after(
        &self,
        observed_generation: u64,
    ) -> Option<SourceBackedIntermediateActivitySnapshot> {
        let generation = self.intermediate_activity_generation();
        if generation == observed_generation {
            return None;
        }
        let stage = match self.intermediate_activity.stage.load(Ordering::Relaxed) {
            INTERMEDIATE_ACTIVITY_PARSING => SourceBackedCurrentSourceProgressStage::Parsing,
            INTERMEDIATE_ACTIVITY_INDEX_WRITING => {
                SourceBackedCurrentSourceProgressStage::IndexWriting
            }
            _ => return None,
        };
        Some(SourceBackedIntermediateActivitySnapshot { generation, stage })
    }

    pub fn reserve(
        &self,
        kind: CoreRouteResourceKind,
        bytes: usize,
    ) -> Result<CoreRouteByteLease, CoreRouteResourceError> {
        self.core.reserve(kind, bytes)
    }

    #[doc(hidden)]
    pub fn live_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        self.core.live_bytes(kind)
    }
}

impl Deref for SourceBackedRouteResources {
    type Target = CoreRouteResources;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use ctx_history_core::CoreRecord;
use ctx_history_index::{
    CoreRecordPreparer, IndexError, PreparedCoreRecord, PreparedCoreRecordDraft,
    PreparedCoreRecordMaterialization,
};

use super::{SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult};

/// Prepared Core records may retain at most this many exact encoded bytes
/// while crossing the shared worker-to-writer route envelope. Reservations
/// are live, not cumulative: large routes continue streaming after the writer
/// consumes each bounded emission.
const SOURCE_BACKED_ROUTE_MAX_LIVE_CORE_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// A protocol emission carries at most one ordinary JSONL/Codex page worth of
/// Core records. JSONL input pages are already capped at 64 physical records;
/// fan-out projectors are split into additional batches at this same bound.
/// Exact prepared bytes remain governed independently by the shared 256 MiB
/// live Core-output budget above.
pub(crate) const SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS: usize = 64;

/// Replacement-document workers may each stage one provider-bounded logical
/// snapshot. The common document family uses at most four such workers and a
/// 256 MiB per-snapshot bound, so this is an explicit aggregate physical-file
/// ceiling rather than N copies of an implicit route allowance.
const SOURCE_BACKED_ROUTE_MAX_PHYSICAL_SCRATCH_BYTES: u64 = 4 * 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceBackedRouteResourceKind {
    CoreOutput,
    LogicalSourceScratch,
}

impl std::fmt::Display for SourceBackedRouteResourceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CoreOutput => "live prepared Core-record output",
            Self::LogicalSourceScratch => "physical logical-source scratch",
        })
    }
}

#[derive(Debug)]
struct SourceBackedRouteByteBudget {
    maximum: u64,
    live: AtomicU64,
    #[cfg(test)]
    successful_reservations: AtomicU64,
}

/// Cloneable route resources shared by every scanner worker. Cloning this
/// value never creates another output or physical-scratch allowance.
#[derive(Debug, Clone)]
pub(crate) struct SourceBackedRouteResources {
    leaf_worker_budget: usize,
    output: Arc<SourceBackedRouteByteBudget>,
    scratch: Arc<SourceBackedRouteByteBudget>,
}

impl SourceBackedRouteResources {
    pub(crate) fn production(leaf_worker_budget: usize) -> Self {
        Self::with_byte_limits(
            leaf_worker_budget,
            SOURCE_BACKED_ROUTE_MAX_LIVE_CORE_OUTPUT_BYTES,
            SOURCE_BACKED_ROUTE_MAX_PHYSICAL_SCRATCH_BYTES,
        )
    }

    fn with_byte_limits(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self {
            leaf_worker_budget: leaf_worker_budget.max(1),
            output: Arc::new(SourceBackedRouteByteBudget {
                maximum: maximum_live_output_bytes,
                live: AtomicU64::new(0),
                #[cfg(test)]
                successful_reservations: AtomicU64::new(0),
            }),
            scratch: Arc::new(SourceBackedRouteByteBudget {
                maximum: maximum_physical_scratch_bytes,
                live: AtomicU64::new(0),
                #[cfg(test)]
                successful_reservations: AtomicU64::new(0),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self::with_byte_limits(
            leaf_worker_budget,
            maximum_live_output_bytes,
            maximum_physical_scratch_bytes,
        )
    }

    pub(crate) fn leaf_worker_budget(&self) -> usize {
        self.leaf_worker_budget
    }

    pub(crate) fn maximum_bytes(&self, kind: SourceBackedRouteResourceKind) -> u64 {
        match kind {
            SourceBackedRouteResourceKind::CoreOutput => self.output.maximum,
            SourceBackedRouteResourceKind::LogicalSourceScratch => self.scratch.maximum,
        }
    }

    /// One worker may reserve this conservative share before preparing a
    /// transport batch. Shares across the configured worker budget sum to no
    /// more than the route maximum, so batching amortizes global accounting
    /// without weakening the aggregate live-byte ceiling.
    pub(crate) fn core_output_batch_reservation_bytes(&self) -> u64 {
        if self.output.maximum == 0 {
            return 0;
        }
        let workers = u64::try_from(self.leaf_worker_budget).unwrap_or(u64::MAX);
        self.output.maximum.checked_div(workers).unwrap_or(0).max(1)
    }

    pub(crate) fn reserve(
        &self,
        kind: SourceBackedRouteResourceKind,
        bytes: usize,
    ) -> SourceBackedRouteResult<SourceBackedRouteByteReservation> {
        let budget = match kind {
            SourceBackedRouteResourceKind::CoreOutput => &self.output,
            SourceBackedRouteResourceKind::LogicalSourceScratch => &self.scratch,
        };
        let bytes = u64::try_from(bytes).map_err(|_| resource_error(kind, budget.maximum))?;
        let mut live = budget.live.load(Ordering::Acquire);
        loop {
            let Some(next) = live.checked_add(bytes) else {
                return Err(resource_error(kind, budget.maximum));
            };
            if next > budget.maximum {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::ResourceUnavailable,
                    format!(
                        "shared route {kind} byte limit exceeded: maximum {}, observed {next}",
                        budget.maximum
                    ),
                ));
            }
            match budget
                .live
                .compare_exchange_weak(live, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    #[cfg(test)]
                    budget
                        .successful_reservations
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(SourceBackedRouteByteReservation {
                        budget: Arc::clone(budget),
                        bytes,
                    });
                }
                Err(actual) => live = actual,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn live_bytes(&self, kind: SourceBackedRouteResourceKind) -> u64 {
        match kind {
            SourceBackedRouteResourceKind::CoreOutput => &self.output,
            SourceBackedRouteResourceKind::LogicalSourceScratch => &self.scratch,
        }
        .live
        .load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn successful_reservations(&self, kind: SourceBackedRouteResourceKind) -> u64 {
        match kind {
            SourceBackedRouteResourceKind::CoreOutput => &self.output,
            SourceBackedRouteResourceKind::LogicalSourceScratch => &self.scratch,
        }
        .successful_reservations
        .load(Ordering::Relaxed)
    }
}

fn resource_error(kind: SourceBackedRouteResourceKind, maximum: u64) -> SourceBackedRouteError {
    SourceBackedRouteError::new(
        SourceBackedRouteErrorKind::ResourceUnavailable,
        format!("shared route {kind} byte accounting overflowed (maximum {maximum})"),
    )
}

#[derive(Debug)]
pub(crate) struct SourceBackedRouteByteReservation {
    budget: Arc<SourceBackedRouteByteBudget>,
    bytes: u64,
}

impl SourceBackedRouteByteReservation {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for SourceBackedRouteByteReservation {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.budget.live.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

/// The single provider-family emission envelope. Its reservation is measured
/// in the index-owned final encoded Core byte domain and remains live until
/// the prepared record has been accepted by the generation writer.
pub(crate) struct CoreRecordEmission {
    prepared: PreparedCoreRecord,
    reservation: SourceBackedRouteByteReservation,
}

impl std::fmt::Debug for CoreRecordEmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreRecordEmission")
            .field("source", self.prepared.source())
            .field("encoded_bytes", &self.prepared.encoded_core_bytes())
            .finish()
    }
}

impl CoreRecordEmission {
    pub(crate) fn new(
        record: CoreRecord,
        resources: &SourceBackedRouteResources,
        preparer: &CoreRecordPreparer,
    ) -> SourceBackedRouteResult<Self> {
        let prepared = Self::prepare(record, preparer)?;
        Self::from_prepared(prepared, resources)
    }

    pub(crate) fn prepare(
        record: CoreRecord,
        preparer: &CoreRecordPreparer,
    ) -> SourceBackedRouteResult<PreparedCoreRecord> {
        preparer.prepare(record).map_err(|error| {
            SourceBackedRouteError::new(core_preparation_error_kind(&error), error.to_string())
        })
    }

    pub(crate) fn prepare_draft(
        record: CoreRecord,
        preparer: &CoreRecordPreparer,
    ) -> SourceBackedRouteResult<PreparedCoreRecordDraft> {
        preparer.prepare_draft(record).map_err(|error| {
            SourceBackedRouteError::new(core_preparation_error_kind(&error), error.to_string())
        })
    }

    pub(crate) fn materialize_draft(
        draft: PreparedCoreRecordDraft,
        maximum_encoded_bytes: usize,
    ) -> SourceBackedRouteResult<PreparedCoreRecordMaterialization> {
        draft.materialize(maximum_encoded_bytes).map_err(|error| {
            SourceBackedRouteError::new(core_preparation_error_kind(&error), error.to_string())
        })
    }

    pub(crate) fn from_prepared(
        prepared: PreparedCoreRecord,
        resources: &SourceBackedRouteResources,
    ) -> SourceBackedRouteResult<Self> {
        let reservation = resources.reserve(
            SourceBackedRouteResourceKind::CoreOutput,
            prepared.encoded_core_bytes(),
        )?;
        Ok(Self::from_prepared_and_reservation(prepared, reservation))
    }

    pub(crate) fn from_prepared_and_reservation(
        prepared: PreparedCoreRecord,
        reservation: SourceBackedRouteByteReservation,
    ) -> Self {
        Self {
            prepared,
            reservation,
        }
    }

    pub(crate) fn into_prepared(self) -> (PreparedCoreRecord, SourceBackedRouteByteReservation) {
        let Self {
            prepared,
            reservation,
        } = self;
        (prepared, reservation)
    }
}

/// A mutable worker-local Core-record batch. The batch acquires one
/// conservative share of the global output budget before retaining prepared
/// records. A record larger than that share instead receives one exact
/// reservation and travels alone.
#[derive(Debug, Default)]
pub(crate) struct CoreRecordEmissionBatchBuilder {
    prepared: Vec<PreparedCoreRecord>,
    reservation: Option<SourceBackedRouteByteReservation>,
    prepared_bytes: u64,
}

impl CoreRecordEmissionBatchBuilder {
    pub(crate) fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.prepared.len()
    }

    pub(crate) fn has_reservation(&self) -> bool {
        self.reservation.is_some()
    }

    pub(crate) fn reservation_bytes(&self) -> u64 {
        self.reservation
            .as_ref()
            .map_or(0, SourceBackedRouteByteReservation::bytes)
    }

    pub(crate) fn remaining_bytes(&self) -> u64 {
        self.reservation_bytes().saturating_sub(self.prepared_bytes)
    }

    pub(crate) fn can_admit(&self, prepared_bytes: u64) -> bool {
        self.reservation.as_ref().is_some_and(|reservation| {
            self.prepared_bytes
                .checked_add(prepared_bytes)
                .is_some_and(|total| total <= reservation.bytes())
        })
    }

    pub(crate) fn reserve_bytes(
        &mut self,
        reservation_bytes: u64,
        resources: &SourceBackedRouteResources,
    ) -> SourceBackedRouteResult<()> {
        debug_assert!(self.is_empty());
        debug_assert!(self.reservation.is_none());
        let reservation_bytes = usize::try_from(reservation_bytes).map_err(|_| {
            resource_error(
                SourceBackedRouteResourceKind::CoreOutput,
                resources.maximum_bytes(SourceBackedRouteResourceKind::CoreOutput),
            )
        })?;
        self.reservation =
            Some(resources.reserve(SourceBackedRouteResourceKind::CoreOutput, reservation_bytes)?);
        Ok(())
    }

    pub(crate) fn release_empty_reservation(&mut self) {
        debug_assert!(self.is_empty());
        self.reservation = None;
        self.prepared_bytes = 0;
    }

    pub(crate) fn push(&mut self, prepared: PreparedCoreRecord) -> SourceBackedRouteResult<()> {
        let prepared_bytes = u64::try_from(prepared.encoded_core_bytes()).map_err(|_| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "prepared Core-record byte count overflowed",
            )
        })?;
        if !self.can_admit(prepared_bytes) {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "prepared Core record exceeded its batch reservation",
            ));
        }
        self.prepared_bytes = self
            .prepared_bytes
            .checked_add(prepared_bytes)
            .ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "prepared Core-record batch byte count overflowed",
                )
            })?;
        self.prepared.push(prepared);
        Ok(())
    }

    pub(crate) fn take_batch(
        &mut self,
    ) -> SourceBackedRouteResult<Option<CoreRecordEmissionBatch>> {
        if self.is_empty() {
            self.reservation = None;
            self.prepared_bytes = 0;
            return Ok(None);
        }
        if self.prepared.len() > SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                format!(
                    "Core-record emission batch exceeds the shared {}-record protocol bound",
                    SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS
                ),
            ));
        }
        let reservation = self.reservation.take().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "prepared Core-record batch has no live byte reservation",
            )
        })?;
        self.prepared_bytes = 0;
        Ok(Some(CoreRecordEmissionBatch {
            prepared: std::mem::take(&mut self.prepared),
            reservation,
        }))
    }
}

/// One worker-prepared Core-record batch. All retained records share one
/// aggregate reservation, released if the message is rejected or after the
/// writer accepts the complete batch.
#[derive(Debug)]
pub(crate) struct CoreRecordEmissionBatch {
    prepared: Vec<PreparedCoreRecord>,
    reservation: SourceBackedRouteByteReservation,
}

impl CoreRecordEmissionBatch {
    pub(crate) fn len(&self) -> usize {
        self.prepared.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &PreparedCoreRecord> {
        self.prepared.iter()
    }

    pub(crate) fn into_prepared(
        self,
    ) -> (Vec<PreparedCoreRecord>, SourceBackedRouteByteReservation) {
        (self.prepared, self.reservation)
    }
}

fn core_preparation_error_kind(error: &IndexError) -> SourceBackedRouteErrorKind {
    if matches!(
        error,
        IndexError::ProjectionContract(_)
            | IndexError::CoreRecord(_)
            | IndexError::CoreRecordPolicyRevisionMismatch { .. }
            | IndexError::EmptyDocumentField { .. }
            | IndexError::DocumentFieldTooLarge { .. }
    ) {
        SourceBackedRouteErrorKind::InvalidSource
    } else {
        SourceBackedRouteErrorKind::Internal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_preparation_failure_classification_preserves_systemic_boundaries() {
        assert_eq!(
            core_preparation_error_kind(&IndexError::ConcurrentGenerationChange),
            SourceBackedRouteErrorKind::Internal
        );
        assert_eq!(
            core_preparation_error_kind(&IndexError::ActiveGenerationNeedsRebuild {
                generation_id: "0".repeat(64),
                detail: "damaged base".to_owned(),
            }),
            SourceBackedRouteErrorKind::Internal
        );
        assert_eq!(
            core_preparation_error_kind(&IndexError::CoreRecordPolicyRevisionMismatch {
                normalization: 0,
                expected_normalization: 1,
                content: 0,
                expected_content: 1,
            }),
            SourceBackedRouteErrorKind::InvalidSource
        );
    }

    #[test]
    fn route_output_budget_is_live_without_a_cumulative_cap() {
        let resources = SourceBackedRouteResources::for_test(2, 9, 20);
        let first = resources
            .reserve(SourceBackedRouteResourceKind::CoreOutput, 5)
            .unwrap();
        assert_eq!(
            resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
            5
        );
        drop(first);
        let second = resources
            .reserve(SourceBackedRouteResourceKind::CoreOutput, 5)
            .unwrap();
        drop(second);

        assert_eq!(
            resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
            0
        );
    }

    #[test]
    fn cloned_workers_share_one_live_output_budget_exactly_one_over() {
        let resources = SourceBackedRouteResources::for_test(4, 9, 20);
        let first = resources
            .reserve(SourceBackedRouteResourceKind::CoreOutput, 5)
            .unwrap();
        let error = resources
            .clone()
            .reserve(SourceBackedRouteResourceKind::CoreOutput, 5)
            .unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
        assert!(error.detail.contains("maximum 9, observed 10"));
        drop(first);
        resources
            .reserve(SourceBackedRouteResourceKind::CoreOutput, 5)
            .unwrap();
    }

    #[test]
    fn physical_scratch_has_a_separate_exact_aggregate_limit() {
        let resources = SourceBackedRouteResources::for_test(4, 3, 9);
        let first = resources
            .reserve(SourceBackedRouteResourceKind::LogicalSourceScratch, 5)
            .unwrap();
        let error = resources
            .clone()
            .reserve(SourceBackedRouteResourceKind::LogicalSourceScratch, 5)
            .unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
        assert!(error.detail.contains("maximum 9, observed 10"));
        assert_eq!(
            resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
            0
        );
        drop(first);
        assert_eq!(
            resources.live_bytes(SourceBackedRouteResourceKind::LogicalSourceScratch),
            0
        );
    }
}

use anyhow::{anyhow, Result};
use ctx_history_index_query::{
    CompiledSearchFilter, DiagnosedLexicalSearchBatchResult, EventCandidateQueryReceipt,
    LexicalSearchBatch, VerifiedIndex,
};

use super::{
    collect_search_hits_with_receipt, HistorySemanticPort, RankedSearchCollection,
    SearchExecutionError, SearchExecutionResult, SearchRequest, SemanticAvailability,
};

/// Exact low-level work used to diagnose retrieval amplification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchWorkReceipt {
    pub retrieval_rounds: Option<u64>,
    pub query_executions: Option<u64>,
    pub candidate_rows: Option<u64>,
    pub records_decoded: Option<u64>,
    pub encoded_core_bytes_decoded: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStopReason {
    Decisive,
    Exhausted,
    CandidateCap,
    FixedPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFailurePhase {
    GenerationOpen,
    QueryPreparation,
    SemanticRetrieval,
    IndexQueryDecode,
    ResultProjection,
}

pub(super) struct SearchWorkTracker {
    pub(super) work: SearchWorkReceipt,
    failure_phase: SearchFailurePhase,
}

impl SearchWorkTracker {
    pub(super) fn new() -> Self {
        Self {
            work: SearchWorkReceipt::default(),
            failure_phase: SearchFailurePhase::QueryPreparation,
        }
    }

    pub(super) fn set_phase(&mut self, phase: SearchFailurePhase) {
        self.failure_phase = phase;
    }

    pub(super) fn record_retrieval_round(&mut self) -> Result<()> {
        checked_add(&mut self.work.retrieval_rounds, 1)
    }

    fn record_candidate_batch(&mut self, batch: EventCandidateQueryReceipt) -> Result<()> {
        checked_add(&mut self.work.query_executions, batch.query_executions)?;
        checked_add(&mut self.work.candidate_rows, batch.collector_hits)?;
        checked_add(&mut self.work.records_decoded, batch.records_decoded)?;
        checked_add(
            &mut self.work.encoded_core_bytes_decoded,
            batch.encoded_core_bytes_decoded,
        )
    }
}

fn checked_add(total: &mut Option<u64>, value: u64) -> Result<()> {
    *total = Some(
        total
            .unwrap_or(0)
            .checked_add(value)
            .ok_or_else(|| anyhow!("search work count overflow"))?,
    );
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ObservedSearchExecutionError {
    pub(crate) error: Box<SearchExecutionError>,
    pub(crate) work: SearchWorkReceipt,
    pub(crate) failure_phase: SearchFailurePhase,
}

impl ObservedSearchExecutionError {
    pub(crate) fn new(
        error: SearchExecutionError,
        work: SearchWorkReceipt,
        failure_phase: SearchFailurePhase,
    ) -> Self {
        Self {
            error: Box::new(error),
            work,
            failure_phase,
        }
    }
}

pub(crate) fn collect_search_hits_observed<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
    semantic: SemanticAvailability,
    semantic_port: &P,
) -> std::result::Result<RankedSearchCollection, ObservedSearchExecutionError> {
    let mut tracker = SearchWorkTracker::new();
    let collection = collect_search_hits_with_receipt(
        request,
        index,
        filter,
        semantic,
        semantic_port,
        &mut tracker,
    )
    .map_err(|error| {
        ObservedSearchExecutionError::new(error, tracker.work, tracker.failure_phase)
    })?;
    Ok(collection)
}

pub(super) fn record_lexical_batch(
    tracker: &mut SearchWorkTracker,
    result: DiagnosedLexicalSearchBatchResult,
) -> SearchExecutionResult<LexicalSearchBatch> {
    tracker.record_retrieval_round()?;
    match result {
        Ok(observed) => {
            tracker.record_candidate_batch(observed.receipt)?;
            Ok(observed.batch)
        }
        Err(failure) => {
            tracker.record_candidate_batch(failure.receipt)?;
            Err(failure.error.into())
        }
    }
}

pub(super) fn lexical_terminal_state(batch: &LexicalSearchBatch) -> Option<SearchStopReason> {
    if !batch.complete {
        // A work ceiling proves truncation but not why the result set ended.
        // Do not mislabel arbitrary work exhaustion as the candidate cap.
        None
    } else if batch.candidate_set_exhaustive {
        Some(SearchStopReason::Exhausted)
    } else {
        Some(SearchStopReason::CandidateCap)
    }
}

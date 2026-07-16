use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{utc_now, CaptureProvider, HistoryRecord};
use ctx_history_store::{
    CatalogImportWork, CatalogIndexedStatus, CatalogSourceIndexUpdate,
    EventSearchBulkMaintenanceOutcome, ImportPendingReason, ProviderFileCheckpoint,
    ProviderFileImportOutcome, ProviderFileInventoryObservation, SourceImportFileIndexUpdate,
    SourceImportFileWork, Store, StoreError,
};
use uuid::Uuid;

use crate::provider::adapter::PiSessionJsonlAdapter;
use crate::provider::codex::events::{
    codex_session_header, codex_session_line_capture, codex_session_line_timestamp,
    CodexSessionLineContext, CodexToolCallContexts,
};
use crate::provider::codex::session::should_parse_codex_session_line;
use crate::{
    provider_jsonl_checkpoint_matches_file, CaptureError, CodexSessionJsonlAdapter,
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderCaptureAdapter,
    ProviderImportSummary, ProviderJsonlAppendCheckpoint, ProviderJsonlReader,
    ProviderJsonlRecordRead, ProviderJsonlReplacementReason, ProviderJsonlResumeState,
    ProviderNormalizationResult, Result, CODEX_SESSION_SOURCE_FORMAT,
};

use super::import_normalized_provider_captures;

/// Maximum transcript paths admitted to one atomic FreshNew group.
pub const FRESH_NEW_BATCH_MAX_PATHS: usize = 1_024;
pub(crate) const FRESH_NEW_BATCH_MAX_ACTUAL_UNITS: u64 = 4_096;
/// Exclusive serialized payload byte ceiling for one FreshNew group.
pub const FRESH_NEW_BATCH_MAX_BYTES: u64 = 8 * 1_024 * 1_024;
/// Canonical Pi transcript source format supported by FreshNew batching.
pub const PI_SESSION_SOURCE_FORMAT: &str = "pi_session_jsonl";
const FRESH_NEW_WAL_CHECKPOINT_BYTES: u64 = 64 * 1_024 * 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshNewObservation {
    pub(crate) file_size_bytes: u64,
    pub(crate) file_modified_at_ms: i64,
    pub(crate) token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshNewCandidateEvidence {
    pub(crate) machine_id: String,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) inventory_generation: u64,
    pub(crate) observation: FreshNewObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshNewCandidateKind {
    CodexCatalog,
    PiOrdinaryFile,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FreshNewCandidateWork {
    Codex(CatalogImportWork),
    Pi(SourceImportFileWork),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FreshNewBatchCandidate {
    pub(crate) kind: FreshNewCandidateKind,
    pub(crate) work: FreshNewCandidateWork,
    pub(crate) evidence: FreshNewCandidateEvidence,
}

impl FreshNewBatchCandidate {
    pub(crate) fn provider(&self) -> CaptureProvider {
        match &self.work {
            FreshNewCandidateWork::Codex(work) => work.session.provider,
            FreshNewCandidateWork::Pi(work) => work.file.provider,
        }
    }

    pub(crate) fn source_format(&self) -> &str {
        match &self.work {
            FreshNewCandidateWork::Codex(work) => &work.session.source_format,
            FreshNewCandidateWork::Pi(work) => &work.file.source_format,
        }
    }

    pub(crate) fn source_root(&self) -> &str {
        match &self.work {
            FreshNewCandidateWork::Codex(work) => &work.session.source_root,
            FreshNewCandidateWork::Pi(work) => &work.file.source_root,
        }
    }

    pub(crate) fn source_path(&self) -> &str {
        match &self.work {
            FreshNewCandidateWork::Codex(work) => &work.session.source_path,
            FreshNewCandidateWork::Pi(work) => &work.file.source_path,
        }
    }

    pub(crate) fn estimated_bytes(&self) -> u64 {
        match &self.work {
            FreshNewCandidateWork::Codex(work) => work.estimated_bytes,
            FreshNewCandidateWork::Pi(work) => work.estimated_bytes,
        }
    }

    fn import_revision(&self) -> u32 {
        match &self.work {
            FreshNewCandidateWork::Codex(work) => work.session.import_revision,
            FreshNewCandidateWork::Pi(work) => work.file.import_revision,
        }
    }

    fn import_outcome<'a>(
        &'a self,
        status: CatalogIndexedStatus,
        error: Option<&'a str>,
        event_count: Option<u64>,
        indexed_at_ms: i64,
    ) -> ProviderFileImportOutcome<'a> {
        let observation = match &self.work {
            FreshNewCandidateWork::Codex(work) => {
                ProviderFileInventoryObservation::ObservedCatalog {
                    source_format: &work.session.source_format,
                    update: CatalogSourceIndexUpdate {
                        source_root: &work.session.source_root,
                        source_path: &work.session.source_path,
                        file_size_bytes: work.session.file_size_bytes,
                        file_modified_at_ms: work.session.file_modified_at_ms,
                        import_revision: work.session.import_revision,
                        inventory_generation: self.evidence.inventory_generation,
                        file_sha256: None,
                        event_count,
                        indexed_at_ms,
                    },
                    metadata: &work.session.metadata,
                }
            }
            FreshNewCandidateWork::Pi(work) => ProviderFileInventoryObservation::SourceImport {
                source_format: &work.file.source_format,
                update: SourceImportFileIndexUpdate {
                    source_root: &work.file.source_root,
                    source_path: &work.file.source_path,
                    file_size_bytes: work.file.file_size_bytes,
                    file_modified_at_ms: work.file.file_modified_at_ms,
                    import_revision: work.file.import_revision,
                    inventory_generation: self.evidence.inventory_generation,
                    metadata: &work.file.metadata,
                    indexed_at_ms,
                },
            },
        };
        ProviderFileImportOutcome {
            provider: self.provider(),
            observation,
            status,
            error,
        }
    }

    fn same_group_scope(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.provider() == other.provider()
            && self.source_format() == other.source_format()
            && self.source_root() == other.source_root()
            && self.evidence.machine_id == other.evidence.machine_id
            && self.evidence.history_record_id == other.evidence.history_record_id
            && self.evidence.inventory_generation == other.evidence.inventory_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FreshNewDurableOnlyReason {
    NotFreshNew(ImportPendingReason),
    PriorOrActivePublication,
    UnsupportedConcreteSource,
    MissingCurrentGeneration,
    MissingCurrentObservation,
    MissingVisibleIdentity,
    ObservationChanged,
    DuplicateSourcePath,
    EstimatedPathOverLimit,
    ActualBatchOverLimit,
    TransientPreparation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshNewDurableOnly {
    pub(crate) source_path: String,
    pub(crate) reason: FreshNewDurableOnlyReason,
}

pub(crate) type FreshNewCandidateResult =
    std::result::Result<FreshNewBatchCandidate, FreshNewDurableOnly>;

pub(crate) fn construct_codex_fresh_new_candidate(
    work: CatalogImportWork,
    evidence: FreshNewCandidateEvidence,
) -> FreshNewCandidateResult {
    let source_path = work.session.source_path.clone();
    if work
        .session
        .external_session_id
        .as_deref()
        .is_none_or(|identity| identity.trim().is_empty())
    {
        return Err(FreshNewDurableOnly {
            source_path,
            reason: FreshNewDurableOnlyReason::MissingVisibleIdentity,
        });
    }
    validate_candidate_common(
        work.reason,
        work.has_active_publication,
        work.session.provider == CaptureProvider::Codex
            && work.session.source_format == CODEX_SESSION_SOURCE_FORMAT,
        work.session.file_size_bytes,
        work.session.file_modified_at_ms,
        work.session
            .metadata
            .get("file_observation_token_v1")
            .and_then(serde_json::Value::as_str),
        &source_path,
        &evidence,
    )?;
    Ok(FreshNewBatchCandidate {
        kind: FreshNewCandidateKind::CodexCatalog,
        work: FreshNewCandidateWork::Codex(work),
        evidence,
    })
}

pub(crate) fn construct_pi_fresh_new_candidate(
    work: SourceImportFileWork,
    evidence: FreshNewCandidateEvidence,
) -> FreshNewCandidateResult {
    let source_path = work.file.source_path.clone();
    validate_candidate_common(
        work.reason,
        work.has_active_publication,
        work.file.provider == CaptureProvider::Pi
            && work.file.source_format == PI_SESSION_SOURCE_FORMAT,
        work.file.file_size_bytes,
        work.file.file_modified_at_ms,
        work.file
            .metadata
            .get("change_token_v1")
            .and_then(serde_json::Value::as_str),
        &source_path,
        &evidence,
    )?;
    Ok(FreshNewBatchCandidate {
        kind: FreshNewCandidateKind::PiOrdinaryFile,
        work: FreshNewCandidateWork::Pi(work),
        evidence,
    })
}

fn validate_candidate_common(
    reason: ImportPendingReason,
    has_active_publication: bool,
    supported_source: bool,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
    expected_observation_token: Option<&str>,
    source_path: &str,
    evidence: &FreshNewCandidateEvidence,
) -> std::result::Result<(), FreshNewDurableOnly> {
    let fail = |reason| FreshNewDurableOnly {
        source_path: source_path.to_owned(),
        reason,
    };
    if reason != ImportPendingReason::FreshNew {
        return Err(fail(FreshNewDurableOnlyReason::NotFreshNew(reason)));
    }
    if has_active_publication {
        return Err(fail(FreshNewDurableOnlyReason::PriorOrActivePublication));
    }
    if !supported_source {
        return Err(fail(FreshNewDurableOnlyReason::UnsupportedConcreteSource));
    }
    if evidence.inventory_generation == 0 {
        return Err(fail(FreshNewDurableOnlyReason::MissingCurrentGeneration));
    }
    if evidence.observation.token.trim().is_empty() || expected_observation_token.is_none() {
        return Err(fail(FreshNewDurableOnlyReason::MissingCurrentObservation));
    }
    if file_size_bytes != evidence.observation.file_size_bytes
        || file_modified_at_ms != evidence.observation.file_modified_at_ms
        || expected_observation_token != Some(evidence.observation.token.as_str())
    {
        return Err(fail(FreshNewDurableOnlyReason::ObservationChanged));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct FreshNewBatchPlan {
    pub(crate) group: Vec<FreshNewBatchCandidate>,
    pub(crate) remainder: Vec<FreshNewBatchCandidate>,
    pub(crate) durable_only: Vec<FreshNewDurableOnly>,
    pub(crate) estimated_bytes: u64,
}

pub(crate) fn plan_fresh_new_batch(
    candidates: impl IntoIterator<Item = FreshNewCandidateResult>,
) -> FreshNewBatchPlan {
    let mut plan = FreshNewBatchPlan {
        group: Vec::new(),
        remainder: Vec::new(),
        durable_only: Vec::new(),
        estimated_bytes: 0,
    };
    let mut paths = HashSet::new();
    let mut group_closed = false;

    for candidate in candidates {
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(route) => {
                plan.durable_only.push(route);
                continue;
            }
        };
        if candidate.estimated_bytes() >= FRESH_NEW_BATCH_MAX_BYTES {
            plan.durable_only.push(FreshNewDurableOnly {
                source_path: candidate.source_path().to_owned(),
                reason: FreshNewDurableOnlyReason::EstimatedPathOverLimit,
            });
            continue;
        }
        if !paths.insert(candidate.source_path().to_owned()) {
            plan.durable_only.push(FreshNewDurableOnly {
                source_path: candidate.source_path().to_owned(),
                reason: FreshNewDurableOnlyReason::DuplicateSourcePath,
            });
            continue;
        }
        if group_closed {
            plan.remainder.push(candidate);
            continue;
        }
        if plan
            .group
            .first()
            .is_some_and(|first| !first.same_group_scope(&candidate))
            || plan.group.len() == FRESH_NEW_BATCH_MAX_PATHS
            || plan
                .estimated_bytes
                .saturating_add(candidate.estimated_bytes())
                >= FRESH_NEW_BATCH_MAX_BYTES
        {
            group_closed = true;
            plan.remainder.push(candidate);
            continue;
        }
        plan.estimated_bytes = plan
            .estimated_bytes
            .saturating_add(candidate.estimated_bytes());
        plan.group.push(candidate);
    }
    plan
}

#[derive(Debug)]
pub(crate) struct FreshNewPreparedFile<T> {
    pub(crate) candidate: FreshNewBatchCandidate,
    pub(crate) payload: T,
}

#[derive(Debug)]
pub(crate) enum FreshNewFilePreparation<T> {
    Prepared {
        payload: T,
        actual_units: u64,
        actual_bytes: u64,
    },
    Rejected {
        summary: ProviderImportSummary,
        payload: T,
    },
    DurableOnly(FreshNewDurableOnlyReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshNewRejected<T> {
    pub(crate) source_path: String,
    pub(crate) summary: ProviderImportSummary,
    pub(crate) payload: T,
}

#[derive(Debug)]
pub(crate) struct FreshNewPreparedBatch<T> {
    pub(crate) files: Vec<FreshNewPreparedFile<T>>,
}

#[derive(Debug)]
pub(crate) struct FreshNewPreparation<T> {
    pub(crate) prepared: Option<FreshNewPreparedBatch<T>>,
    pub(crate) remainder: Vec<FreshNewBatchCandidate>,
    pub(crate) durable_only: Vec<FreshNewDurableOnly>,
    pub(crate) rejected: Vec<FreshNewRejected<T>>,
}

pub(crate) fn prepare_fresh_new_batch<T>(
    plan: FreshNewBatchPlan,
    mut prepare: impl FnMut(&FreshNewBatchCandidate) -> FreshNewFilePreparation<T>,
) -> FreshNewPreparation<T> {
    let mut files = Vec::with_capacity(plan.group.len());
    let mut durable_only = plan.durable_only;
    let mut rejected = Vec::new();
    let mut actual_units = 0_u64;
    let mut actual_bytes = 0_u64;

    let mut candidates = plan.group.into_iter();
    while let Some(candidate) = candidates.next() {
        match prepare(&candidate) {
            FreshNewFilePreparation::Prepared {
                payload,
                actual_units: file_units,
                actual_bytes: file_bytes,
            } => {
                let next_units = actual_units.saturating_add(file_units);
                let next_bytes = actual_bytes.saturating_add(file_bytes);
                if next_units > FRESH_NEW_BATCH_MAX_ACTUAL_UNITS
                    || next_bytes >= FRESH_NEW_BATCH_MAX_BYTES
                {
                    durable_only.extend(files.drain(..).map(|file: FreshNewPreparedFile<T>| {
                        FreshNewDurableOnly {
                            source_path: file.candidate.source_path().to_owned(),
                            reason: FreshNewDurableOnlyReason::ActualBatchOverLimit,
                        }
                    }));
                    durable_only.push(FreshNewDurableOnly {
                        source_path: candidate.source_path().to_owned(),
                        reason: FreshNewDurableOnlyReason::ActualBatchOverLimit,
                    });
                    durable_only.extend(candidates.map(|candidate| FreshNewDurableOnly {
                        source_path: candidate.source_path().to_owned(),
                        reason: FreshNewDurableOnlyReason::ActualBatchOverLimit,
                    }));
                    break;
                } else {
                    actual_units = next_units;
                    actual_bytes = next_bytes;
                    files.push(FreshNewPreparedFile { candidate, payload });
                }
            }
            FreshNewFilePreparation::Rejected { summary, payload } => {
                rejected.push(FreshNewRejected {
                    source_path: candidate.source_path().to_owned(),
                    summary,
                    payload,
                })
            }
            FreshNewFilePreparation::DurableOnly(reason) => {
                durable_only.extend(files.drain(..).map(|file: FreshNewPreparedFile<T>| {
                    FreshNewDurableOnly {
                        source_path: file.candidate.source_path().to_owned(),
                        reason: reason.clone(),
                    }
                }));
                durable_only.push(FreshNewDurableOnly {
                    source_path: candidate.source_path().to_owned(),
                    reason: reason.clone(),
                });
                durable_only.extend(candidates.map(|candidate| FreshNewDurableOnly {
                    source_path: candidate.source_path().to_owned(),
                    reason: reason.clone(),
                }));
                break;
            }
        }
    }

    FreshNewPreparation {
        prepared: (!files.is_empty()).then_some(FreshNewPreparedBatch { files }),
        remainder: plan.remainder,
        durable_only,
        rejected,
    }
}

#[derive(Debug, Clone)]
/// Shared identity needed to materialize one bounded FreshNew group.
pub struct FreshNewImportContext {
    /// Stable local machine identity used by normalized provider sources.
    pub machine_id: String,
    /// Source record created in the same transaction as imported material.
    pub history_record: HistoryRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FreshNewAdmission {
    Continue,
    StopPending,
    StopAfterMaintenanceError(String),
}

#[derive(Debug, Clone, Default)]
/// Result of attempting one bounded FreshNew group.
pub struct FreshNewImportOutcome {
    /// Import and deterministic-rejection counters for completed paths.
    pub summary: ProviderImportSummary,
    /// Paths whose content, status, and append checkpoint committed atomically.
    pub committed_paths: Vec<String>,
    /// Paths terminally rejected after deterministic parsing and revalidation.
    pub rejected_paths: Vec<String>,
    /// Paths durably redirected to the replacement state machine.
    pub durable_only_paths: Vec<String>,
    /// Valid FreshNew paths left for a later scheduler slice.
    pub remainder_paths: Vec<String>,
    /// Whether WAL/search maintenance requires admission to stop without spinning.
    pub maintenance_pending: bool,
    /// Post-commit maintenance failure, if one occurred.
    pub maintenance_error: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedFreshNewPayload {
    normalization: ProviderNormalizationResult,
    checkpoint: ProviderFileCheckpoint,
    visible_external_session_ids: Vec<(CaptureProvider, String)>,
    persist_cursors: bool,
}

/// Attempts one bounded atomic group of previously unseen Codex transcripts.
pub fn import_codex_fresh_new_batch(
    store: &mut Store,
    work: Vec<CatalogImportWork>,
    inventory_generation: u64,
    context: FreshNewImportContext,
) -> Result<FreshNewImportOutcome> {
    let candidates = work
        .into_iter()
        .map(|work| {
            let token = work
                .session
                .metadata
                .get("file_observation_token_v1")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let evidence = FreshNewCandidateEvidence {
                machine_id: context.machine_id.clone(),
                history_record_id: Some(context.history_record.id),
                inventory_generation,
                observation: FreshNewObservation {
                    file_size_bytes: work.session.file_size_bytes,
                    file_modified_at_ms: work.session.file_modified_at_ms,
                    token,
                },
            };
            let fallback = FreshNewBatchCandidate {
                kind: FreshNewCandidateKind::CodexCatalog,
                work: FreshNewCandidateWork::Codex(work.clone()),
                evidence: evidence.clone(),
            };
            (
                fallback,
                construct_codex_fresh_new_candidate(work, evidence),
            )
        })
        .collect::<Vec<_>>();
    import_fresh_new_candidates(store, candidates, context)
}

/// Attempts one bounded atomic group of previously unseen Pi transcripts.
pub fn import_pi_fresh_new_batch(
    store: &mut Store,
    work: Vec<SourceImportFileWork>,
    inventory_generation: u64,
    context: FreshNewImportContext,
) -> Result<FreshNewImportOutcome> {
    let candidates = work
        .into_iter()
        .map(|work| {
            let token = work
                .file
                .metadata
                .get("change_token_v1")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let evidence = FreshNewCandidateEvidence {
                machine_id: context.machine_id.clone(),
                history_record_id: Some(context.history_record.id),
                inventory_generation,
                observation: FreshNewObservation {
                    file_size_bytes: work.file.file_size_bytes,
                    file_modified_at_ms: work.file.file_modified_at_ms,
                    token,
                },
            };
            let fallback = FreshNewBatchCandidate {
                kind: FreshNewCandidateKind::PiOrdinaryFile,
                work: FreshNewCandidateWork::Pi(work.clone()),
                evidence: evidence.clone(),
            };
            (fallback, construct_pi_fresh_new_candidate(work, evidence))
        })
        .collect::<Vec<_>>();
    import_fresh_new_candidates(store, candidates, context)
}

fn import_fresh_new_candidates(
    store: &mut Store,
    candidates: Vec<(FreshNewBatchCandidate, FreshNewCandidateResult)>,
    context: FreshNewImportContext,
) -> Result<FreshNewImportOutcome> {
    let mut candidate_by_path = BTreeMap::new();
    for (candidate, _) in &candidates {
        candidate_by_path.insert(candidate.source_path().to_owned(), candidate.clone());
    }
    let plan = plan_fresh_new_batch(candidates.into_iter().map(|(_, result)| result));
    let preparation = prepare_fresh_new_batch(plan, prepare_fresh_new_candidate);
    let FreshNewPreparation {
        prepared,
        remainder,
        durable_only,
        rejected,
    } = preparation;

    let mut outcome = FreshNewImportOutcome {
        remainder_paths: remainder
            .iter()
            .map(|candidate| candidate.source_path().to_owned())
            .collect(),
        durable_only_paths: durable_only
            .iter()
            .map(|route| route.source_path.clone())
            .collect(),
        ..FreshNewImportOutcome::default()
    };
    defer_fresh_new_paths(store, &candidate_by_path, &outcome.durable_only_paths)?;

    if !rejected.is_empty() {
        let rejected_paths = rejected
            .iter()
            .map(|rejection| rejection.source_path.clone())
            .collect::<Vec<_>>();
        let rejected_candidates = candidates_for_paths(&candidate_by_path, &rejected_paths)?;
        let errors = rejected
            .iter()
            .map(|rejection| summary_failure(&rejection.summary))
            .collect::<Vec<_>>();
        let rejected_outcomes = rejected_candidates
            .iter()
            .zip(&errors)
            .map(|(candidate, error)| {
                candidate.import_outcome(
                    CatalogIndexedStatus::Rejected,
                    error.as_deref(),
                    None,
                    utc_now().timestamp_millis(),
                )
            })
            .collect::<Vec<_>>();
        let rejection_stability_paths = rejected
            .iter()
            .map(|rejection| {
                let candidate = candidate_by_path.get(&rejection.source_path).ok_or(
                    CaptureError::SystemInvariant("FreshNew rejection lost its source candidate"),
                )?;
                Ok((
                    PathBuf::from(&rejection.source_path),
                    candidate.evidence.observation.clone(),
                    rejection.payload.checkpoint.clone(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        if store.reject_fresh_new_atomic_batch::<CaptureError>(&rejected_outcomes, || {
            fresh_new_sources_are_stable(&rejection_stability_paths)
        })? {
            outcome.rejected_paths = rejected_paths;
            for rejection in rejected {
                outcome.summary.merge_from(rejection.summary);
            }
        } else {
            defer_fresh_new_paths(store, &candidate_by_path, &rejected_paths)?;
            outcome.durable_only_paths.extend(rejected_paths);
        }
    }

    let Some(batch) = prepared else {
        return Ok(outcome);
    };
    let batch_paths = batch
        .files
        .iter()
        .map(|file| file.candidate.source_path().to_owned())
        .collect::<Vec<_>>();
    let statuses = batch
        .files
        .iter()
        .map(|file| normalization_status(&file.payload.normalization))
        .collect::<Vec<_>>();
    let errors = batch
        .files
        .iter()
        .map(|file| summary_failure(&file.payload.normalization.summary))
        .collect::<Vec<_>>();
    let event_counts = batch
        .files
        .iter()
        .map(|file| {
            u64::try_from(
                file.payload
                    .normalization
                    .captures
                    .iter()
                    .filter(|(_, capture)| capture.event.is_some())
                    .count(),
            )
            .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    let indexed_at_ms = utc_now().timestamp_millis();
    let store_outcomes = batch
        .files
        .iter()
        .zip(&statuses)
        .zip(&errors)
        .zip(&event_counts)
        .map(|(((file, status), error), event_count)| {
            file.candidate.import_outcome(
                *status,
                error.as_deref(),
                Some(*event_count),
                indexed_at_ms,
            )
        })
        .collect::<Vec<_>>();
    let checkpoints = batch
        .files
        .iter()
        .map(|file| file.payload.checkpoint.clone())
        .collect::<Vec<_>>();
    let visible_ids = batch
        .files
        .iter()
        .flat_map(|file| file.payload.visible_external_session_ids.clone())
        .collect::<Vec<_>>();
    let payloads = batch
        .files
        .iter()
        .map(|file| file.payload.clone())
        .collect::<Vec<_>>();
    let stability_paths = batch
        .files
        .iter()
        .map(|file| {
            (
                PathBuf::from(file.candidate.source_path()),
                file.candidate.evidence.observation.clone(),
                file.payload.checkpoint.clone(),
            )
        })
        .collect::<Vec<_>>();
    let history_record = context.history_record;
    let committed = store.commit_fresh_new_atomic_batch::<ProviderImportSummary, CaptureError>(
        &store_outcomes,
        &checkpoints,
        &visible_ids,
        || fresh_new_sources_are_stable(&stability_paths),
        |store| {
            store.upsert_record(&history_record)?;
            let mut summary = ProviderImportSummary::default();
            for payload in payloads {
                summary.merge_from(import_normalized_provider_captures(
                    store,
                    payload.normalization,
                    NormalizedProviderImportOptions {
                        history_record_id: Some(history_record.id),
                        persist_cursors: payload.persist_cursors,
                        wrap_transaction: false,
                        fast_event_inserts: true,
                    },
                )?);
            }
            Ok(summary)
        },
    )?;
    let Some(summary) = committed else {
        defer_fresh_new_paths(store, &candidate_by_path, &batch_paths)?;
        outcome.durable_only_paths.extend(batch_paths);
        return Ok(outcome);
    };
    outcome.summary.merge_from(summary);
    outcome.committed_paths = batch_paths;
    match maintain_fresh_new_group_admission(store) {
        FreshNewAdmission::Continue => {}
        FreshNewAdmission::StopPending => outcome.maintenance_pending = true,
        FreshNewAdmission::StopAfterMaintenanceError(error) => {
            outcome.maintenance_error = Some(error)
        }
    }
    Ok(outcome)
}

fn prepare_fresh_new_candidate(
    candidate: &FreshNewBatchCandidate,
) -> FreshNewFilePreparation<PreparedFreshNewPayload> {
    match prepare_fresh_new_candidate_inner(candidate) {
        Ok(payload)
            if normalization_status(&payload.normalization) == CatalogIndexedStatus::Rejected =>
        {
            FreshNewFilePreparation::Rejected {
                summary: payload.normalization.summary.clone(),
                payload,
            }
        }
        Ok(payload) => match normalization_actual_bytes(&payload.normalization) {
            Ok(actual_bytes) => FreshNewFilePreparation::Prepared {
                actual_units: u64::try_from(
                    payload.normalization.captures.len()
                        + payload.normalization.files_touched.len(),
                )
                .unwrap_or(u64::MAX)
                .max(1),
                actual_bytes,
                payload,
            },
            Err(error) => FreshNewFilePreparation::DurableOnly(
                FreshNewDurableOnlyReason::TransientPreparation(error.to_string()),
            ),
        },
        Err(FreshNewPreparationError::DurableOnly(reason)) => {
            FreshNewFilePreparation::DurableOnly(reason)
        }
        Err(FreshNewPreparationError::Capture(error)) => FreshNewFilePreparation::DurableOnly(
            FreshNewDurableOnlyReason::TransientPreparation(error.to_string()),
        ),
    }
}

fn normalization_actual_bytes(normalization: &ProviderNormalizationResult) -> Result<u64> {
    normalization
        .captures
        .iter()
        .map(|(_, capture)| serde_json::to_vec(capture))
        .chain(
            normalization
                .files_touched
                .iter()
                .map(|(_, file)| serde_json::to_vec(file)),
        )
        .try_fold(0_u64, |total, encoded| {
            let bytes = u64::try_from(encoded?.len()).unwrap_or(u64::MAX);
            Ok(total.saturating_add(bytes))
        })
}

enum FreshNewPreparationError {
    DurableOnly(FreshNewDurableOnlyReason),
    Capture(CaptureError),
}

impl From<CaptureError> for FreshNewPreparationError {
    fn from(error: CaptureError) -> Self {
        Self::Capture(error)
    }
}

fn prepare_fresh_new_candidate_inner(
    candidate: &FreshNewBatchCandidate,
) -> std::result::Result<PreparedFreshNewPayload, FreshNewPreparationError> {
    let path = Path::new(candidate.source_path());
    let context = ProviderAdapterContext {
        machine_id: candidate.evidence.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: (candidate.kind == FreshNewCandidateKind::CodexCatalog)
            .then(|| PathBuf::from(candidate.source_root())),
        imported_at: utc_now(),
    };
    let (normalization, checkpoint, persist_cursors) = match candidate.kind {
        FreshNewCandidateKind::CodexCatalog => {
            let normalization = CodexSessionJsonlAdapter.normalize_path(path, &context)?;
            let checkpoint = codex_fresh_new_checkpoint(path, &context)?;
            (normalization, checkpoint, false)
        }
        FreshNewCandidateKind::PiOrdinaryFile => {
            let normalization = PiSessionJsonlAdapter.normalize_path(path, &context)?;
            let checkpoint = ordinary_fresh_new_checkpoint(path)?;
            (normalization, checkpoint, true)
        }
    };
    let checkpoint = checkpoint.map_err(|reason| {
        FreshNewPreparationError::DurableOnly(FreshNewDurableOnlyReason::TransientPreparation(
            format!("checkpoint certification required durable replacement: {reason:?}"),
        ))
    })?;
    let visible_external_session_ids = normalization
        .captures
        .iter()
        .map(|(_, capture)| {
            (
                capture.provider,
                capture.session.provider_session_id.clone(),
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if let FreshNewCandidateWork::Codex(work) = &candidate.work {
        let expected = work
            .session
            .external_session_id
            .as_deref()
            .unwrap_or_default();
        if !visible_external_session_ids
            .iter()
            .any(|(provider, identity)| *provider == CaptureProvider::Codex && identity == expected)
        {
            return Err(FreshNewPreparationError::DurableOnly(
                FreshNewDurableOnlyReason::MissingVisibleIdentity,
            ));
        }
    }
    Ok(PreparedFreshNewPayload {
        normalization,
        checkpoint: store_checkpoint_from_capture(candidate, &checkpoint)?,
        visible_external_session_ids,
        persist_cursors,
    })
}

fn ordinary_fresh_new_checkpoint(
    path: &Path,
) -> Result<std::result::Result<ProviderJsonlAppendCheckpoint, ProviderJsonlReplacementReason>> {
    let mut reader = ProviderJsonlReader::open_replacement(path)?;
    let mut line = Vec::new();
    loop {
        if matches!(
            reader.read_record(&mut line)?,
            ProviderJsonlRecordRead::Eof | ProviderJsonlRecordRead::DeferredPartial { .. }
        ) {
            break;
        }
    }
    reader.safe_checkpoint()
}

fn codex_fresh_new_checkpoint(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<std::result::Result<ProviderJsonlAppendCheckpoint, ProviderJsonlReplacementReason>> {
    let mut reader = ProviderJsonlReader::open_replacement(path)?;
    let mut header = None;
    let mut session_meta_seen = false;
    let mut call_contexts = CodexToolCallContexts::default();
    let mut line = Vec::new();
    loop {
        let line_number = match reader.read_record(&mut line)? {
            ProviderJsonlRecordRead::Eof | ProviderJsonlRecordRead::DeferredPartial { .. } => break,
            ProviderJsonlRecordRead::Oversized { .. } => continue,
            ProviderJsonlRecordRead::Record { line_number, .. } => {
                usize::try_from(line_number).unwrap_or(usize::MAX)
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) || !should_parse_codex_session_line(&line) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("session_meta") {
            if session_meta_seen {
                continue;
            }
            session_meta_seen = true;
            if let Ok(parsed) = codex_session_header(value) {
                call_contexts.clear();
                header = Some(parsed);
            }
            continue;
        }
        let Some(header) = header.as_ref() else {
            continue;
        };
        let Ok(occurred_at) = codex_session_line_timestamp(&value, header.timestamp) else {
            continue;
        };
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());
        codex_session_line_capture(
            header,
            &value,
            &mut call_contexts,
            CodexSessionLineContext {
                line_number,
                occurred_at,
                raw_source_path: raw_source_path.as_deref(),
                source_root: context.source_root_display().as_deref(),
                source_format: CODEX_SESSION_SOURCE_FORMAT,
            },
        );
    }
    reader.safe_checkpoint().map(|checkpoint| {
        checkpoint.map(|mut checkpoint| {
            checkpoint.resume_state = Some(ProviderJsonlResumeState::CodexSession(
                call_contexts.resume_state(),
            ));
            checkpoint
        })
    })
}

fn store_checkpoint_from_capture(
    candidate: &FreshNewBatchCandidate,
    checkpoint: &ProviderJsonlAppendCheckpoint,
) -> Result<ProviderFileCheckpoint> {
    let resume_state = checkpoint
        .resume_state
        .as_ref()
        .map(ProviderJsonlResumeState::encode_persisted_json)
        .transpose()?
        .map(String::into_bytes);
    Ok(ProviderFileCheckpoint {
        provider: candidate.provider(),
        source_format: candidate.source_format().to_owned(),
        source_root: candidate.source_root().to_owned(),
        source_path: candidate.source_path().to_owned(),
        import_revision: candidate.import_revision(),
        checkpoint_version: checkpoint.version,
        stable_file_identity: checkpoint.stable_identity.to_storage_key(),
        committed_byte_offset: checkpoint.committed_offset,
        committed_complete_line_count: checkpoint.complete_line_count,
        head_sha256: checkpoint.head_sha256.clone(),
        boundary_sha256: checkpoint.boundary_sha256.clone(),
        resume_state,
        updated_at_ms: utc_now().timestamp_millis(),
    })
}

fn fresh_new_sources_are_stable(
    sources: &[(PathBuf, FreshNewObservation, ProviderFileCheckpoint)],
) -> Result<bool> {
    for (path, observation, checkpoint) in sources {
        let metadata = fs::metadata(path)?;
        let modified_at_ms = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
            .as_millis();
        if metadata.len() != observation.file_size_bytes
            || i64::try_from(modified_at_ms).unwrap_or(i64::MAX) != observation.file_modified_at_ms
        {
            return Ok(false);
        }
        let capture_checkpoint = ProviderJsonlAppendCheckpoint {
            version: checkpoint.checkpoint_version,
            stable_identity: crate::ProviderFileStableIdentity::from_storage_key(
                &checkpoint.stable_file_identity,
            )
            .ok_or_else(|| {
                CaptureError::InvalidPayload("fresh-new stable identity is invalid".to_owned())
            })?,
            committed_offset: checkpoint.committed_byte_offset,
            complete_line_count: checkpoint.committed_complete_line_count,
            head_sha256: checkpoint.head_sha256.clone(),
            boundary_sha256: checkpoint.boundary_sha256.clone(),
            resume_state: None,
        };
        if !provider_jsonl_checkpoint_matches_file(path, &capture_checkpoint)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn normalization_status(normalization: &ProviderNormalizationResult) -> CatalogIndexedStatus {
    if normalization.summary.failed == 0 {
        CatalogIndexedStatus::Indexed
    } else if !normalization.captures.is_empty() || !normalization.files_touched.is_empty() {
        CatalogIndexedStatus::CompletedWithRejections
    } else {
        CatalogIndexedStatus::Rejected
    }
}

fn summary_failure(summary: &ProviderImportSummary) -> Option<String> {
    summary.failures.first().map(|failure| {
        if failure.line == 0 {
            failure.error.clone()
        } else {
            format!("line {}: {}", failure.line, failure.error)
        }
    })
}

fn candidates_for_paths<'a>(
    candidates: &'a BTreeMap<String, FreshNewBatchCandidate>,
    paths: &[String],
) -> Result<Vec<&'a FreshNewBatchCandidate>> {
    paths
        .iter()
        .map(|path| {
            candidates.get(path).ok_or(CaptureError::SystemInvariant(
                "FreshNew preparation lost its source candidate",
            ))
        })
        .collect()
}

fn defer_fresh_new_paths(
    store: &mut Store,
    candidates: &BTreeMap<String, FreshNewBatchCandidate>,
    paths: &[String],
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let candidates = candidates_for_paths(candidates, paths)?;
    let outcomes = candidates
        .iter()
        .map(|candidate| {
            candidate.import_outcome(
                CatalogIndexedStatus::Failed,
                Some("FreshNew eligibility requires durable replacement"),
                None,
                utc_now().timestamp_millis(),
            )
        })
        .collect::<Vec<_>>();
    store.defer_fresh_new_atomic_batch(&outcomes)?;
    Ok(())
}

fn maintain_fresh_new_group_admission(store: &Store) -> FreshNewAdmission {
    match store.checkpoint_wal_truncate_required_if_larger_than(FRESH_NEW_WAL_CHECKPOINT_BYTES) {
        Ok(_) => {}
        Err(StoreError::WalCheckpointBusy { .. }) => return FreshNewAdmission::StopPending,
        Err(error) => return FreshNewAdmission::StopAfterMaintenanceError(error.to_string()),
    }
    match store.maintain_event_search_bulk_mode() {
        Ok(EventSearchBulkMaintenanceOutcome::Complete) => FreshNewAdmission::Continue,
        Ok(EventSearchBulkMaintenanceOutcome::Pending)
        | Err(StoreError::WalCheckpointBusy { .. }) => FreshNewAdmission::StopPending,
        Err(error) => FreshNewAdmission::StopAfterMaintenanceError(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{AgentType, CaptureProvider};
    use ctx_history_store::{CatalogSession, SourceImportFile};
    use serde_json::json;

    use super::*;

    fn evidence(token: &str) -> FreshNewCandidateEvidence {
        FreshNewCandidateEvidence {
            machine_id: "machine".to_owned(),
            history_record_id: None,
            inventory_generation: 7,
            observation: FreshNewObservation {
                file_size_bytes: 10,
                file_modified_at_ms: 20,
                token: token.to_owned(),
            },
        }
    }

    fn codex(
        path: &str,
        reason: ImportPendingReason,
        estimated_bytes: u64,
    ) -> FreshNewCandidateResult {
        construct_codex_fresh_new_candidate(
            CatalogImportWork {
                session: CatalogSession {
                    provider: CaptureProvider::Codex,
                    source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
                    source_root: "/codex".to_owned(),
                    source_path: path.to_owned(),
                    external_session_id: Some(path.to_owned()),
                    parent_external_session_id: None,
                    agent_type: AgentType::Primary,
                    role_hint: None,
                    external_agent_id: None,
                    cwd: None,
                    session_started_at_ms: None,
                    file_size_bytes: 10,
                    file_modified_at_ms: 20,
                    import_revision: 1,
                    cataloged_at_ms: 30,
                    metadata: json!({"file_observation_token_v1": "observed"}),
                },
                reason,
                estimated_bytes,
                last_attempt_at_ms: None,
                has_active_publication: false,
            },
            evidence("observed"),
        )
    }

    fn pi(path: &str) -> FreshNewCandidateResult {
        construct_pi_fresh_new_candidate(
            SourceImportFileWork {
                file: SourceImportFile {
                    provider: CaptureProvider::Pi,
                    source_format: PI_SESSION_SOURCE_FORMAT.to_owned(),
                    source_root: "/pi".to_owned(),
                    source_path: path.to_owned(),
                    file_size_bytes: 10,
                    file_modified_at_ms: 20,
                    import_revision: 1,
                    observed_at_ms: 30,
                    metadata: json!({"change_token_v1": "observed"}),
                },
                reason: ImportPendingReason::FreshNew,
                estimated_bytes: 10,
                last_attempt_at_ms: None,
                has_active_publication: false,
            },
            evidence("observed"),
        )
    }

    #[test]
    fn only_fresh_new_constructs_a_candidate() {
        for reason in [
            ImportPendingReason::FreshChanged,
            ImportPendingReason::FreshAppend,
            ImportPendingReason::AbandonedPublication,
        ] {
            let route = codex("session.jsonl", reason, 10).unwrap_err();
            assert_eq!(route.reason, FreshNewDurableOnlyReason::NotFreshNew(reason));
        }
        assert!(codex("session.jsonl", ImportPendingReason::FreshNew, 10).is_ok());
        assert!(pi("session.jsonl").is_ok());
    }

    #[test]
    fn planner_enforces_path_and_strict_byte_bounds() {
        let candidates = (0..=FRESH_NEW_BATCH_MAX_PATHS)
            .map(|index| codex(&format!("{index}.jsonl"), ImportPendingReason::FreshNew, 1));
        let plan = plan_fresh_new_batch(candidates);
        assert_eq!(plan.group.len(), FRESH_NEW_BATCH_MAX_PATHS);
        assert_eq!(plan.remainder.len(), 1);

        let plan = plan_fresh_new_batch([codex(
            "large.jsonl",
            ImportPendingReason::FreshNew,
            FRESH_NEW_BATCH_MAX_BYTES,
        )]);
        assert!(plan.group.is_empty());
        assert_eq!(
            plan.durable_only[0].reason,
            FreshNewDurableOnlyReason::EstimatedPathOverLimit
        );
    }

    #[test]
    fn planner_emits_only_one_homogeneous_group() {
        let plan = plan_fresh_new_batch([
            codex("codex.jsonl", ImportPendingReason::FreshNew, 10),
            pi("pi.jsonl"),
        ]);
        assert_eq!(plan.group.len(), 1);
        assert_eq!(plan.remainder.len(), 1);
        assert_eq!(plan.group[0].kind, FreshNewCandidateKind::CodexCatalog);
    }

    #[test]
    fn deterministic_rejection_is_not_requeued() {
        let plan =
            plan_fresh_new_batch([codex("malformed.jsonl", ImportPendingReason::FreshNew, 10)]);
        let mut summary = ProviderImportSummary::default();
        summary.failed = 1;
        let prepared: FreshNewPreparation<()> =
            prepare_fresh_new_batch(plan, |_| FreshNewFilePreparation::Rejected {
                summary: summary.clone(),
                payload: (),
            });
        assert!(prepared.prepared.is_none());
        assert!(prepared.durable_only.is_empty());
        assert_eq!(prepared.rejected.len(), 1);
        assert_eq!(prepared.rejected[0].summary.failed, 1);
    }

    #[test]
    fn actual_over_limit_routes_prepared_paths_durable_only() {
        let plan = plan_fresh_new_batch([
            codex("a.jsonl", ImportPendingReason::FreshNew, 10),
            codex("b.jsonl", ImportPendingReason::FreshNew, 10),
        ]);
        let prepared =
            prepare_fresh_new_batch(plan, |candidate| FreshNewFilePreparation::Prepared {
                payload: (),
                actual_units: if candidate.source_path() == "a.jsonl" {
                    FRESH_NEW_BATCH_MAX_ACTUAL_UNITS
                } else {
                    1
                },
                actual_bytes: 10,
            });
        assert!(prepared.prepared.is_none());
        assert_eq!(prepared.durable_only.len(), 2);
        assert!(prepared
            .durable_only
            .iter()
            .all(|route| route.reason == FreshNewDurableOnlyReason::ActualBatchOverLimit));
    }

    #[test]
    fn transient_preparation_routes_the_whole_group_durable_only() {
        let plan = plan_fresh_new_batch([
            codex("a.jsonl", ImportPendingReason::FreshNew, 10),
            codex("b.jsonl", ImportPendingReason::FreshNew, 10),
            codex("c.jsonl", ImportPendingReason::FreshNew, 10),
        ]);
        let prepared = prepare_fresh_new_batch(plan, |candidate| {
            if candidate.source_path() == "b.jsonl" {
                FreshNewFilePreparation::DurableOnly(FreshNewDurableOnlyReason::ObservationChanged)
            } else {
                FreshNewFilePreparation::Prepared {
                    payload: (),
                    actual_units: 1,
                    actual_bytes: 10,
                }
            }
        });
        assert!(prepared.prepared.is_none());
        assert_eq!(prepared.durable_only.len(), 3);
        assert!(prepared
            .durable_only
            .iter()
            .all(|route| route.reason == FreshNewDurableOnlyReason::ObservationChanged));
    }
}

use std::collections::HashSet;

use ctx_history_core::CaptureProvider;
use ctx_history_store::{
    CatalogImportWork, EventSearchBulkMaintenanceOutcome, ImportPendingReason,
    SourceImportFileWork, Store, StoreError,
};
use uuid::Uuid;

use crate::{ProviderImportSummary, CODEX_SESSION_SOURCE_FORMAT};

pub(crate) const FRESH_NEW_BATCH_MAX_PATHS: usize = 1_024;
pub(crate) const FRESH_NEW_BATCH_MAX_ACTUAL_UNITS: u64 = 4_096;
pub(crate) const FRESH_NEW_BATCH_MAX_BYTES: u64 = 8 * 1_024 * 1_024;
const PI_SESSION_SOURCE_FORMAT: &str = "pi_session_jsonl";
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
    pub(crate) ownership_token: String,
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

    fn same_group_scope(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.provider() == other.provider()
            && self.source_format() == other.source_format()
            && self.source_root() == other.source_root()
            && self.evidence.machine_id == other.evidence.machine_id
            && self.evidence.history_record_id == other.evidence.history_record_id
            && self.evidence.inventory_generation == other.evidence.inventory_generation
            && self.evidence.ownership_token == other.evidence.ownership_token
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FreshNewDurableOnlyReason {
    NotFreshNew(ImportPendingReason),
    PriorOrActivePublication,
    UnsupportedConcreteSource,
    MissingCurrentGeneration,
    MissingCurrentOwnership,
    MissingCurrentObservation,
    MissingVisibleIdentity,
    ObservationChanged,
    DuplicateSourcePath,
    EstimatedPathOverLimit,
    ActualBatchOverLimit,
    EligibilityConflict,
    TransientPreparation(String),
    AtomicCommitConflict(String),
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
    if evidence.ownership_token.trim().is_empty() {
        return Err(fail(FreshNewDurableOnlyReason::MissingCurrentOwnership));
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
        if plan.group.first().is_some_and(|first| !first.same_group_scope(&candidate))
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
    pub(crate) actual_units: u64,
    pub(crate) actual_bytes: u64,
}

#[derive(Debug)]
pub(crate) enum FreshNewFilePreparation<T> {
    Prepared {
        payload: T,
        actual_units: u64,
        actual_bytes: u64,
    },
    Rejected(ProviderImportSummary),
    DurableOnly(FreshNewDurableOnlyReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshNewRejected {
    pub(crate) source_path: String,
    pub(crate) summary: ProviderImportSummary,
}

#[derive(Debug)]
pub(crate) struct FreshNewPreparedBatch<T> {
    pub(crate) files: Vec<FreshNewPreparedFile<T>>,
    pub(crate) actual_units: u64,
    pub(crate) actual_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct FreshNewPreparation<T> {
    pub(crate) prepared: Option<FreshNewPreparedBatch<T>>,
    pub(crate) remainder: Vec<FreshNewBatchCandidate>,
    pub(crate) durable_only: Vec<FreshNewDurableOnly>,
    pub(crate) rejected: Vec<FreshNewRejected>,
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
                    actual_units = 0;
                    actual_bytes = 0;
                    break;
                } else {
                    actual_units = next_units;
                    actual_bytes = next_bytes;
                    files.push(FreshNewPreparedFile {
                        candidate,
                        payload,
                        actual_units: file_units,
                        actual_bytes: file_bytes,
                    });
                }
            }
            FreshNewFilePreparation::Rejected(summary) => rejected.push(FreshNewRejected {
                source_path: candidate.source_path().to_owned(),
                summary,
            }),
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
                actual_units = 0;
                actual_bytes = 0;
                break;
            }
        }
    }

    FreshNewPreparation {
        prepared: (!files.is_empty()).then_some(FreshNewPreparedBatch {
            files,
            actual_units,
            actual_bytes,
        }),
        remainder: plan.remainder,
        durable_only,
        rejected,
    }
}

pub(crate) struct FreshNewAtomicCommitRequest<'a, T> {
    pub(crate) batch: &'a FreshNewPreparedBatch<T>,
    pub(crate) max_paths: usize,
    pub(crate) max_actual_units: u64,
    pub(crate) max_bytes_exclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FreshNewAtomicCommitDisposition {
    Committed,
    DurableOnly(FreshNewDurableOnlyReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FreshNewAdmission {
    Continue,
    StopPending,
    StopAfterMaintenanceError(String),
}

#[derive(Debug)]
pub(crate) struct FreshNewBatchExecution {
    pub(crate) committed_paths: Vec<String>,
    pub(crate) remainder: Vec<FreshNewBatchCandidate>,
    pub(crate) durable_only: Vec<FreshNewDurableOnly>,
    pub(crate) rejected: Vec<FreshNewRejected>,
    pub(crate) admission: FreshNewAdmission,
}

/// The callback is the store-owned atomic boundary. Before writing, it must
/// revalidate current generation, ownership, and observations and prove no
/// prior material, source/material attribution, visible identity, active or
/// abandoned publication, or cross-source identity collision. It must meter
/// actual units/bytes, then commit content, projections, inventory status,
/// indexed revision, observations, and the canonical provider-file append
/// checkpoint in one transaction. On conflict or error it must roll back and
/// durably mark every requested path as durable-only before returning
/// `DurableOnly`.
pub(crate) fn commit_prepared_fresh_new_batch<T>(
    store: &mut Store,
    preparation: FreshNewPreparation<T>,
    commit: impl FnOnce(
        &mut Store,
        FreshNewAtomicCommitRequest<'_, T>,
    ) -> FreshNewAtomicCommitDisposition,
) -> FreshNewBatchExecution {
    let FreshNewPreparation {
        prepared,
        remainder,
        mut durable_only,
        rejected,
    } = preparation;
    let Some(batch) = prepared else {
        return FreshNewBatchExecution {
            committed_paths: Vec::new(),
            remainder,
            durable_only,
            rejected,
            admission: FreshNewAdmission::Continue,
        };
    };
    let paths = batch
        .files
        .iter()
        .map(|file| file.candidate.source_path().to_owned())
        .collect::<Vec<_>>();
    let disposition = commit(
        store,
        FreshNewAtomicCommitRequest {
            batch: &batch,
            max_paths: FRESH_NEW_BATCH_MAX_PATHS,
            max_actual_units: FRESH_NEW_BATCH_MAX_ACTUAL_UNITS,
            max_bytes_exclusive: FRESH_NEW_BATCH_MAX_BYTES,
        },
    );
    let (committed_paths, admission) = match disposition {
        FreshNewAtomicCommitDisposition::Committed => {
            let admission = maintain_fresh_new_group_admission(store);
            (paths, admission)
        }
        FreshNewAtomicCommitDisposition::DurableOnly(reason) => {
            durable_only.extend(paths.into_iter().map(|source_path| FreshNewDurableOnly {
                source_path,
                reason: reason.clone(),
            }));
            (Vec::new(), FreshNewAdmission::Continue)
        }
    };
    FreshNewBatchExecution {
        committed_paths,
        remainder,
        durable_only,
        rejected,
        admission,
    }
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
            ownership_token: "owner-7".to_owned(),
            observation: FreshNewObservation {
                file_size_bytes: 10,
                file_modified_at_ms: 20,
                token: token.to_owned(),
            },
        }
    }

    fn codex(path: &str, reason: ImportPendingReason, estimated_bytes: u64) -> FreshNewCandidateResult {
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
        let plan = plan_fresh_new_batch([codex(
            "malformed.jsonl",
            ImportPendingReason::FreshNew,
            10,
        )]);
        let mut summary = ProviderImportSummary::default();
        summary.failed = 1;
        let prepared: FreshNewPreparation<()> = prepare_fresh_new_batch(plan, |_| {
            FreshNewFilePreparation::Rejected(summary.clone())
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
        let prepared = prepare_fresh_new_batch(plan, |candidate| {
            FreshNewFilePreparation::Prepared {
                payload: (),
                actual_units: if candidate.source_path() == "a.jsonl" {
                    FRESH_NEW_BATCH_MAX_ACTUAL_UNITS
                } else {
                    1
                },
                actual_bytes: 10,
            }
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
                FreshNewFilePreparation::DurableOnly(
                    FreshNewDurableOnlyReason::ObservationChanged,
                )
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

    #[test]
    fn commit_callback_receives_runtime_limits_and_returns_admission() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
        let plan = plan_fresh_new_batch([codex(
            "a.jsonl",
            ImportPendingReason::FreshNew,
            10,
        )]);
        let prepared = prepare_fresh_new_batch(plan, |_| FreshNewFilePreparation::Prepared {
            payload: "normalized",
            actual_units: 2,
            actual_bytes: 20,
        });

        let execution = commit_prepared_fresh_new_batch(&mut store, prepared, |_, request| {
            assert_eq!(request.batch.files.len(), 1);
            assert_eq!(request.batch.files[0].payload, "normalized");
            assert_eq!(request.max_paths, FRESH_NEW_BATCH_MAX_PATHS);
            assert_eq!(
                request.max_actual_units,
                FRESH_NEW_BATCH_MAX_ACTUAL_UNITS
            );
            assert_eq!(request.max_bytes_exclusive, FRESH_NEW_BATCH_MAX_BYTES);
            FreshNewAtomicCommitDisposition::Committed
        });

        assert_eq!(execution.committed_paths, ["a.jsonl"]);
        assert_eq!(execution.admission, FreshNewAdmission::Continue);
    }
}

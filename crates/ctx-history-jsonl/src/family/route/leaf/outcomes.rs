use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionDrafts, SourceBackedRouteResult, SourceBackedSourceOutcome,
};
use ctx_history_core::{CertifiedSource, CertifiedSourceAppend, SourceKey};

use super::{
    route_internal, route_invalid, JsonlFamilyError, JsonlFamilyLeaf, JsonlFamilyRuntime,
    JsonlFamilyTerminalProof, JsonlFamilyWorkerContext, JsonlResult, TerminalSourceEvidence,
};

pub(in crate::family::route) struct PreparedLeaf<E: JsonlFamilyError> {
    pub(in crate::family::route) certificate: CertifiedSource,
    pub(in crate::family::route) append: Option<CertifiedSourceAppend>,
    pub(in crate::family::route) terminal_proof: JsonlFamilyTerminalProof<E>,
    pub(in crate::family::route) record_rejections: SourceBackedRecordRejectionDrafts,
    pub(in crate::family::route) logical_source_quarantine: Option<(SourceKey, String)>,
}

pub(in crate::family::route) struct QuarantinedLeaf<E: JsonlFamilyError> {
    pub(in crate::family::route) claimed_source: SourceKey,
    pub(in crate::family::route) source_path: std::path::PathBuf,
    pub(in crate::family::route) terminal_proof: JsonlFamilyTerminalProof<E>,
    pub(in crate::family::route) failure_source: SourceKey,
    pub(in crate::family::route) detail: String,
    pub(in crate::family::route) certified_bytes: u64,
    pub(in crate::family::route) exact_scan_bytes: Option<u64>,
}

pub(in crate::family::route) struct LeafScanResult<E: JsonlFamilyError> {
    pub(in crate::family::route) terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence<E>>,
    pub(in crate::family::route) quarantined: Vec<QuarantinedLeaf<E>>,
}

// Both variants are transient per-source scan outcomes. Boxing them would add
// one allocation to every JSONL source, including the successful hot path.
#[allow(clippy::large_enum_variant)]
pub(super) enum LeafScanOutcome<E: JsonlFamilyError> {
    Certified(TerminalSourceEvidence<E>),
    Quarantined(QuarantinedLeaf<E>),
}

pub(super) fn quarantine_leaf<E: JsonlFamilyError>(
    leaf: &JsonlFamilyLeaf<E>,
    certificate: &CertifiedSource,
    append: Option<&CertifiedSourceAppend>,
    staged: bool,
    logical_source_quarantine: (SourceKey, String),
) -> JsonlResult<QuarantinedLeaf<E>, E> {
    if staged || append.is_some() || certificate.counts().retained_records != 0 {
        return Err(E::system_invariant(
            "JSONL quarantined leaf entered publication staging",
        ));
    }
    let (failure_source, detail) = logical_source_quarantine;
    Ok(QuarantinedLeaf {
        claimed_source: leaf.source().clone(),
        source_path: leaf.source_path().to_path_buf(),
        terminal_proof: JsonlFamilyTerminalProof::exact_path(
            leaf.source_path().to_path_buf(),
            Arc::clone(&leaf.authority),
            leaf.authority_path.clone(),
        )?,
        failure_source,
        detail,
        certified_bytes: certificate.counts().certified_bytes,
        exact_scan_bytes: leaf.exact_scan_bytes(),
    })
}

pub(super) struct JsonlLeafJob<E: JsonlFamilyError> {
    pub(super) leaf: JsonlFamilyLeaf<E>,
    pub(super) base: Option<CertifiedSource>,
    pub(super) context_shard: Option<u64>,
}

// Partitioned adapters receive deterministic logical cache lanes rather than
// caches tied to the physical worker count. Source-local event-time state is
// cleared by `begin_leaf()`, while revalidated repository certification caches
// remain stable across worker counts and physical scheduling decisions.
pub(super) struct JsonlFamilyWorkerContexts<R: JsonlFamilyRuntime> {
    independent: JsonlFamilyWorkerContext<R>,
    partition_cache_lanes: BTreeMap<u64, JsonlFamilyWorkerContext<R>>,
}

impl<R: JsonlFamilyRuntime> Default for JsonlFamilyWorkerContexts<R> {
    fn default() -> Self {
        Self {
            independent: JsonlFamilyWorkerContext::default(),
            partition_cache_lanes: BTreeMap::new(),
        }
    }
}

impl<R: JsonlFamilyRuntime> JsonlFamilyWorkerContexts<R> {
    pub(super) fn for_job(
        &mut self,
        context_shard: Option<u64>,
    ) -> &mut JsonlFamilyWorkerContext<R> {
        match context_shard {
            Some(context_shard) => self.partition_cache_lanes.entry(context_shard).or_default(),
            None => &mut self.independent,
        }
    }
}

pub(super) fn reconcile_parallel_leaf_outcomes<E: JsonlFamilyError>(
    outcomes: Vec<SourceBackedSourceOutcome<LeafScanOutcome<E>>>,
    failed_evidences: Vec<TerminalSourceEvidence<E>>,
) -> SourceBackedRouteResult<Vec<LeafScanOutcome<E>>> {
    let mut failed_evidences = failed_evidences
        .into_iter()
        .map(|evidence| {
            (
                evidence
                    .observed_certificate()
                    .observation()
                    .source()
                    .exact_descriptor_digest(),
                evidence,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut results = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            SourceBackedSourceOutcome::Success(result) => results.push(result),
            SourceBackedSourceOutcome::Failed(mut failure) => {
                if !failure.failure.kind.is_logical_source_failure() {
                    return Err(route_internal(
                        "parallel JSONL source failure was not logical",
                    ));
                }
                let mut evidence = failed_evidences
                    .remove(&failure.source.exact_descriptor_digest())
                    .ok_or_else(|| {
                        route_internal("parallel JSONL source failure lost terminal evidence")
                    })?;
                if !evidence
                    .observed_certificate()
                    .observation()
                    .source()
                    .exact_descriptor_eq(&failure.source)
                    || failure.retained.as_ref() != Some(&evidence.certificate)
                {
                    return Err(route_internal(
                        "parallel JSONL source failure evidence changed identity",
                    ));
                }
                evidence.record_rejections = std::mem::take(&mut failure.record_rejections);
                results.push(LeafScanOutcome::Certified(evidence));
            }
        }
    }
    if !failed_evidences.is_empty() {
        return Err(route_internal(
            "parallel JSONL terminal evidence has no source failure outcome",
        ));
    }
    Ok(results)
}

pub(super) fn collect_leaf_outcomes<E: JsonlFamilyError>(
    outcomes: Vec<LeafScanOutcome<E>>,
) -> SourceBackedRouteResult<LeafScanResult<E>> {
    let mut terminal_sources = HashMap::with_capacity(outcomes.len());
    let mut quarantined = Vec::new();
    for outcome in outcomes {
        match outcome {
            LeafScanOutcome::Certified(evidence) => {
                let digest = evidence
                    .certificate
                    .observation()
                    .source()
                    .exact_descriptor_digest();
                if terminal_sources.insert(digest, evidence).is_some() {
                    return Err(route_invalid("duplicate JSONL source identity"));
                }
            }
            LeafScanOutcome::Quarantined(leaf) => quarantined.push(leaf),
        }
    }
    Ok(LeafScanResult {
        terminal_sources,
        quarantined,
    })
}

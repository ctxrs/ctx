use std::{collections::BTreeMap, sync::Mutex};

use chrono::{DateTime, Utc};

use super::generation::CodexPreparedRouteV0;
use super::*;
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
            JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
            JsonlFamilyMembershipObservation, JsonlFamilyOptimizedLeafOutcome,
            JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRejectedLeaf,
            JsonlFamilyRootMissingMode, JsonlFamilyTerminalProof, JsonlFamilyWorkerContext,
        },
        SourceBackedRouteErrorKind,
    },
    Result,
};

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);
type CodexReplayLineageV0 = (CodexCatalogSource, CodexAppendProof, String);
const CODEX_LINEAGE_EXHAUSTED_DETAIL: &str =
    "Codex lineage working set exceeded its bounded task-local capacity";
const CODEX_LINEAGE_UNAVAILABLE_DETAIL: &str = "Codex lineage working set is unavailable";

fn codex_lineage_rejected_leaf_v0(
    rejected: CodexLineageRejectedSourceV0,
    authority_path: PathBuf,
) -> Result<JsonlFamilyRejectedLeaf> {
    let proof = TypedKey::bytes(serde_json::to_vec(&rejected.proof)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(JsonlFamilyRejectedLeaf::bind_observed(
        rejected.source.source_path,
        authority_path,
        proof,
        1,
    ))
}

#[derive(Default)]
struct CodexSessionJsonlFamilyStateV0 {
    plans: HashMap<SourceKey, CodexSessionPlanV0>,
    outcome_lineage: Option<Arc<CodexOutcomeLineageAuthorityV0>>,
    replay_lineage: BTreeMap<u64, Vec<CodexReplayLineageV0>>,
    counters: CodexSourceBackedCountersV0,
    stage_pending: bool,
}

fn scan_codex_session_jsonl_leaf_v0(
    adapter: &dyn JsonlFamilyAdapter,
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    base_event_lookup: &BaseEventIdentityLookup,
    worker: &mut JsonlFamilyWorkerContext,
    emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> Result<()>,
) -> Result<JsonlFamilyOptimizedLeafOutcome> {
    let (plan, outcome_lineage) = {
        let state = state.lock().map_err(|_| codex_family_state_error())?;
        let plan = state.plans.get(leaf.source()).cloned().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex JSONL family leaf has no native source plan".to_owned(),
            )
        })?;
        let outcome_lineage = state.outcome_lineage.clone().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex JSONL family has no opening lineage authority".to_owned(),
            )
        })?;
        (plan, outcome_lineage)
    };
    if plan.0.source_path != leaf.source_path() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut scan_context = CodexJsonlFamilyLeafContextV0 {
        base_event_lookup,
        outcome_lineage: &outcome_lineage,
        repository_attributor: worker.repository_attributor(),
    };
    let outcome = scan_codex_jsonl_family_leaf_v0(
        plan.0,
        plan.1,
        plan.2,
        base,
        &mut scan_context,
        |publication, records| {
            let publication = match publication {
                CodexJsonlFamilyPublicationV0::Append => JsonlFamilyPublication::Append,
                CodexJsonlFamilyPublicationV0::Replace => JsonlFamilyPublication::Replace,
            };
            emit_page(publication, records).map_err(CodexSourceBackedErrorV0::Capture)
        },
    )
    .map_err(codex_family_capture_error)?;
    let terminal_proof = JsonlFamilyTerminalProof::frozen_prefix(
        adapter,
        leaf,
        &outcome.certificate,
        outcome.terminal_prefix_bytes,
        outcome.terminal_prefix_sha256,
    )?;
    let family_outcome = match outcome.append {
        Some(append) => JsonlFamilyOptimizedLeafOutcome::append(append, terminal_proof),
        None => JsonlFamilyOptimizedLeafOutcome::replacement(outcome.certificate, terminal_proof),
    };
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    state.counters.add_assign(outcome.counters);
    state.stage_pending = true;
    Ok(family_outcome)
}

fn codex_family_state_error() -> CaptureError {
    CaptureError::InvalidPayload("Codex JSONL family state lock was poisoned".to_owned())
}

fn prepare_codex_session_jsonl_scans_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaves: &[JsonlFamilyLeaf],
    bases: &HashMap<[u8; 32], &CertifiedSource>,
    bind_route_sources: bool,
) -> Result<Option<usize>> {
    let (plans, outcome_lineage) = {
        let state = state.lock().map_err(|_| codex_family_state_error())?;
        let outcome_lineage = state.outcome_lineage.clone().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex JSONL family has no opening lineage authority".to_owned(),
            )
        })?;
        (state.plans.clone(), outcome_lineage)
    };
    let selected = leaves
        .iter()
        .map(|leaf| leaf.source().exact_descriptor_digest())
        .collect::<HashSet<_>>();
    let selected_native_session_ids = plans
        .iter()
        .filter(|(source_key, _)| selected.contains(&source_key.exact_descriptor_digest()))
        .map(|(_, (_, _, native_session_id))| native_session_id.clone())
        .collect::<HashSet<_>>();
    if bind_route_sources {
        outcome_lineage
            .bind_route_sources(&selected_native_session_ids)
            .map_err(codex_family_capture_error)?;
    }
    let mut replay_sources = Vec::new();
    let mut changed_ids = HashSet::new();
    for (source_key, (source, _, native_session_id)) in &plans {
        if !selected.contains(&source_key.exact_descriptor_digest()) {
            continue;
        }
        let base = bases
            .get(&source_key.exact_descriptor_digest())
            .copied()
            .filter(|base| base.observation().source().exact_descriptor_eq(source_key));
        let lineage_dependency_sha256 = outcome_lineage.dependency_digest(native_session_id);
        let proof = base
            .filter(|base| base.parser_revision() == CODEX_PARSER_REVISION)
            .and_then(|base| decode_append_proof(source, source_key, base).ok())
            .filter(|proof| {
                proof.checkpoint.lineage_dependency_sha256 == lineage_dependency_sha256
            });
        let replay_needed = outcome_lineage
            .needs_descendant_facts(native_session_id)
            .map_err(codex_family_capture_error)?;
        match proof.filter(|proof| proof.checkpoint.observation == source.catalog_observation) {
            Some(proof) if replay_needed => {
                replay_sources.push((source.clone(), proof, native_session_id.clone()));
            }
            Some(_) => {}
            None => {
                changed_ids.insert(native_session_id.clone());
            }
        }
    }
    let changed_partitions = changed_ids
        .iter()
        .filter_map(|native_session_id| outcome_lineage.component_partition(native_session_id))
        .collect::<HashSet<_>>();
    let mut replay_lineage = BTreeMap::<u64, Vec<CodexReplayLineageV0>>::new();
    if !changed_partitions.is_empty() {
        for replay in replay_sources {
            let partition = outcome_lineage
                .component_partition(&replay.2)
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Codex replay source has no lineage partition".to_owned(),
                    )
                })?;
            if changed_partitions.contains(&partition) {
                replay_lineage.entry(partition).or_default().push(replay);
            }
        }
    }
    for replay_sources in replay_lineage.values_mut() {
        replay_sources.sort_by(|left, right| {
            outcome_lineage
                .depth(&left.2)
                .cmp(&outcome_lineage.depth(&right.2))
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.0.source_path.cmp(&right.0.source_path))
        });
    }
    state
        .lock()
        .map_err(|_| codex_family_state_error())?
        .replay_lineage = replay_lineage;
    Ok(None)
}

fn codex_session_jsonl_scan_partition_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaf: &JsonlFamilyLeaf,
) -> Result<Option<u64>> {
    let state = state.lock().map_err(|_| codex_family_state_error())?;
    let outcome_lineage = state.outcome_lineage.as_ref().ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Codex JSONL family has no opening lineage authority".to_owned(),
        )
    })?;
    let (_, _, native_session_id) = state.plans.get(leaf.source()).ok_or_else(|| {
        CaptureError::InvalidPayload("Codex JSONL family leaf has no native source plan".to_owned())
    })?;
    outcome_lineage
        .component_partition(native_session_id)
        .map(Some)
        .ok_or_else(|| CaptureError::InvalidPayload("Codex lineage partition is absent".to_owned()))
}

fn begin_codex_session_jsonl_scan_partition_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    partition: u64,
) -> Result<()> {
    let (replay_sources, outcome_lineage) = {
        let state = state.lock().map_err(|_| codex_family_state_error())?;
        let outcome_lineage = state.outcome_lineage.clone().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex JSONL family has no opening lineage authority".to_owned(),
            )
        })?;
        (
            state
                .replay_lineage
                .get(&partition)
                .cloned()
                .unwrap_or_default(),
            outcome_lineage,
        )
    };
    prepare_replayed_lineage_v0(&replay_sources, &outcome_lineage)
        .map_err(codex_family_capture_error)
}

fn finish_codex_session_jsonl_scan_partition_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    partition: u64,
) -> Result<()> {
    let outcome_lineage = {
        let mut state = state.lock().map_err(|_| codex_family_state_error())?;
        state.replay_lineage.remove(&partition);
        state.outcome_lineage.clone().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex JSONL family has no opening lineage authority".to_owned(),
            )
        })?
    };
    outcome_lineage
        .release_component(partition)
        .map_err(codex_family_capture_error)
}

fn codex_session_jsonl_scan_phase_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaf: &JsonlFamilyLeaf,
) -> Result<usize> {
    let state = state.lock().map_err(|_| codex_family_state_error())?;
    let outcome_lineage = state.outcome_lineage.as_ref().ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Codex JSONL family has no opening lineage authority".to_owned(),
        )
    })?;
    let (_, _, native_session_id) = state.plans.get(leaf.source()).ok_or_else(|| {
        CaptureError::InvalidPayload("Codex JSONL family leaf has no native source plan".to_owned())
    })?;
    Ok(outcome_lineage.depth(native_session_id))
}

fn order_codex_session_jsonl_scans_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaves: &mut [JsonlFamilyLeaf],
) -> Result<()> {
    let state = state.lock().map_err(|_| codex_family_state_error())?;
    let outcome_lineage = state.outcome_lineage.as_ref().ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Codex JSONL family has no opening lineage authority".to_owned(),
        )
    })?;
    let mut depths = HashMap::with_capacity(leaves.len());
    for leaf in leaves.iter() {
        let (_, _, native_session_id) = state.plans.get(leaf.source()).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex JSONL family leaf has no native source plan".to_owned(),
            )
        })?;
        depths.insert(
            leaf.source().exact_descriptor_digest(),
            outcome_lineage.depth(native_session_id),
        );
    }
    leaves.sort_by(|left, right| {
        let left_depth = depths
            .get(&left.source().exact_descriptor_digest())
            .copied()
            .unwrap_or(usize::MAX);
        let right_depth = depths
            .get(&right.source().exact_descriptor_digest())
            .copied()
            .unwrap_or(usize::MAX);
        left_depth
            .cmp(&right_depth)
            // The retained native runner first orders by source-identity
            // digest and then stable-sorts by lineage depth. Reproduce that
            // equal-depth order so shared scheduling and writer admission do
            // not acquire a provider-specific tail-latency difference.
            .then_with(|| {
                left.source()
                    .identity()
                    .digest()
                    .cmp(&right.source().identity().digest())
            })
    });
    Ok(())
}

/// Codex's multi-root session inventory and native optimized JSONL leaf
/// executor. The shared family owns the generation lifecycle and bounded
/// per-source scheduler; this adapter retains the native prefilter, parser,
/// checkpoints, identities, projection, and commit-time prefix evidence.
#[derive(Clone)]
pub(crate) struct CodexSessionTreeJsonlFamilyAdapterV0 {
    roots: Arc<[PathBuf]>,
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    generation: Option<CodexGenerationRouteV0>,
    #[cfg(test)]
    lineage_budget_override: Option<Arc<CodexLineageFactBudgetV0>>,
    #[cfg(test)]
    after_stage: Option<fn(CodexSourceBackedCountersV0)>,
}

impl CodexSessionTreeJsonlFamilyAdapterV0 {
    pub(crate) fn new(mut roots: Vec<PathBuf>) -> CodexSourceBackedResultV0<Self> {
        roots.sort_by(|left, right| {
            codex_session_root_rank(left)
                .cmp(&codex_session_root_rank(right))
                .then_with(|| left.cmp(right))
        });
        roots.dedup();
        if roots.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Codex session-tree authority has no roots".to_owned(),
            )
            .into());
        }
        Ok(Self {
            roots: roots.into(),
            state: Arc::new(Mutex::new(CodexSessionJsonlFamilyStateV0::default())),
            generation: None,
            #[cfg(test)]
            lineage_budget_override: None,
            #[cfg(test)]
            after_stage: None,
        })
    }

    pub(crate) fn with_generation(mut self, generation: CodexGenerationRouteV0) -> Self {
        self.generation = Some(generation);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_after_stage_observer(
        mut self,
        observer: fn(CodexSourceBackedCountersV0),
    ) -> Self {
        self.after_stage = Some(observer);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_lineage_budget_limits(
        mut self,
        byte_limit: usize,
        fact_limit: usize,
    ) -> Self {
        self.lineage_budget_override = Some(Arc::new(CodexLineageFactBudgetV0::with_limits(
            byte_limit, fact_limit,
        )));
        self
    }

    pub(crate) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub(crate) fn discover(&self) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
        discover_codex_session_tree_inventory_v0(self.roots())
    }

    fn discover_family(&self, route_root: &Path) -> Result<JsonlFamilyInventory> {
        let _completed_stage = self.run_pending_stage_observer();
        // The shared family invokes this first discovery only after route
        // admission and before starting leaf workers. That opening inventory is
        // frozen by the shared lifecycle; construction and registration remain
        // free of recursive discovery, hashing, and provider metadata parsing.
        let prepared = match self.generation.as_ref() {
            Some(generation) => generation.prepared().map_err(codex_family_capture_error)?,
            None => {
                let inventory = self.discover().map_err(codex_family_capture_error)?;
                #[cfg(test)]
                let normalized = match self.lineage_budget_override.as_ref() {
                    Some(budget) => CodexOutcomeLineageAuthorityV0::normalize_sources_with_budget(
                        &inventory.sources,
                        Arc::clone(budget),
                    ),
                    None => CodexOutcomeLineageAuthorityV0::normalize_sources(&inventory.sources),
                };
                #[cfg(not(test))]
                let normalized =
                    CodexOutcomeLineageAuthorityV0::normalize_sources(&inventory.sources);
                let normalized = normalized.map_err(codex_family_capture_error)?;
                CodexPreparedRouteV0 {
                    missing: false,
                    sources: normalized.sources,
                    rejections: normalized.rejections,
                    authority: Arc::new(normalized.authority),
                    #[cfg(test)]
                    work: inventory.work,
                }
            }
        };
        if prepared.missing {
            return Err(CaptureError::SystemInvariant(
                "Codex session-tree generation partition is missing",
            ));
        }
        let outcome_lineage = prepared.authority;
        let normalized_sources = prepared.sources;
        let mut rejected_leaves = Vec::with_capacity(prepared.rejections.len());
        for rejected in prepared.rejections {
            let authority_path = rejected.source.authority_relative_path.clone().ok_or(
                CaptureError::SystemInvariant(
                    "rejected Codex catalog source has no authority path",
                ),
            )?;
            rejected_leaves.push(codex_lineage_rejected_leaf_v0(rejected, authority_path)?);
        }
        let mut ordered_sources = normalized_sources.iter().collect::<Vec<_>>();
        ordered_sources
            .sort_by_key(|(_, _, native_session_id)| outcome_lineage.depth(native_session_id));
        let mut authorities = BTreeMap::<PathBuf, Arc<ProviderSourceRoot>>::new();
        let mut leaves = Vec::with_capacity(normalized_sources.len());
        for (source, source_key, native_session_id) in ordered_sources {
            let authority = Arc::new(source.authority_root.clone().ok_or(
                CaptureError::SystemInvariant("Codex catalog source has no retained root"),
            )?);
            let authority_path =
                source
                    .authority_relative_path
                    .clone()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex catalog source has no authority path",
                    ))?;
            let opened = authority.open_file(&authority_path)?;
            let observation = observe_opened_file(&source.source_path, &opened)?;
            leaves.push(JsonlFamilyLeaf::bind_observed(
                source_key.clone(),
                source.source_path.clone(),
                Arc::clone(&authority),
                authority_path,
                TypedKey::utf8(native_session_id)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
                observation,
            ));
            authorities
                .entry(authority.named_path().to_path_buf())
                .or_insert(authority);
        }
        for root in self.roots() {
            if !authorities.contains_key(root) {
                let authority = Arc::new(ProviderSourceRoot::open(root)?);
                authorities.insert(authority.named_path().to_path_buf(), authority);
            }
        }
        let authorities = authorities.into_values().collect();
        let family_inventory = if rejected_leaves.is_empty() {
            JsonlFamilyInventory::present_multi(
                CaptureProvider::Codex,
                route_root,
                authorities,
                leaves,
            )?
        } else {
            JsonlFamilyInventory::present_multi_with_rejected(
                CaptureProvider::Codex,
                route_root,
                authorities,
                leaves,
                rejected_leaves,
            )?
        };
        let mut state = self.state.lock().map_err(|_| {
            CaptureError::InvalidPayload("Codex JSONL family state lock was poisoned".to_owned())
        })?;
        state.plans = normalized_sources
            .iter()
            .cloned()
            .map(|plan| (plan.1.clone(), plan))
            .collect();
        state.outcome_lineage = Some(outcome_lineage);
        state.replay_lineage.clear();
        state.counters = CodexSourceBackedCountersV0::default();
        #[cfg(test)]
        {
            state.counters.add_catalog_work(prepared.work);
            if normalized_sources.is_empty() && !_completed_stage {
                state.stage_pending = true;
            }
        }
        Ok(family_inventory)
    }

    fn run_pending_stage_observer(&self) -> bool {
        #[cfg(test)]
        {
            let counters = self.state.lock().ok().and_then(|mut state| {
                state.stage_pending.then(|| {
                    state.stage_pending = false;
                    state.counters
                })
            });
            if let (Some(observer), Some(counters)) = (self.after_stage, counters) {
                observer(counters);
            }
            counters.is_some()
        }
        #[cfg(not(test))]
        false
    }
}

impl JsonlFamilyAdapter for CodexSessionTreeJsonlFamilyAdapterV0 {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &'static str {
        CODEX_SESSION_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        CODEX_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        CODEX_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        self.discover_family(root)
    }

    fn observe_terminal_membership(
        &self,
        _root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation> {
        self.run_pending_stage_observer();
        let mut observation = JsonlFamilyMembershipObservation::observe_authorities(opening)?;
        let candidates = observation
            .unbound_routes()
            .map(|(path, authority, authority_path)| {
                (path.to_path_buf(), authority, authority_path.to_path_buf())
            })
            .collect::<Vec<_>>();
        for (path, authority, authority_path) in candidates {
            if let Some(native_session_id) = super::catalog::codex_terminal_native_session_id_hint(
                &path,
                &authority,
                &authority_path,
            )
            .map_err(codex_family_capture_error)?
            {
                observation.bind_source_hint(
                    path,
                    codex_source_key(&native_session_id).map_err(codex_family_capture_error)?,
                );
            }
        }
        Ok(observation)
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_discovery_error_kind(error)
    }

    fn scan_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_scan_error_kind(error)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        order_codex_session_jsonl_scans_v0(&self.state, leaves)
    }

    fn prepare_leaf_scans(
        &self,
        leaves: &[JsonlFamilyLeaf],
        bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> Result<Option<usize>> {
        prepare_codex_session_jsonl_scans_v0(&self.state, leaves, bases, self.generation.is_none())
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        codex_session_jsonl_scan_phase_v0(&self.state, leaf)
    }

    fn leaf_scan_partition(&self, leaf: &JsonlFamilyLeaf) -> Result<Option<u64>> {
        codex_session_jsonl_scan_partition_v0(&self.state, leaf)
    }

    fn begin_leaf_scan_partition(&self, partition: u64) -> Result<()> {
        begin_codex_session_jsonl_scan_partition_v0(&self.state, partition)
    }

    fn finish_leaf_scan_partition(&self, partition: u64) -> Result<()> {
        if self.generation.is_some() {
            Ok(())
        } else {
            finish_codex_session_jsonl_scan_partition_v0(&self.state, partition)
        }
    }

    fn finish_leaf_scans(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
        state.replay_lineage.clear();
        state.outcome_lineage = None;
        Ok(())
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "Codex JSONL leaves require the native optimized executor",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        base: Option<&CertifiedSource>,
        base_event_lookup: &BaseEventIdentityLookup,
        worker: &mut JsonlFamilyWorkerContext,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        #[cfg(test)]
        if let Some(generation) = self.generation.as_ref() {
            generation.record_worker_start();
        }
        scan_codex_session_jsonl_leaf_v0(
            self,
            &self.state,
            leaf,
            base,
            base_event_lookup,
            worker,
            emit_page,
        )
        .map(Some)
    }

    fn base_source_path(&self, _certificate: &CertifiedSource) -> Result<PathBuf> {
        self.roots
            .first()
            .cloned()
            .ok_or(CaptureError::SystemInvariant(
                "Codex JSONL family has no route root",
            ))
    }
}

/// One explicitly selected Codex rollout using the shared JSONL-family
/// lifecycle and the same native leaf executor as automatic discovery.
#[derive(Clone)]
pub(crate) struct CodexExplicitSessionJsonlFamilyAdapterV0 {
    input: CodexExplicitSessionSourceBackedInputV0,
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    generation: Option<CodexGenerationRouteV0>,
    #[cfg(test)]
    after_stage: Option<fn(CodexSourceBackedCountersV0)>,
}

impl CodexExplicitSessionJsonlFamilyAdapterV0 {
    pub(crate) fn new(input: CodexExplicitSessionSourceBackedInputV0) -> Self {
        Self {
            input,
            state: Arc::new(Mutex::new(CodexSessionJsonlFamilyStateV0::default())),
            generation: None,
            #[cfg(test)]
            after_stage: None,
        }
    }

    pub(crate) fn with_generation(mut self, generation: CodexGenerationRouteV0) -> Self {
        self.generation = Some(generation);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_after_stage_observer(
        mut self,
        observer: fn(CodexSourceBackedCountersV0),
    ) -> Self {
        self.after_stage = Some(observer);
        self
    }

    fn discover_family(&self, route_path: &Path) -> Result<JsonlFamilyInventory> {
        let _completed_stage = self.run_pending_stage_observer();
        if route_path != self.input.path() {
            return Err(CaptureError::InvalidPayload(
                "explicit Codex JSONL route path changed".to_owned(),
            ));
        }
        let prepared = match self.generation.as_ref() {
            Some(generation) => generation.prepared().map_err(codex_family_capture_error)?,
            None => {
                let inventory = observe_codex_explicit_session_source_backed_v0(&self.input)
                    .map_err(codex_family_capture_error)?;
                let Some(plan) = inventory.source_plan() else {
                    let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
                    state.plans.clear();
                    state.outcome_lineage = None;
                    state.counters = CodexSourceBackedCountersV0::default();
                    #[cfg(test)]
                    if !_completed_stage {
                        state.stage_pending = true;
                    }
                    return JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path);
                };
                let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&[plan])
                    .map_err(codex_family_capture_error)?;
                CodexPreparedRouteV0 {
                    missing: false,
                    sources: normalized.sources,
                    rejections: normalized.rejections,
                    authority: Arc::new(normalized.authority),
                    #[cfg(test)]
                    work: CodexCatalogWorkV0::default(),
                }
            }
        };
        if prepared.missing {
            let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
            state.plans.clear();
            state.outcome_lineage = None;
            state.counters = CodexSourceBackedCountersV0::default();
            #[cfg(test)]
            if !_completed_stage {
                state.stage_pending = true;
            }
            return JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path);
        }
        let parent = route_path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no parent".to_owned())
        })?;
        let authority_path = route_path.file_name().map(PathBuf::from).ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no filename".to_owned())
        })?;
        let authority = Arc::new(ProviderSourceRoot::open(parent)?);
        let outcome_lineage = prepared.authority;
        let plans = prepared.sources;
        let mut leaves = Vec::with_capacity(plans.len());
        for plan in &plans {
            let opened = authority.open_file(&authority_path)?;
            let observation = observe_opened_file(&plan.0.source_path, &opened)?;
            leaves.push(JsonlFamilyLeaf::bind_observed(
                plan.1.clone(),
                plan.0.source_path.clone(),
                Arc::clone(&authority),
                authority_path.clone(),
                TypedKey::utf8(&plan.2)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
                observation,
            ));
        }
        let rejected_leaves = prepared
            .rejections
            .into_iter()
            .map(|rejected| codex_lineage_rejected_leaf_v0(rejected, authority_path.clone()))
            .collect::<Result<Vec<_>>>()?;
        let family_inventory = JsonlFamilyInventory::present_with_rejected(
            CaptureProvider::Codex,
            route_path,
            authority,
            leaves,
            rejected_leaves,
        )?;
        let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
        state.plans = plans
            .iter()
            .cloned()
            .map(|plan| (plan.1.clone(), plan))
            .collect();
        state.outcome_lineage = Some(outcome_lineage);
        state.counters = CodexSourceBackedCountersV0::default();
        #[cfg(test)]
        if plans.is_empty() && !_completed_stage {
            state.stage_pending = true;
        }
        Ok(family_inventory)
    }

    fn run_pending_stage_observer(&self) -> bool {
        #[cfg(test)]
        {
            let counters = self.state.lock().ok().and_then(|mut state| {
                state.stage_pending.then(|| {
                    state.stage_pending = false;
                    state.counters
                })
            });
            if let (Some(observer), Some(counters)) = (self.after_stage, counters) {
                observer(counters);
            }
            counters.is_some()
        }
        #[cfg(not(test))]
        false
    }
}

impl JsonlFamilyAdapter for CodexExplicitSessionJsonlFamilyAdapterV0 {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &'static str {
        CODEX_SESSION_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        CODEX_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        CODEX_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::AuthoritativeEmpty
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        self.discover_family(root)
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation> {
        self.run_pending_stage_observer();
        JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_discovery_error_kind(error)
    }

    fn scan_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_scan_error_kind(error)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "Codex JSONL leaves require the native optimized executor",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        base: Option<&CertifiedSource>,
        base_event_lookup: &BaseEventIdentityLookup,
        worker: &mut JsonlFamilyWorkerContext,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        #[cfg(test)]
        if let Some(generation) = self.generation.as_ref() {
            generation.record_worker_start();
        }
        scan_codex_session_jsonl_leaf_v0(
            self,
            &self.state,
            leaf,
            base,
            base_event_lookup,
            worker,
            emit_page,
        )
        .map(Some)
    }

    fn finish_leaf_scans(&self) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| codex_family_state_error())?
            .outcome_lineage = None;
        Ok(())
    }

    fn base_source_path(&self, _certificate: &CertifiedSource) -> Result<PathBuf> {
        Ok(self.input.path().to_path_buf())
    }
}

fn codex_family_capture_error(error: CodexSourceBackedErrorV0) -> CaptureError {
    match error {
        CodexSourceBackedErrorV0::Capture(error) => error,
        CodexSourceBackedErrorV0::Io(error) => CaptureError::Io(error),
        CodexSourceBackedErrorV0::Json(error) => CaptureError::Json(error),
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn codex_discovery_error_kind(error: &CaptureError) -> SourceBackedRouteErrorKind {
    if let Some(kind) = codex_systemic_error_kind(error) {
        return kind;
    }
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

fn codex_scan_error_kind(error: &CaptureError) -> SourceBackedRouteErrorKind {
    if let Some(kind) = codex_systemic_error_kind(error) {
        return kind;
    }
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

fn codex_systemic_error_kind(error: &CaptureError) -> Option<SourceBackedRouteErrorKind> {
    match error {
        CaptureError::InvalidPayload(detail) if detail == CODEX_LINEAGE_EXHAUSTED_DETAIL => {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::InvalidPayload(detail) if detail == CODEX_LINEAGE_UNAVAILABLE_DETAIL => {
            Some(SourceBackedRouteErrorKind::Internal)
        }
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        CaptureError::Io(_) | CaptureError::SystemIo { .. } => {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::SystemInvariant(_) | CaptureError::WorkerPanicked(_) => {
            Some(SourceBackedRouteErrorKind::Internal)
        }
        _ => None,
    }
}

pub(crate) fn codex_session_root_rank(root: &Path) -> u8 {
    match root.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("sessions") => 0,
        Some("archived_sessions") => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_session(root: &Path, native_session_id: &str) {
        write_session_with_parent(root, native_session_id, None);
    }

    fn write_session_with_parent(
        root: &Path,
        native_session_id: &str,
        parent_native_session_id: Option<&str>,
    ) {
        let record = serde_json::json!({
            "timestamp": "2026-08-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": native_session_id,
                "timestamp": "2026-08-03T12:00:00Z",
                "cwd": "/tmp/jsonl-family-adapter",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "forked_from_id": parent_native_session_id,
                "model_provider": "openai"
            }
        });
        fs::write(
            root.join(format!("rollout-{native_session_id}.jsonl")),
            format!("{record}\n"),
        )
        .unwrap();
    }

    #[test]
    fn adapter_preserves_sessions_and_archived_union_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let archived = temp.path().join("archived_sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&archived).unwrap();
        write_session(&sessions, "019facf0-4000-7777-8888-000000000001");
        write_session(&archived, "019facf0-4000-7777-8888-000000000002");

        let adapter = CodexSessionTreeJsonlFamilyAdapterV0::new(vec![
            archived.clone(),
            sessions.clone(),
            archived.clone(),
        ])
        .unwrap();
        assert_eq!(adapter.roots(), &[sessions, archived]);

        let inventory = adapter.discover().unwrap();
        assert_eq!(inventory.sources.len(), 2);
        assert_eq!(inventory.work.inventory_walks, 2);
        assert_eq!(inventory.work.source_observations, 2);
        assert_eq!(inventory.work.source_body_reads, 2);
        assert_eq!(inventory.work.session_meta_parses, 2);
    }

    #[test]
    fn adapter_rejects_an_empty_multi_root_authority() {
        let error = CodexSessionTreeJsonlFamilyAdapterV0::new(Vec::new())
            .err()
            .expect("empty roots must be rejected");
        assert!(error
            .to_string()
            .contains("Codex session-tree authority has no roots"));
    }

    #[test]
    fn adapter_captures_new_files_only_when_family_discovery_executes() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let adapter = CodexSessionTreeJsonlFamilyAdapterV0::new(vec![sessions.clone()]).unwrap();

        write_session(&sessions, "019facf0-4000-7777-8888-000000000003");

        let inventory = JsonlFamilyAdapter::discover(&adapter, &sessions).unwrap();
        assert_eq!(inventory.leaves().len(), 1);
        assert!(adapter.state.lock().unwrap().outcome_lineage.is_some());
        JsonlFamilyAdapter::finish_leaf_scans(&adapter).unwrap();
        assert!(adapter.state.lock().unwrap().outcome_lineage.is_none());
    }

    #[test]
    fn dependency_tree_uses_parallel_depth_phases_instead_of_a_global_worker_cap() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let root = "019facf0-4000-7777-8888-000000000010";
        let first_child = "019facf0-4000-7777-8888-000000000011";
        let second_child = "019facf0-4000-7777-8888-000000000012";
        let grandchild = "019facf0-4000-7777-8888-000000000013";
        write_session_with_parent(&sessions, root, None);
        write_session_with_parent(&sessions, first_child, Some(root));
        write_session_with_parent(&sessions, second_child, Some(root));
        write_session_with_parent(&sessions, grandchild, Some(first_child));

        let adapter = CodexSessionTreeJsonlFamilyAdapterV0::new(vec![sessions.clone()]).unwrap();
        let opening = JsonlFamilyAdapter::discover(&adapter, &sessions).unwrap();
        let mut leaves = opening.leaves().to_vec();
        JsonlFamilyAdapter::order_leaf_scans(&adapter, &mut leaves).unwrap();
        assert_eq!(
            JsonlFamilyAdapter::prepare_leaf_scans(&adapter, &leaves, &HashMap::new()).unwrap(),
            None,
            "one dependency must not serialize the whole JSONL family"
        );
        let phases = leaves
            .iter()
            .map(|leaf| JsonlFamilyAdapter::leaf_scan_phase(&adapter, leaf).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(phases, vec![0, 1, 1, 2]);
        let partitions = leaves
            .iter()
            .map(|leaf| {
                JsonlFamilyAdapter::leaf_scan_partition(&adapter, leaf)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(partitions
            .iter()
            .all(|partition| *partition == partitions[0]));
    }

    #[test]
    fn lineage_resource_failures_remain_route_systemic() {
        let exhausted =
            codex_family_capture_error(CodexSourceBackedErrorV0::LineageWorkingSetExhausted);
        assert_eq!(
            codex_scan_error_kind(&exhausted),
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
        let unavailable =
            codex_family_capture_error(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
        assert_eq!(
            codex_scan_error_kind(&unavailable),
            SourceBackedRouteErrorKind::Internal
        );
        assert_eq!(
            codex_scan_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(24))),
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
    }
}

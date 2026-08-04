use std::{collections::BTreeMap, sync::Mutex};

use chrono::{DateTime, Utc};

use super::*;
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
            JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
            JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjector, JsonlFamilyPublication,
            JsonlFamilyRootMissingMode, JsonlFamilyWorkerContext,
        },
        SourceBackedRouteErrorKind,
    },
    Result,
};

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

#[derive(Default)]
struct CodexSessionJsonlFamilyStateV0 {
    opening_inventory: Option<CodexSessionTreeInventoryV0>,
    plans: HashMap<SourceKey, CodexSessionPlanV0>,
    outcome_lineage: Option<Arc<CodexOutcomeLineageAuthorityV0>>,
    terminal_evidence: HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    counters: CodexSourceBackedCountersV0,
    stage_pending: bool,
}

fn scan_codex_session_jsonl_leaf_v0(
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
    let source_key = plan.1.clone();
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
    let family_outcome = match outcome.append {
        Some(append) => JsonlFamilyOptimizedLeafOutcome::append(append),
        None => JsonlFamilyOptimizedLeafOutcome::replacement(outcome.certificate),
    };
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    state.terminal_evidence.insert(source_key, outcome.evidence);
    state.counters.add_assign(outcome.counters);
    state.stage_pending = true;
    Ok(family_outcome)
}

fn revalidate_codex_session_jsonl_leaf_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaf: &JsonlFamilyLeaf,
    certificate: &CertifiedSource,
) -> Result<bool> {
    let state = state.lock().map_err(|_| codex_family_state_error())?;
    let Some(evidence) = state.terminal_evidence.get(leaf.source()) else {
        return Ok(false);
    };
    if !matches!(
        source_observation(leaf.source(), &evidence.observation),
        Ok(observation) if observation == *certificate.observation()
    ) {
        return Ok(false);
    }
    evidence
        .revalidate_fallible()
        .map_err(codex_family_capture_error)
}

fn codex_family_state_error() -> CaptureError {
    CaptureError::InvalidPayload("Codex JSONL family state lock was poisoned".to_owned())
}

fn prepare_codex_session_jsonl_scans_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaves: &[JsonlFamilyLeaf],
    bases: &HashMap<[u8; 32], &CertifiedSource>,
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
        if let Some(proof) =
            proof.filter(|proof| proof.checkpoint.observation == source.catalog_observation)
        {
            replay_sources.push((source.clone(), proof, native_session_id.clone()));
        } else {
            changed_ids.insert(native_session_id.clone());
        }
    }
    if changed_ids.is_empty() {
        return Ok(None);
    }
    prepare_replayed_lineage_v0(&replay_sources, &outcome_lineage)
        .map_err(codex_family_capture_error)?;
    let has_changed_dependency = plans.values().any(|(source, _, native_session_id)| {
        changed_ids.contains(native_session_id)
            && source
                .catalog_parent_native_session_id
                .as_ref()
                .is_some_and(|parent| changed_ids.contains(parent))
    });
    Ok(has_changed_dependency.then_some(1))
}

/// Codex's multi-root session inventory and native optimized JSONL leaf
/// executor. The shared family owns the generation lifecycle and bounded
/// per-source scheduler; this adapter retains the native prefilter, parser,
/// checkpoints, identities, projection, and commit-time prefix evidence.
#[derive(Clone)]
pub(crate) struct CodexSessionTreeJsonlFamilyAdapterV0 {
    roots: Arc<[PathBuf]>,
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
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
        let opening_inventory = discover_codex_session_tree_inventory_v0(&roots)?;
        let state = CodexSessionJsonlFamilyStateV0 {
            opening_inventory: Some(opening_inventory),
            ..CodexSessionJsonlFamilyStateV0::default()
        };
        Ok(Self {
            roots: roots.into(),
            state: Arc::new(Mutex::new(state)),
            #[cfg(test)]
            after_stage: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_after_stage_observer(
        mut self,
        observer: fn(CodexSourceBackedCountersV0),
    ) -> Self {
        self.after_stage = Some(observer);
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
        // Registration freezes the first opening inventory. This preserves the
        // route-ownership boundary when overlapping automatic and explicit
        // routes are registered, and defers appends or new leaves observed
        // after registration to the next refresh. Later discoveries are live,
        // including the terminal revalidation discovery for this refresh.
        let opening_inventory = self
            .state
            .lock()
            .map_err(|_| codex_family_state_error())?
            .opening_inventory
            .take();
        let inventory = opening_inventory
            .map_or_else(
                || self.discover(),
                Ok::<CodexSessionTreeInventoryV0, CodexSourceBackedErrorV0>,
            )
            .map_err(codex_family_capture_error)?;
        let outcome_lineage = Arc::new(
            CodexOutcomeLineageAuthorityV0::from_sources(&inventory.sources)
                .map_err(codex_family_capture_error)?,
        );
        let mut ordered_sources = inventory.sources.iter().collect::<Vec<_>>();
        ordered_sources
            .sort_by_key(|(_, _, native_session_id)| outcome_lineage.depth(native_session_id));
        let mut authorities = BTreeMap::<PathBuf, Arc<ProviderSourceRoot>>::new();
        let mut leaves = Vec::with_capacity(inventory.sources.len());
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
        let family_inventory = JsonlFamilyInventory::present_multi(
            CaptureProvider::Codex,
            route_root,
            authorities.into_values().collect(),
            leaves,
        )?;
        let mut state = self.state.lock().map_err(|_| {
            CaptureError::InvalidPayload("Codex JSONL family state lock was poisoned".to_owned())
        })?;
        state.plans = inventory
            .sources
            .iter()
            .cloned()
            .map(|plan| (plan.1.clone(), plan))
            .collect();
        state.outcome_lineage = Some(outcome_lineage);
        let current_sources = state.plans.keys().cloned().collect::<HashSet<_>>();
        state
            .terminal_evidence
            .retain(|source, _| current_sources.contains(source));
        state.counters = CodexSourceBackedCountersV0::default();
        #[cfg(test)]
        {
            state.counters.add_catalog_work(inventory.work);
            if inventory.sources.is_empty() && !_completed_stage {
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

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_discovery_error_kind(error)
    }

    fn scan_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        codex_scan_error_kind(error)
    }

    fn prepare_leaf_scans(
        &self,
        leaves: &[JsonlFamilyLeaf],
        bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> Result<Option<usize>> {
        prepare_codex_session_jsonl_scans_v0(&self.state, leaves, bases)
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
        scan_codex_session_jsonl_leaf_v0(
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

    fn revalidate_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        _checkpoint: Option<&crate::provider::source_backed::family::jsonl::JsonlCheckpoint>,
    ) -> Result<bool> {
        revalidate_codex_session_jsonl_leaf_v0(&self.state, leaf, certificate)
    }
}

/// One explicitly selected Codex rollout using the shared JSONL-family
/// lifecycle and the same native leaf executor as automatic discovery.
#[derive(Clone)]
pub(crate) struct CodexExplicitSessionJsonlFamilyAdapterV0 {
    input: CodexExplicitSessionSourceBackedInputV0,
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    #[cfg(test)]
    after_stage: Option<fn(CodexSourceBackedCountersV0)>,
}

impl CodexExplicitSessionJsonlFamilyAdapterV0 {
    pub(crate) fn new(input: CodexExplicitSessionSourceBackedInputV0) -> Self {
        Self {
            input,
            state: Arc::new(Mutex::new(CodexSessionJsonlFamilyStateV0::default())),
            #[cfg(test)]
            after_stage: None,
        }
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
        let inventory = observe_codex_explicit_session_source_backed_v0(&self.input)
            .map_err(codex_family_capture_error)?;
        let Some(plan) = inventory.source_plan() else {
            let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
            state.plans.clear();
            state.outcome_lineage = None;
            state.terminal_evidence.clear();
            state.counters = CodexSourceBackedCountersV0::default();
            #[cfg(test)]
            if !_completed_stage {
                state.stage_pending = true;
            }
            return JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path);
        };
        let parent = plan.0.source_path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no parent".to_owned())
        })?;
        let authority_path = plan
            .0
            .source_path
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| {
                CaptureError::InvalidPayload("explicit Codex JSONL path has no filename".to_owned())
            })?;
        let authority = Arc::new(ProviderSourceRoot::open(parent)?);
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&plan.0.source_path, &opened)?;
        let leaf = JsonlFamilyLeaf::bind_observed(
            plan.1.clone(),
            plan.0.source_path.clone(),
            Arc::clone(&authority),
            authority_path,
            TypedKey::utf8(&plan.2)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            observation,
        );
        let family_inventory = JsonlFamilyInventory::present(
            CaptureProvider::Codex,
            route_path,
            authority,
            vec![leaf],
        )?;
        let plans = vec![plan];
        let mut state = self.state.lock().map_err(|_| codex_family_state_error())?;
        state.plans = plans
            .iter()
            .cloned()
            .map(|plan| (plan.1.clone(), plan))
            .collect();
        state.outcome_lineage = Some(Arc::new(
            CodexOutcomeLineageAuthorityV0::from_sources(&plans)
                .map_err(codex_family_capture_error)?,
        ));
        let current_sources = state.plans.keys().cloned().collect::<HashSet<_>>();
        state
            .terminal_evidence
            .retain(|source, _| current_sources.contains(source));
        state.counters = CodexSourceBackedCountersV0::default();
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
        scan_codex_session_jsonl_leaf_v0(
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
        Ok(self.input.path().to_path_buf())
    }

    fn revalidate_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        _checkpoint: Option<&crate::provider::source_backed::family::jsonl::JsonlCheckpoint>,
    ) -> Result<bool> {
        revalidate_codex_session_jsonl_leaf_v0(&self.state, leaf, certificate)
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
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

fn codex_scan_error_kind(error: &CaptureError) -> SourceBackedRouteErrorKind {
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
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
}

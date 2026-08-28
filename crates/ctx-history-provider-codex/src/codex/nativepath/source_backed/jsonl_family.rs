use std::{
    collections::{BTreeMap, VecDeque},
    marker::PhantomData,
    sync::Mutex,
};

use super::*;
use crate::codex::nativepath::reader::MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES;
use crate::provider::source_backed::{ProviderBaseEventLookup, ProviderRuntimeBinding};
use crate::{
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, observe_opened_file_allow_append, probe_records_until,
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyExecutionIo,
            JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
            JsonlFamilyMembershipObservation, JsonlFamilyOpenedMember, JsonlFamilyProjectionMode,
            JsonlFamilyRejectedLeaf, JsonlFamilyRootMissingMode, JsonlFamilySemanticExecutor,
            JsonlFamilySemanticPage, JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary,
            JsonlFamilyWorkerContext, JsonlFileObservation, JsonlRecordFraming,
        },
        SourceBackedRouteErrorKind,
    },
    Result,
};
use ctx_history_jsonl::{JsonlPhysicalEncoding, MAX_STANDARD_ZSTD_PARALLEL_STREAMS};
use sha2::{Digest, Sha256};

fn observe_generation_source_capability_v0(
    source: &CodexCatalogSource,
) -> Result<JsonlFileObservation> {
    let opened = reopen_codex_source_capability(source)?;
    revalidate_codex_catalog_source_capability(source, &opened)?;
    let admitted = observe_opened_file(&source.source_path, &opened)?;
    let current = observe_opened_file_allow_append(&source.source_path, &opened)?;
    if !admitted.admits_frozen_prefix_in(&current) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(admitted)
}

fn carried_or_observe_generation_source_capability_v0(
    source: &CodexCatalogSource,
) -> Result<JsonlFileObservation> {
    match &source.carried_jsonl_observation {
        Some(observation) => Ok(observation.clone()),
        None => observe_generation_source_capability_v0(source),
    }
}

#[derive(serde::Serialize)]
struct CodexRejectedOwnerProofV0<'a> {
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_native_session_id: Option<&'a str>,
    observation: &'a CodexFileObservation,
    reason: &'static str,
}

fn generation_source_owner_is_admissible_v0(
    source: &CodexCatalogSource,
    expected_native_session_id: &str,
) -> Result<bool> {
    // A zero-byte rollout has no records to attribute. Preserve the existing
    // ignored-file behavior for partial writes while quarantining any
    // nonempty source whose bounded ownership prefix is absent or conflicting.
    //
    // This validation must precede streaming: NativePath may otherwise stage
    // pages before a later session_meta proves ownership ambiguous.
    if source.catalog_observation.len == 0 {
        return Ok(true);
    }
    let opened = reopen_codex_source_capability(source)?;
    if probe_records_until(&source.source_path, &opened, 1, |_| {
        Ok::<Option<()>, CaptureError>(Some(()))
    })?
    .is_none()
    {
        // A nonempty file without one complete first physical record is an
        // in-progress JSONL write. Shared JSONL will classify it as pending;
        // it has no complete provider record on which to make an ownership
        // decision yet.
        return Ok(true);
    }
    let observed = match crate::provider::codex::catalog::probe_codex_native_session_id(
        &source.source_path,
        &opened,
        source.catalog_observation.len,
    ) {
        Ok(observed) => observed,
        Err(CaptureError::InvalidPayload(_)) => return Ok(false),
        Err(error) => return Err(error),
    };
    opened.revalidate_same_object()?;
    Ok(observed.as_deref() == Some(expected_native_session_id))
}

fn rejected_owner_leaf_v0(
    source: &CodexCatalogSource,
    source_key: &SourceKey,
    authority_path: PathBuf,
    observation: JsonlFileObservation,
    expected_native_session_id: &str,
) -> Result<JsonlFamilyRejectedLeaf> {
    Ok(rejected_catalog_leaf_v0(
        &source.source_path,
        authority_path,
        &source.catalog_observation,
        observation,
        Some(expected_native_session_id),
        "missing or conflicting Codex session owner",
    )?
    .with_logical_source_failure(
        codex_quarantined_rollout_source_key(
            source.source_root_lineage,
            &source.catalog_observation,
        )?,
        format!(
            "Codex session ownership is ambiguous or conflicting; quarantined rollout file {}",
            source.source_path.display()
        ),
    )
    .with_quarantined_source(source_key.clone()))
}

fn rejected_catalog_leaf_v0(
    source_path: &Path,
    authority_path: PathBuf,
    observation: &CodexFileObservation,
    jsonl_observation: JsonlFileObservation,
    expected_native_session_id: Option<&str>,
    reason: &'static str,
) -> Result<JsonlFamilyRejectedLeaf> {
    let proof = CodexRejectedOwnerProofV0 {
        schema_version: 1,
        expected_native_session_id,
        observation,
        reason,
    };
    let proof = TypedKey::bytes(serde_json::to_vec(&proof)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(JsonlFamilyRejectedLeaf::bind_observed(
        source_path.to_path_buf(),
        authority_path,
        jsonl_observation,
        proof,
        1,
    ))
}

fn codex_quarantined_rollout_source_key(
    source_root_lineage: Option<[u8; 32]>,
    observation: &CodexFileObservation,
) -> Result<SourceKey> {
    // This is explicitly a file-observation key, not a claimed Codex session
    // identity. It cannot connect a quarantined file to another rollout.
    let observation = serde_json::to_vec(observation)?;
    let digest = Sha256::digest(observation);
    let scope =
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage);
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Codex.as_str(),
        CODEX_SESSION_SOURCE_FORMAT,
        CODEX_SOURCE_SCHEMA_VARIANT,
        1,
        "codex.quarantined-rollout-file.v1",
        TypedKey::bytes(digest.to_vec())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        scope,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

#[derive(Default)]
struct CodexSessionJsonlFamilyStateV0 {
    plans: HashMap<SourceKey, CodexSessionPlanV0>,
}

struct CodexSessionSemanticExecutorV0<B: ProviderRuntimeBinding> {
    binding: PhantomData<fn() -> B>,
    scanner: Option<CodexNativeScanner>,
    pending_pages: VecDeque<JsonlFamilySemanticPage>,
    checkpoint: Option<super::super::checkpoint::CodexSemanticCheckpoint>,
    append_checkpoint_required: bool,
}

impl<B: ProviderRuntimeBinding> CodexSessionSemanticExecutorV0<B> {
    fn new(
        state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        projection_mode: JsonlFamilyProjectionMode,
    ) -> Result<Self> {
        let plan = {
            let state = state.lock().map_err(|_| codex_family_state_error())?;
            state.plans.get(leaf.source()).cloned().ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex JSONL family leaf has no native source plan".to_owned(),
                )
            })?
        };
        if plan.0.source_path != leaf.source_path() {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let append_checkpoint_required =
            projection_mode == JsonlFamilyProjectionMode::CertifiedAppend;
        let checkpoint = match (projection_mode, checkpoint) {
            (JsonlFamilyProjectionMode::CertifiedAppend, Some(checkpoint)) => {
                super::super::checkpoint::CodexSemanticCheckpoint::decode_key(checkpoint).ok()
            }
            (JsonlFamilyProjectionMode::CertifiedAppend, None)
            | (JsonlFamilyProjectionMode::Cold | JsonlFamilyProjectionMode::Replacement, _) => None,
        };
        let base_event_lookup = match projection_mode {
            JsonlFamilyProjectionMode::CertifiedAppend => Some(base_event_lookup.ok_or(
                CaptureError::SystemInvariant("Codex semantic append has no base event lookup"),
            )?),
            JsonlFamilyProjectionMode::Cold | JsonlFamilyProjectionMode::Replacement => None,
        };
        let scanner = CodexNativeScanner::new_semantic(plan.0, base_event_lookup)?;
        Ok(Self {
            binding: PhantomData,
            scanner: Some(scanner),
            pending_pages: VecDeque::new(),
            checkpoint,
            append_checkpoint_required,
        })
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilySemanticExecutor for CodexSessionSemanticExecutorV0<B> {
    type Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>;

    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<B>,
    ) -> Result<JsonlFamilySemanticPreflight> {
        if self.append_checkpoint_required {
            let Some(checkpoint) = self.checkpoint.as_ref() else {
                return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
            };
            if self
                .scanner
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex semantic executor lost its scanner",
                ))?
                .restore_semantic_checkpoint(checkpoint)
                .is_err()
            {
                return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
            }
        }
        let retry = self
            .scanner
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Codex semantic executor lost its scanner",
            ))?
            .preflight_semantic(input)?;
        Ok(if retry {
            JsonlFamilySemanticPreflight::RetryReplacement
        } else {
            JsonlFamilySemanticPreflight::Ready
        })
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<B>,
        _worker: &mut JsonlFamilyWorkerContext<B>,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        if let Some(page) = self.pending_pages.pop_front() {
            return Ok(Some(page));
        }
        let Some(page) = self
            .scanner
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Codex semantic executor lost its scanner",
            ))?
            .next_semantic_page(input)?
        else {
            return Ok(None);
        };
        self.pending_pages.extend(
            JsonlFamilySemanticPage::split_bounded_with_singleton_limit::<CaptureError>(
                page.records,
                MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES,
            )?,
        );
        Ok(self.pending_pages.pop_front())
    }

    fn finish(mut self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        if !self.pending_pages.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "Codex semantic executor finished with pending bounded pages",
            ));
        }
        let scanner = self.scanner.take().ok_or(CaptureError::SystemInvariant(
            "Codex semantic executor lost its scanner",
        ))?;
        let ownership_quarantine = scanner
            .ownership_quarantined_source()
            .map(|source| {
                Ok::<(SourceKey, String), CaptureError>((
        codex_quarantined_rollout_source_key(
            source.source_root_lineage,
            &source.catalog_observation,
        )?,
                    format!(
                        "Codex session ownership is ambiguous or conflicting; quarantined rollout file {}",
                        source.source_path.display()
                    ),
                ))
            })
            .transpose()?;
        let scan = scanner.finish_semantic()?;
        let provider_checkpoint = scan
            .checkpoint
            .map(|checkpoint| checkpoint.encode_key().map_err(CaptureError::from))
            .transpose()?;
        let summary = JsonlFamilySemanticSummary::new(
            scan.counters.retained_records,
            scan.counters.rejected_complete_records,
            provider_checkpoint,
        )
        .with_record_rejections(scan.record_rejections);
        Ok(match ownership_quarantine {
            Some((source, detail)) => summary.with_logical_source_quarantine(source, detail),
            None => summary,
        })
    }
}

fn codex_family_state_error() -> CaptureError {
    CaptureError::InvalidPayload("Codex JSONL family state lock was poisoned".to_owned())
}

fn prepare_codex_session_jsonl_scans_v0<B: ProviderRuntimeBinding>(
    _adapter: &dyn JsonlFamilyAdapter<
        Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>,
    >,
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    leaves: &[JsonlFamilyLeaf],
    _bases: &HashMap<[u8; 32], &CertifiedSource>,
) -> Result<Option<usize>> {
    let state = state.lock().map_err(|_| codex_family_state_error())?;
    for leaf in leaves {
        if !state.plans.contains_key(leaf.source()) {
            return Err(CaptureError::InvalidPayload(
                "Codex JSONL family selected an unplanned leaf".to_owned(),
            ));
        }
    }
    Ok(leaves
        .iter()
        .any(|leaf| {
            crate::provider::codex::catalog::is_codex_compressed_session_rollout_path(
                leaf.source_path(),
            )
        })
        .then_some(MAX_STANDARD_ZSTD_PARALLEL_STREAMS))
}

fn install_prepared_state_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    plans: &[CodexSessionPlanV0],
    _completed_stage: bool,
) -> Result<()> {
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    state.plans = plans
        .iter()
        .cloned()
        .map(|plan| (plan.1.clone(), plan))
        .collect();
    Ok(())
}

fn missing_codex_inventory_v0(
    state: &Mutex<CodexSessionJsonlFamilyStateV0>,
    route_path: &Path,
    _completed_stage: bool,
) -> Result<JsonlFamilyInventory> {
    let mut state = state.lock().map_err(|_| codex_family_state_error())?;
    state.plans.clear();
    JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path)
}

/// Codex session inventory and semantic JSONL execution for either explicit or
/// tree discovery. Shared JSONL owns carried bases and the physical lifecycle;
/// its mandatory generation route owns discovery and terminal membership.
#[derive(Clone)]
pub struct CodexSessionJsonlFamilyAdapterV0<B: ProviderRuntimeBinding> {
    state: Arc<Mutex<CodexSessionJsonlFamilyStateV0>>,
    generation: CodexGenerationRouteV0,
    binding: PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> CodexSessionJsonlFamilyAdapterV0<B> {
    pub fn new(generation: CodexGenerationRouteV0) -> Self {
        Self {
            state: Arc::new(Mutex::new(CodexSessionJsonlFamilyStateV0::default())),
            generation,
            binding: PhantomData,
        }
    }

    fn discover_tree_family(
        &self,
        route_root: &Path,
        roots: &[PathBuf],
    ) -> Result<JsonlFamilyInventory> {
        let completed_stage = self.run_pending_stage_observer()?;
        // Shared JSONL invokes this only after route admission and freezes the
        // opening inventory before leaf workers start.
        let prepared = self.generation.prepared()?;
        if prepared.missing {
            return Err(CaptureError::SystemInvariant(
                "Codex session-tree generation partition is missing",
            ));
        }
        let plans = prepared.sources;
        let mut rejected_leaves = prepared
            .rejected_leaves
            .iter()
            .map(|leaf| {
                rejected_catalog_leaf_v0(
                    &leaf.source_path,
                    leaf.authority_path.clone(),
                    &leaf.observation,
                    leaf.jsonl_observation.clone(),
                    None,
                    leaf.reason,
                )
                .and_then(|rejected| {
                    Ok(rejected.with_logical_source_failure(
                        codex_quarantined_rollout_source_key(
                            leaf.source_root_lineage,
                            &leaf.observation,
                        )?,
                        format!(
                            "Codex session ownership is ambiguous or conflicting; quarantined rollout file {}",
                            leaf.source_path.display()
                        ),
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut ordered_sources = (0..plans.len()).collect::<Vec<_>>();
        ordered_sources.sort_by_key(|index| plans[*index].1.identity().digest());
        let mut authorities = BTreeMap::<PathBuf, Arc<ProviderSourceRoot>>::new();
        let mut leaves = Vec::with_capacity(plans.len());
        for index in ordered_sources {
            let (source, source_key, native_session_id) = plans.get(index).ok_or(
                CaptureError::SystemInvariant("Codex generation source ordering changed"),
            )?;
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
            let observation = carried_or_observe_generation_source_capability_v0(source)?;
            let owner_is_admissible =
                crate::provider::codex::catalog::is_codex_compressed_session_rollout_path(
                    &source.source_path,
                ) || generation_source_owner_is_admissible_v0(source, native_session_id)?;
            if owner_is_admissible {
                leaves.push(JsonlFamilyLeaf::bind_frozen_observed(
                    source_key.clone(),
                    source.source_path.clone(),
                    Arc::clone(&authority),
                    authority_path,
                    TypedKey::utf8(native_session_id)
                        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
                    observation,
                ));
            } else {
                rejected_leaves.push(rejected_owner_leaf_v0(
                    source,
                    source_key,
                    authority_path,
                    observation,
                    native_session_id,
                )?);
            }
            authorities
                .entry(authority.named_path().to_path_buf())
                .or_insert(authority);
        }
        for root in roots {
            if !authorities.contains_key(root) {
                let authority = Arc::new(ProviderSourceRoot::open(root)?);
                authorities.insert(authority.named_path().to_path_buf(), authority);
            }
        }
        let inventory = JsonlFamilyInventory::present_multi_with_rejected(
            CaptureProvider::Codex,
            route_root,
            authorities.into_values().collect(),
            leaves,
            rejected_leaves,
        )?;
        install_prepared_state_v0(&self.state, &plans, completed_stage)?;
        Ok(inventory)
    }

    fn discover_explicit_family(
        &self,
        route_path: &Path,
        input: &CodexExplicitSessionSourceBackedInputV0,
    ) -> Result<JsonlFamilyInventory> {
        let completed_stage = self.run_pending_stage_observer()?;
        if route_path != input.path() {
            return Err(CaptureError::InvalidPayload(
                "explicit Codex JSONL route path changed".to_owned(),
            ));
        }
        let prepared = self.generation.prepared()?;
        if prepared.missing {
            return missing_codex_inventory_v0(&self.state, route_path, completed_stage);
        }
        let parent = route_path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no parent".to_owned())
        })?;
        let authority_path = route_path.file_name().map(PathBuf::from).ok_or_else(|| {
            CaptureError::InvalidPayload("explicit Codex JSONL path has no filename".to_owned())
        })?;
        let authority = Arc::new(ProviderSourceRoot::open(parent)?);
        let plans = prepared.sources;
        let mut leaves = Vec::with_capacity(plans.len());
        for plan in &plans {
            let observation = observe_generation_source_capability_v0(&plan.0)?;
            leaves.push(JsonlFamilyLeaf::bind_frozen_observed(
                plan.1.clone(),
                plan.0.source_path.clone(),
                Arc::clone(&authority),
                authority_path.clone(),
                TypedKey::utf8(&plan.2)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
                observation,
            ));
        }
        let inventory =
            JsonlFamilyInventory::present(CaptureProvider::Codex, route_path, authority, leaves)?;
        install_prepared_state_v0(&self.state, &plans, completed_stage)?;
        Ok(inventory)
    }

    fn run_pending_stage_observer(&self) -> Result<bool> {
        Ok(false)
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilyAdapter for CodexSessionJsonlFamilyAdapterV0<B> {
    type Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>;

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

    fn record_framing(&self) -> JsonlRecordFraming {
        JsonlRecordFraming::terminal_nul_padded(super::super::reader::MAX_CODEX_RECORD_BYTES)
    }

    fn physical_encoding(&self, leaf: &JsonlFamilyLeaf) -> JsonlPhysicalEncoding {
        if crate::provider::codex::catalog::is_codex_compressed_session_rollout_path(
            leaf.source_path(),
        ) {
            JsonlPhysicalEncoding::StandardZstdJsonl
        } else {
            JsonlPhysicalEncoding::RawJsonl
        }
    }

    fn bind_admitted_eof(&self) -> bool {
        true
    }

    fn append_only_same_object_v1(&self) -> bool {
        self.generation.is_session_tree()
    }

    fn accepts_direct_append_checkpoint(&self, checkpoint: &TypedKey) -> bool {
        super::super::checkpoint::CodexSemanticCheckpoint::decode_key(checkpoint)
            .is_ok_and(|checkpoint| checkpoint.direct_append_safe())
    }

    fn allows_direct_append_for_leaf(&self, leaf: &JsonlFamilyLeaf) -> bool {
        self.generation.is_session_tree()
            && self.physical_encoding(leaf) == JsonlPhysicalEncoding::RawJsonl
            && leaf.observation().supports_exact_revalidation()
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        if self.generation.is_session_tree() {
            JsonlFamilyRootMissingMode::Unavailable
        } else {
            JsonlFamilyRootMissingMode::AuthoritativeEmpty
        }
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        if let Some(roots) = self.generation.session_tree_roots() {
            self.discover_tree_family(root, roots)
        } else if let Some(input) = self.generation.explicit_session_input() {
            self.discover_explicit_family(root, input)
        } else {
            Err(CaptureError::SystemInvariant(
                "Codex generation route has no discovery authority",
            ))
        }
    }

    fn partial_member_roots(&self, _root: &Path) -> Option<Vec<PathBuf>> {
        self.generation.session_tree_roots().map(<[_]>::to_vec)
    }

    fn bind_partial_member(
        &self,
        member: &JsonlFamilyOpenedMember<'_>,
    ) -> Result<Option<JsonlFamilyLeaf>> {
        if !self.generation.is_session_tree() {
            return Ok(None);
        }
        let plan = match super::catalog::bind_codex_partial_member_v0(
            member,
            self.generation.source_root_lineage(),
        ) {
            Ok(plan) => plan,
            Err(_) => return Ok(None),
        };
        let leaf = JsonlFamilyLeaf::bind_frozen_observed(
            plan.1.clone(),
            member.source_path().to_path_buf(),
            Arc::clone(member.authority()),
            member.authority_path().to_path_buf(),
            TypedKey::utf8(&plan.2)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            member.observation().clone(),
        );
        self.state
            .lock()
            .map_err(|_| codex_family_state_error())?
            .plans
            .insert(plan.1.clone(), plan);
        Ok(Some(leaf))
    }

    fn prepare_partial_member_fallback(&self) -> Result<()> {
        self.generation.prepare_selected().map_err(Into::into)
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation> {
        self.run_pending_stage_observer()?;
        if self.generation.is_session_tree() {
            let mut observation = JsonlFamilyMembershipObservation::observe_authorities(opening)?;
            let candidates = observation
                .unbound_routes()
                .map(|(path, authority, authority_path)| {
                    (path.to_path_buf(), authority, authority_path.to_path_buf())
                })
                .collect::<Vec<_>>();
            for (path, authority, authority_path) in candidates {
                if let Some(native_session_id) =
                    super::catalog::codex_terminal_native_session_id_hint(
                        &path,
                        &authority,
                        &authority_path,
                    )?
                {
                    let source_root_lineage = self.generation.source_root_lineage();
                    observation.bind_source_hint(
                        path,
                        codex_source_key_in_root(source_root_lineage, &native_session_id)?,
                    );
                }
            }
            Ok(observation)
        } else {
            JsonlFamilyMembershipObservation::observe(root, opening)
        }
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        match error {
            CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SourceBackedRouteErrorKind::Unavailable
            }
            CaptureError::SystemIo { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                SourceBackedRouteErrorKind::ResourceUnavailable
            }
            _ => SourceBackedRouteErrorKind::InvalidSource,
        }
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        leaves.sort_by(|left, right| {
            left.source()
                .identity()
                .digest()
                .cmp(&right.source().identity().digest())
        });
        Ok(())
    }

    fn prepare_leaf_scans(
        &self,
        leaves: &[JsonlFamilyLeaf],
        bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> Result<Option<usize>> {
        prepare_codex_session_jsonl_scans_v0(self, &self.state, leaves, bases)
    }

    fn semantic_executor(
        &self,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<
        Option<
            Box<
                dyn JsonlFamilySemanticExecutor<
                    Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>,
                >,
            >,
        >,
    > {
        Ok(Some(Box::new(CodexSessionSemanticExecutorV0::new(
            Arc::clone(&self.state),
            leaf,
            checkpoint,
            base_event_lookup,
            mode,
        )?)))
    }
}

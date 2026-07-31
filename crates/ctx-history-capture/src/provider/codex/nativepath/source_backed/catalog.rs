use super::*;

const CODEX_SESSION_TREE_UNION_INVENTORY_REVISION_KIND: &str =
    "codex-session-tree-union-inventory-v0";
const CODEX_SESSION_TREE_UNION_DISCOVERY_REVISION: &str = "codex-session-tree-union-catalog-v0";

#[derive(Debug)]
pub(crate) struct CodexRootInventoryV0 {
    pub(crate) sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    pub(crate) certificate: CertifiedSourceInventory,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexSessionTreeInventoryV0 {
    pub(crate) sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    pub(crate) certificate: CertifiedSourceInventory,
    pub(crate) work: CodexCatalogWorkV0,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexCatalogWorkV0 {
    pub(crate) inventory_walks: u64,
    pub(crate) source_observations: u64,
    pub(crate) source_body_reads: u64,
    pub(crate) session_meta_parses: u64,
}

impl CodexCatalogWorkV0 {
    fn add_assign(&mut self, other: Self) {
        self.inventory_walks = self.inventory_walks.saturating_add(other.inventory_walks);
        self.source_observations = self
            .source_observations
            .saturating_add(other.source_observations);
        self.source_body_reads = self
            .source_body_reads
            .saturating_add(other.source_body_reads);
        self.session_meta_parses = self
            .session_meta_parses
            .saturating_add(other.session_meta_parses);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexExplicitSessionSourceBackedInputV0 {
    path: PathBuf,
    source: SourceKey,
    native_session_id: String,
}

impl CodexExplicitSessionSourceBackedInputV0 {
    pub(crate) fn discover(path: impl AsRef<Path>) -> CodexSourceBackedResultV0<Self> {
        let path = absolute_lexical_path(path.as_ref())?;
        let (_, source, native_session_id) = open_codex_explicit_source_plan_v0(&path)?;
        Ok(Self {
            path,
            source,
            native_session_id,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }
}

// The 488-byte present plan is moved intact through bounded inventory discovery;
// boxing it would add allocation without reducing retained source authority.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum CodexExplicitSessionInventoryStateV0 {
    Present {
        plan: (CodexCatalogSource, SourceKey, String),
    },
    Missing,
}

impl PartialEq for CodexExplicitSessionInventoryStateV0 {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Present { plan: left }, Self::Present { plan: right }) => {
                let (left_source, left_key, left_native_session_id) = left;
                let (right_source, right_key, right_native_session_id) = right;
                left_key.exact_descriptor_eq(right_key)
                    && left_native_session_id == right_native_session_id
                    && left_source.source_root == right_source.source_root
                    && left_source.source_path == right_source.source_path
                    && left_source.catalog_native_session_id
                        == right_source.catalog_native_session_id
                    && left_source.catalog_parent_native_session_id
                        == right_source.catalog_parent_native_session_id
                    && left_source.catalog_root_native_session_id
                        == right_source.catalog_root_native_session_id
            }
            (Self::Missing, Self::Missing) => true,
            _ => false,
        }
    }
}

impl Eq for CodexExplicitSessionInventoryStateV0 {}

/// One finite observation of exactly one caller-selected Codex rollout.
#[derive(Debug, Clone)]
pub(crate) struct CodexExplicitSessionInventoryV0 {
    input: CodexExplicitSessionSourceBackedInputV0,
    observation: SourceInventoryObservation,
    state: CodexExplicitSessionInventoryStateV0,
}

impl CodexExplicitSessionInventoryV0 {
    pub(crate) fn is_missing(&self) -> bool {
        self.state == CodexExplicitSessionInventoryStateV0::Missing
    }

    pub(crate) fn source_plan(&self) -> Option<(CodexCatalogSource, SourceKey, String)> {
        match &self.state {
            CodexExplicitSessionInventoryStateV0::Present { plan } => Some(plan.clone()),
            CodexExplicitSessionInventoryStateV0::Missing => None,
        }
    }

    pub(crate) fn certify_against(
        &self,
        closing: &Self,
    ) -> CodexSourceBackedResultV0<CertifiedSourceInventory> {
        if self.input != closing.input || self.state != closing.state {
            return Err(CodexSourceBackedErrorV0::ExplicitInventoryChanged);
        }
        let sources = if self.is_missing() {
            Vec::new()
        } else {
            vec![self.input.source.clone()]
        };
        Ok(CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            CODEX_EXPLICIT_DISCOVERY_REVISION,
            sources,
        )?)
    }
}

pub(crate) fn observe_codex_explicit_session_source_backed_v0(
    input: &CodexExplicitSessionSourceBackedInputV0,
) -> CodexSourceBackedResultV0<CodexExplicitSessionInventoryV0> {
    let state = match open_codex_explicit_source_plan_v0(input.path()) {
        Ok(plan)
            if plan.1.exact_descriptor_eq(input.source()) && plan.2 == input.native_session_id =>
        {
            CodexExplicitSessionInventoryStateV0::Present { plan }
        }
        Ok(_) => return Err(CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged),
        Err(CodexSourceBackedErrorV0::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            CodexExplicitSessionInventoryStateV0::Missing
        }
        Err(error) => return Err(error),
    };
    let observation = codex_explicit_inventory_observation_v0(input, &state)?;
    Ok(CodexExplicitSessionInventoryV0 {
        input: input.clone(),
        observation,
        state,
    })
}

fn open_codex_explicit_source_plan_v0(
    path: &Path,
) -> CodexSourceBackedResultV0<(CodexCatalogSource, SourceKey, String)> {
    let opened = Arc::new(open_provider_source_file(path)?);
    let catalog = catalog_codex_explicit_session_opened(path, &opened)?;
    let discovery = super::discover_codex_catalog_sources(&[catalog]);
    if discovery.ineligible != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: discovery.ineligible,
        });
    }
    let mut sources = discovery.sources;
    let Some(source) = sources.first_mut() else {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        });
    };
    source.opened = Some(opened);
    let mut bound = bind_source_keys(sources)?;
    if bound.len() != 1 {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: bound.len(),
            failed: 0,
        });
    }
    bound
        .pop()
        .ok_or(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        })
}

fn codex_explicit_inventory_observation_v0(
    input: &CodexExplicitSessionSourceBackedInputV0,
    state: &CodexExplicitSessionInventoryStateV0,
) -> CodexSourceBackedResultV0<SourceInventoryObservation> {
    let path_identity = crate::provider::provider_path_identity(input.path())?;
    let authority_key: [u8; 32] = Sha256::digest(path_identity.as_bytes()).into();
    let mut revision = Sha256::new();
    revision.update(CODEX_EXPLICIT_INVENTORY_DIGEST_DOMAIN);
    revision.update(input.source.exact_descriptor_digest());
    match state {
        CodexExplicitSessionInventoryStateV0::Present { .. } => revision.update(b"present\0"),
        CodexExplicitSessionInventoryStateV0::Missing => revision.update(b"missing\0"),
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::Codex.as_str(),
        CODEX_EXPLICIT_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(authority_key.to_vec())?,
        CODEX_EXPLICIT_INVENTORY_REVISION_KIND,
        revision.finalize().to_vec(),
    )?)
}

fn absolute_lexical_path(path: &Path) -> CodexSourceBackedResultV0<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

pub(super) fn bind_catalog_capabilities(
    mut sources: Vec<CodexCatalogSource>,
    root: &ProviderSourceRoot,
    session_root: &Path,
) -> CodexSourceBackedResultV0<Vec<CodexCatalogSource>> {
    for source in &mut sources {
        let relative_path = source.source_path.strip_prefix(session_root).map_err(|_| {
            CodexSourceBackedErrorV0::Capture(CaptureError::SystemInvariant(
                "Codex catalog source escaped its retained root authority",
            ))
        })?;
        source.authority_root = Some(root.clone());
        source.authority_relative_path = Some(relative_path.to_path_buf());
    }
    Ok(sources)
}

pub(super) fn bind_source_keys(
    sources: Vec<CodexCatalogSource>,
) -> CodexSourceBackedResultV0<Vec<(CodexCatalogSource, SourceKey, String)>> {
    let mut native_ids = HashSet::new();
    let mut bound = Vec::with_capacity(sources.len());
    for source in sources {
        let native_session_id = source.catalog_native_session_id.clone().ok_or_else(|| {
            CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: source.source_path.clone(),
            }
        })?;
        if !native_ids.insert(native_session_id.clone()) {
            return Err(CodexSourceBackedErrorV0::DuplicateNativeSessionId(
                native_session_id,
            ));
        }
        let source_key = codex_source_key(&native_session_id)?;
        bound.push((source, source_key, native_session_id));
    }
    Ok(bound)
}

pub(crate) fn discover_codex_session_tree_inventory_from_base_v0(
    session_roots: &[PathBuf],
    base_sources: &HashMap<SourceKey, CertifiedSource>,
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    let seeds = base_sources
        .values()
        .filter_map(incremental_seed_from_certificate)
        .collect::<CodexSourceBackedResultV0<Vec<_>>>()?;
    discover_codex_session_tree_inventory_incremental_v0(session_roots, &seeds)
}

pub(crate) fn discover_codex_root_inventory_v0(
    session_root: &Path,
) -> CodexSourceBackedResultV0<CodexRootInventoryV0> {
    let retained = discover_codex_session_catalog_retained(session_root)?;
    build_codex_root_inventory_v0(session_root, retained)
}

pub(crate) fn discover_codex_session_tree_inventory_v0(
    session_roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    discover_codex_session_tree_inventory_incremental_v0(session_roots, &[])
}

fn build_codex_root_inventory_v0(
    session_root: &Path,
    retained: crate::provider::codex::catalog::RetainedCodexSessionCatalog,
) -> CodexSourceBackedResultV0<CodexRootInventoryV0> {
    let root_revision = codex_root_revision_v0(session_root)?;
    let discovery = super::discover_codex_catalog_sources(&retained.sessions);
    if retained.summary.failed_sessions != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: retained.summary.failed_sessions,
        });
    }
    let sources = bind_source_keys(bind_catalog_capabilities(
        discovery.sources,
        &retained.root,
        session_root,
    )?)?;
    retained.root.revalidate()?;
    let observation = codex_inventory_observation_v0(session_root, &root_revision, &sources)?;
    let source_keys = sources
        .iter()
        .map(|(_, source_key, _)| source_key.clone())
        .collect::<Vec<_>>();
    let certificate = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        CODEX_DISCOVERY_REVISION,
        source_keys,
    )?;
    Ok(CodexRootInventoryV0 {
        sources,
        certificate,
    })
}

pub(crate) fn discover_codex_session_tree_inventory_from_plans_v0(
    session_roots: &[PathBuf],
    prior_inventory: &CodexSessionTreeInventoryV0,
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    let seeds = prior_inventory
        .sources
        .iter()
        .map(|(source, source_key, native_session_id)| {
            let expected = codex_source_key(native_session_id)?;
            if !expected.exact_descriptor_eq(source_key) {
                return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
            }
            Ok(CodexIncrementalInventorySeedV0 {
                native_session_id: native_session_id.clone(),
                parent_native_session_id: source.catalog_parent_native_session_id.clone(),
                root_native_session_id: source.catalog_root_native_session_id.clone(),
                observation: source.catalog_observation.clone(),
            })
        })
        .collect::<CodexSourceBackedResultV0<Vec<_>>>()?;
    discover_codex_session_tree_inventory_incremental_v0(session_roots, &seeds)
}

#[derive(Debug, Clone)]
struct CodexIncrementalInventorySeedV0 {
    native_session_id: String,
    parent_native_session_id: Option<String>,
    root_native_session_id: Option<String>,
    observation: CodexFileObservation,
}

#[derive(Debug)]
struct CodexMetadataInventoryLeafV0 {
    source_root: String,
    source_path: PathBuf,
    relative_path: PathBuf,
    observation: CodexFileObservation,
    authority: ProviderSourceRoot,
}

fn incremental_seed_from_certificate(
    certificate: &CertifiedSource,
) -> Option<CodexSourceBackedResultV0<CodexIncrementalInventorySeedV0>> {
    let source_key = certificate.observation().source();
    if !managed_codex_session_source(source_key)
        || certificate.observation().revision_kind() != CODEX_SOURCE_REVISION_KIND
    {
        return None;
    }
    let SourceAnchor::ProviderNative { namespace, key } = source_key.anchor() else {
        return None;
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return None;
    };
    if namespace != CODEX_SOURCE_ANCHOR_NAMESPACE {
        return None;
    }
    let observation = match serde_json::from_slice::<CodexFileObservation>(
        certificate.observation().revision(),
    ) {
        Ok(observation) => observation,
        Err(error) => return Some(Err(error.into())),
    };
    let (parent_native_session_id, root_native_session_id) = certificate
        .frontier()
        .filter(|frontier| frontier.checkpoint_kind() == CODEX_FRONTIER_KIND)
        .and_then(|frontier| match frontier.checkpoint() {
            TypedKey::Bytes(bytes) => CodexNativeCheckpoint::decode(bytes).ok(),
            _ => None,
        })
        .filter(|checkpoint| checkpoint.owner.native_session_id == *native_session_id)
        .map(|checkpoint| {
            (
                checkpoint.owner.parent_native_session_id,
                checkpoint.owner.root_native_session_id,
            )
        })
        .unwrap_or_default();
    Some(Ok(CodexIncrementalInventorySeedV0 {
        native_session_id: native_session_id.clone(),
        parent_native_session_id,
        root_native_session_id,
        observation,
    }))
}

fn discover_codex_session_tree_inventory_incremental_v0(
    session_roots: &[PathBuf],
    seeds: &[CodexIncrementalInventorySeedV0],
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    let normalized_roots = normalized_session_roots(session_roots)?;
    let mut leaves = Vec::new();
    let mut root_revisions = Vec::with_capacity(normalized_roots.len());
    let mut authorities = Vec::with_capacity(normalized_roots.len());
    let mut work = CodexCatalogWorkV0::default();
    for session_root in &normalized_roots {
        let (root, mut root_leaves, root_work) =
            discover_codex_metadata_inventory_root_v0(session_root)?;
        work.add_assign(root_work);
        crate::provider::codex::catalog::ensure_catalog_source_bound(
            leaves.len().saturating_add(root_leaves.len()),
        )?;
        leaves.append(&mut root_leaves);
        root_revisions.push(codex_root_revision_v0(session_root)?);
        authorities.push(root);
    }

    let candidates = exact_seed_candidates(&leaves, seeds);
    let mut catalog_sources = Vec::with_capacity(leaves.len());
    for (leaf, seed_index) in leaves.into_iter().zip(candidates) {
        let source = match seed_index {
            Some(seed_index) => catalog_source_from_seed(&leaf, &seeds[seed_index]),
            None => {
                work.source_body_reads = work.source_body_reads.saturating_add(1);
                work.session_meta_parses = work.session_meta_parses.saturating_add(1);
                catalog_source_from_body(&leaf)?
            }
        };
        catalog_sources.push(source);
    }
    for authority in &authorities {
        authority.revalidate()?;
    }

    let mut sources = bind_source_keys(catalog_sources)?;
    sort_bound_sources(&mut sources);
    let observation = if let [session_root] = normalized_roots.as_slice() {
        codex_inventory_observation_v0(session_root, &root_revisions[0], &sources)?
    } else {
        codex_session_tree_inventory_observation_v0(&normalized_roots, &root_revisions, &sources)?
    };
    let discovery_revision = if normalized_roots.len() == 1 {
        CODEX_DISCOVERY_REVISION
    } else {
        CODEX_SESSION_TREE_UNION_DISCOVERY_REVISION
    };
    let source_keys = sources
        .iter()
        .map(|(_, source_key, _)| source_key.clone())
        .collect();
    let certificate = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        discovery_revision,
        source_keys,
    )?;
    Ok(CodexSessionTreeInventoryV0 {
        sources,
        certificate,
        work,
    })
}

fn discover_codex_metadata_inventory_root_v0(
    session_root: &Path,
) -> CodexSourceBackedResultV0<(
    ProviderSourceRoot,
    Vec<CodexMetadataInventoryLeafV0>,
    CodexCatalogWorkV0,
)> {
    let authority = ProviderSourceRoot::open(session_root)?;
    let mut leaves = Vec::new();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    let mut visited_directories = 0_usize;
    let mut visited_entries = 0_usize;
    while let Some((relative_directory, depth)) = pending.pop() {
        if depth > PROVIDER_JSONL_INVENTORY_MAX_DEPTH {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex catalog directory depth exceeds the provider inventory bound".to_owned(),
                ),
            ));
        }
        visited_directories = visited_directories.saturating_add(1);
        if visited_directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex catalog directory count exceeds the provider inventory bound".to_owned(),
                ),
            ));
        }
        let directory = authority.open_directory(&relative_directory)?;
        let names = directory.entries(
            PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
                .saturating_sub(visited_entries)
                .saturating_add(1),
        )?;
        visited_entries = visited_entries.saturating_add(names.len());
        if visited_entries > PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex catalog entry count exceeds the provider inventory bound".to_owned(),
                ),
            ));
        }
        let mut child_directories = Vec::new();
        for name in names {
            let relative_path = relative_directory.join(&name);
            let source_path = session_root.join(&relative_path);
            if source_path.as_os_str().as_encoded_bytes().len()
                > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES
            {
                return Err(CodexSourceBackedErrorV0::Capture(
                    CaptureError::InvalidPayload(
                        "Codex catalog path exceeds the provider inventory bound".to_owned(),
                    ),
                ));
            }
            match directory.open_child(&name)? {
                OpenedProviderSourcePath::Directory(_) => {
                    child_directories.push((relative_path, depth.saturating_add(1)));
                }
                OpenedProviderSourcePath::File(opened)
                    if source_path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        == Some("jsonl") =>
                {
                    crate::provider::provider_path_identity(&source_path)?;
                    let observation = opened_codex_file_observation(&source_path, opened.file())?;
                    opened.revalidate_leaf()?;
                    leaves.push(CodexMetadataInventoryLeafV0 {
                        source_root: session_root.display().to_string(),
                        source_path,
                        relative_path,
                        observation,
                        authority: authority.clone(),
                    });
                    crate::provider::codex::catalog::ensure_catalog_source_bound(leaves.len())?;
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
        child_directories.reverse();
        pending.extend(child_directories);
    }
    authority.revalidate()?;
    let source_observations =
        u64::try_from(leaves.len()).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    Ok((
        authority,
        leaves,
        CodexCatalogWorkV0 {
            inventory_walks: 1,
            source_observations,
            ..CodexCatalogWorkV0::default()
        },
    ))
}

fn exact_seed_candidates(
    leaves: &[CodexMetadataInventoryLeafV0],
    seeds: &[CodexIncrementalInventorySeedV0],
) -> Vec<Option<usize>> {
    let mut seeds_by_stable = HashMap::<[u8; 32], Vec<usize>>::new();
    let mut leaves_by_stable = HashMap::<[u8; 32], Vec<usize>>::new();
    let mut seeds_by_native_id = HashMap::<&str, Vec<usize>>::new();
    for (index, seed) in seeds.iter().enumerate() {
        if let Some(stable) = seed.observation.stable_token {
            seeds_by_stable.entry(stable).or_default().push(index);
        }
        seeds_by_native_id
            .entry(seed.native_session_id.as_str())
            .or_default()
            .push(index);
    }
    for (index, leaf) in leaves.iter().enumerate() {
        if let Some(stable) = leaf.observation.stable_token {
            leaves_by_stable.entry(stable).or_default().push(index);
        }
    }

    let mut candidates = leaves
        .iter()
        .map(|leaf| {
            let stable_candidate = leaf.observation.stable_token.and_then(|stable| {
                match (
                    seeds_by_stable.get(&stable).map(Vec::as_slice),
                    leaves_by_stable.get(&stable).map(Vec::as_slice),
                ) {
                    (Some([seed_index]), Some([_]))
                        if seeds[*seed_index].observation == leaf.observation =>
                    {
                        Some(*seed_index)
                    }
                    _ => None,
                }
            });
            stable_candidate.or_else(|| {
                let native_session_id = codex_native_session_id_path_hint(&leaf.source_path)?;
                match seeds_by_native_id
                    .get(native_session_id.as_str())
                    .map(Vec::as_slice)
                {
                    Some([seed_index]) if seeds[*seed_index].observation == leaf.observation => {
                        Some(*seed_index)
                    }
                    _ => None,
                }
            })
        })
        .collect::<Vec<_>>();
    let mut candidate_counts = HashMap::<usize, usize>::new();
    for seed_index in candidates.iter().flatten() {
        *candidate_counts.entry(*seed_index).or_default() += 1;
    }
    for candidate in &mut candidates {
        if candidate.is_some_and(|seed_index| candidate_counts.get(&seed_index) != Some(&1)) {
            *candidate = None;
        }
    }
    candidates
}

fn catalog_source_from_seed(
    leaf: &CodexMetadataInventoryLeafV0,
    seed: &CodexIncrementalInventorySeedV0,
) -> CodexCatalogSource {
    CodexCatalogSource {
        source_root: leaf.source_root.clone(),
        source_path: leaf.source_path.clone(),
        cataloged_at_ms: 0,
        catalog_observation: leaf.observation.clone(),
        catalog_native_session_id: Some(seed.native_session_id.clone()),
        catalog_parent_native_session_id: seed.parent_native_session_id.clone(),
        catalog_root_native_session_id: seed.root_native_session_id.clone(),
        opened: None,
        authority_root: Some(leaf.authority.clone()),
        authority_relative_path: Some(leaf.relative_path.clone()),
    }
}

fn catalog_source_from_body(
    leaf: &CodexMetadataInventoryLeafV0,
) -> CodexSourceBackedResultV0<CodexCatalogSource> {
    let opened = leaf.authority.open_file(&leaf.relative_path)?;
    let mut catalog = catalog_codex_explicit_session_opened(&leaf.source_path, &opened)?;
    catalog.source_root = leaf.source_root.clone();
    let discovery = super::discover_codex_catalog_sources(&[catalog]);
    if discovery.ineligible != 0 || !discovery.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: discovery.rejections.len(),
            failed: discovery.ineligible,
        });
    }
    let mut sources = discovery.sources;
    if sources.len() != 1 {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: sources.len(),
            failed: 0,
        });
    }
    let mut source = sources
        .pop()
        .ok_or(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        })?;
    if source.catalog_observation != leaf.observation {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    source.authority_root = Some(leaf.authority.clone());
    source.authority_relative_path = Some(leaf.relative_path.clone());
    Ok(source)
}

fn codex_native_session_id_path_hint(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() >= 36 {
        let tail = &stem[stem.len() - 36..];
        if tail
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return Some(tail.to_owned());
        }
    }
    (!stem.trim().is_empty()).then(|| stem.to_owned())
}

fn normalized_session_roots(session_roots: &[PathBuf]) -> CodexSourceBackedResultV0<Vec<PathBuf>> {
    let mut normalized_roots = session_roots
        .iter()
        .map(|root| absolute_lexical_path(root))
        .collect::<CodexSourceBackedResultV0<Vec<_>>>()?;
    normalized_roots.sort();
    normalized_roots.dedup();
    if normalized_roots.is_empty() {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(
                "Codex session-tree authority has no inventory roots".to_owned(),
            ),
        ));
    }
    Ok(normalized_roots)
}

fn sort_bound_sources(sources: &mut [(CodexCatalogSource, SourceKey, String)]) {
    sources.sort_by(|left, right| {
        left.1
            .identity()
            .digest()
            .cmp(&right.1.identity().digest())
            .then_with(|| {
                left.1
                    .exact_descriptor_digest()
                    .cmp(&right.1.exact_descriptor_digest())
            })
            .then_with(|| left.2.cmp(&right.2))
    });
}

fn codex_inventory_observation_v0(
    session_root: &Path,
    root_revision: &[u8; 32],
    sources: &[(CodexCatalogSource, SourceKey, String)],
) -> CodexSourceBackedResultV0<SourceInventoryObservation> {
    let root_identity = crate::provider::provider_path_identity(session_root)
        .map_err(CodexSourceBackedErrorV0::Capture)?;
    let authority_key: [u8; 32] = Sha256::digest(root_identity.as_bytes()).into();
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(_, source_key, _)| source_key.identity().digest());

    let mut revision = Sha256::new();
    revision.update(b"ctx.codex-session-tree-inventory-v1\0");
    revision.update(root_revision);
    hash_inventory_field(&mut revision, root_identity.as_bytes());
    revision.update((ordered.len() as u64).to_be_bytes());
    for (source, source_key, native_session_id) in ordered {
        revision.update(source_key.identity().digest());
        revision.update(source_key.exact_descriptor_digest());
        hash_inventory_field(&mut revision, source.source_root.as_bytes());
        let path_identity = crate::provider::provider_path_identity(&source.source_path)?;
        hash_inventory_field(&mut revision, path_identity.as_bytes());
        hash_inventory_field(&mut revision, native_session_id.as_bytes());
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::Codex.as_str(),
        CODEX_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(authority_key.to_vec())?,
        CODEX_INVENTORY_REVISION_KIND,
        revision.finalize().to_vec(),
    )?)
}

fn codex_session_tree_inventory_observation_v0(
    session_roots: &[PathBuf],
    root_revisions: &[[u8; 32]],
    sources: &[(CodexCatalogSource, SourceKey, String)],
) -> CodexSourceBackedResultV0<SourceInventoryObservation> {
    if session_roots.len() != root_revisions.len() {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SystemInvariant(
                "Codex session-tree root revisions did not match their roots",
            ),
        ));
    }

    let mut authority = Sha256::new();
    authority.update(b"ctx.codex-session-tree-union-authority-v0\0");
    authority.update((session_roots.len() as u64).to_be_bytes());
    let mut revision = Sha256::new();
    revision.update(b"ctx.codex-session-tree-union-inventory-v0\0");
    revision.update((session_roots.len() as u64).to_be_bytes());
    for (session_root, root_revision) in session_roots.iter().zip(root_revisions) {
        let root_identity = crate::provider::provider_path_identity(session_root)?;
        hash_inventory_field(&mut authority, root_identity.as_bytes());
        hash_inventory_field(&mut revision, root_identity.as_bytes());
        revision.update(root_revision);
    }
    let authority_key: [u8; 32] = authority.finalize().into();

    revision.update((sources.len() as u64).to_be_bytes());
    for (source, source_key, native_session_id) in sources {
        revision.update(source_key.identity().digest());
        revision.update(source_key.exact_descriptor_digest());
        hash_inventory_field(&mut revision, source.source_root.as_bytes());
        let path_identity = crate::provider::provider_path_identity(&source.source_path)?;
        hash_inventory_field(&mut revision, path_identity.as_bytes());
        hash_inventory_field(&mut revision, native_session_id.as_bytes());
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::Codex.as_str(),
        CODEX_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(authority_key.to_vec())?,
        CODEX_SESSION_TREE_UNION_INVENTORY_REVISION_KIND,
        revision.finalize().to_vec(),
    )?)
}

fn hash_inventory_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn codex_root_revision_v0(session_root: &Path) -> CodexSourceBackedResultV0<[u8; 32]> {
    let root_identity = crate::provider::provider_path_identity(session_root)?;
    let mut revision = Sha256::new();
    revision.update(b"ctx.codex-session-root-revision-v0\0");
    hash_inventory_field(&mut revision, root_identity.as_bytes());
    Ok(revision.finalize().into())
}

pub(crate) fn writer_base_sources(
    writer: &GenerationWriter,
) -> HashMap<SourceKey, CertifiedSource> {
    writer
        .base_manifest()
        .into_iter()
        .flat_map(|manifest| manifest.sources.iter())
        .cloned()
        .map(|source| (source.observation().source().clone(), source))
        .collect()
}

pub(crate) fn managed_codex_session_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::Codex.as_str()
        && source.source_format() == CODEX_SESSION_SOURCE_FORMAT
        && source.schema_variant() == CODEX_SOURCE_SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

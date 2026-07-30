use super::*;

#[derive(Debug)]
pub(crate) struct CodexRootInventoryV0 {
    pub(crate) sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    pub(crate) certificate: CertifiedSourceInventory,
    pub(super) root: ProviderSourceRoot,
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
                    && left_source.catalog_observation == right_source.catalog_observation
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

    pub(crate) fn resolver(&self) -> CodexSourceBackedResultV0<Option<CodexLocatorResolverV0>> {
        let Some(plan) = self.source_plan() else {
            return Ok(None);
        };
        Ok(Some(CodexLocatorResolverV0::from_bound_sources([plan])?))
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

pub(crate) fn discover_codex_root_inventory_v0(
    session_root: &Path,
) -> CodexSourceBackedResultV0<CodexRootInventoryV0> {
    let retained = discover_codex_session_catalog_retained(session_root)?;
    build_codex_root_inventory_v0(session_root, retained)
}

pub(super) fn rediscover_codex_root_inventory_v0(
    session_root: &Path,
    root: &ProviderSourceRoot,
) -> CodexSourceBackedResultV0<CodexRootInventoryV0> {
    let retained = rediscover_codex_session_catalog_retained(session_root, root)?;
    build_codex_root_inventory_v0(session_root, retained)
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
        root: retained.root,
    })
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

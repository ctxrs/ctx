use super::*;
use crate::provider::source_backed::family::jsonl::{
    observe_opened_file, JsonlFamilyOpenedMember, JsonlFileObservation,
};

#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    static AFTER_CODEX_METADATA_INVENTORY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn install_after_codex_metadata_inventory_hook(hook: impl FnOnce() + 'static) {
    AFTER_CODEX_METADATA_INVENTORY_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex metadata-inventory hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_codex_metadata_inventory_hook() {
    let hook = AFTER_CODEX_METADATA_INVENTORY_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexExplicitSessionSourceBackedInputV0 {
    path: PathBuf,
    source: SourceKey,
    native_session_id: String,
}

impl CodexExplicitSessionSourceBackedInputV0 {
    pub fn discover(path: impl AsRef<Path>) -> CodexSourceBackedResultV0<Self> {
        let path = absolute_lexical_path(path.as_ref())?;
        let (_, source, native_session_id) = open_codex_explicit_source_plan_v0(&path)?;
        Ok(Self {
            path,
            source,
            native_session_id,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }
}

pub(crate) fn observe_codex_explicit_session_source_backed_v0(
    input: &CodexExplicitSessionSourceBackedInputV0,
) -> CodexSourceBackedResultV0<Option<(CodexCatalogSource, SourceKey, String)>> {
    match open_codex_explicit_source_plan_v0(input.path()) {
        Ok(plan)
            if plan.1.exact_descriptor_eq(input.source()) && plan.2 == input.native_session_id =>
        {
            Ok(Some(plan))
        }
        Ok(_) => Err(CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged),
        Err(CodexSourceBackedErrorV0::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn open_codex_explicit_source_plan_v0(
    path: &Path,
) -> CodexSourceBackedResultV0<(CodexCatalogSource, SourceKey, String)> {
    let opened = Arc::new(open_provider_source_file(path)?);
    let frozen_observation = opened_codex_file_observation(path, opened.file())?;
    let frozen_prefix_sha256 = opened_file_prefix_sha256(opened.file(), frozen_observation.len)?;
    let after = opened_codex_file_observation(path, opened.file())?;
    if !frozen_observation.admits_append_only_growth(&after) {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
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
    if !frozen_observation.admits_append_only_growth(&source.catalog_observation) {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    source.catalog_observation = frozen_observation;
    source.catalog_prefix_sha256 = Some(frozen_prefix_sha256);
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

pub fn absolute_lexical_path(path: &Path) -> std::io::Result<PathBuf> {
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

pub(super) fn bind_source_keys(
    sources: Vec<CodexCatalogSource>,
) -> CodexSourceBackedResultV0<Vec<(CodexCatalogSource, SourceKey, String)>> {
    let mut bound = Vec::with_capacity(sources.len());
    for source in sources {
        let native_session_id = source.catalog_native_session_id.clone().ok_or_else(|| {
            CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: source.source_path.clone(),
            }
        })?;
        let source_key = codex_source_key_in_root(source.source_root_lineage, &native_session_id)?;
        bound.push((source, source_key, native_session_id));
    }
    Ok(bound)
}

/// Discovers stable leaf identities and observations without opening provider
/// bodies. The JSONL family later supplies each leaf's durable base
/// certificate, so only a changed or parser-migrated leaf needs hydration from
/// its own bytes.
pub(crate) fn discover_codex_deferred_session_tree_inventory_v0(
    session_roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexDeferredSessionTreeInventoryV0> {
    discover_codex_session_tree_metadata_inventory_v0(session_roots)
}

/// Binds one already-authorized member without walking the surrounding Codex
/// catalog. Shared JSONL retains physical authority and owns later scanning
/// and publication proof.
pub(super) fn bind_codex_partial_member_v0(
    member: &JsonlFamilyOpenedMember<'_>,
    source_root_lineage: Option<[u8; 32]>,
) -> CodexSourceBackedResultV0<(CodexCatalogSource, SourceKey, String)> {
    if !crate::provider::codex::catalog::is_codex_session_rollout_path(member.source_path()) {
        return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
            rejected: 1,
            failed: 0,
        });
    }
    ctx_history_source_io::provider_path_identity(member.source_path())
        .map_err(CaptureError::from)?;
    let observation = opened_codex_file_observation(member.source_path(), member.opened().file())?;
    let native_session_id =
        crate::provider::codex::catalog::codex_canonical_session_id_from_path(member.source_path())
            .or(
                crate::provider::codex::catalog::probe_codex_native_session_id(
                    member.source_path(),
                    member.opened(),
                    observation.len,
                )?,
            )
            .or_else(|| codex_uncompressed_native_session_id_path_hint(member.source_path()))
            .ok_or_else(|| CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: member.source_path().to_path_buf(),
            })?;
    let after = opened_codex_file_observation(member.source_path(), member.opened().file())?;
    if !observation.admits_append_only_growth(&after) {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture,
        ));
    }
    member.opened().revalidate_same_object()?;
    let source = CodexCatalogSource {
        source_path: member.source_path().to_path_buf(),
        source_root_lineage,
        catalog_observation: observation,
        carried_jsonl_observation: Some(member.observation().clone()),
        catalog_prefix_sha256: None,
        catalog_native_session_id: Some(native_session_id.clone()),
        authority_root: Some(member.authority().as_ref().clone()),
        authority_relative_path: Some(member.authority_path().to_path_buf()),
    };
    let source_key = codex_source_key_in_root(source.source_root_lineage, &native_session_id)?;
    Ok((source, source_key, native_session_id))
}

#[derive(Debug)]
struct CodexMetadataInventoryLeafV0 {
    source_path: PathBuf,
    relative_path: PathBuf,
    observation: CodexFileObservation,
    jsonl_observation: JsonlFileObservation,
    authority: ProviderSourceRoot,
}

#[derive(Debug, Clone)]
pub(super) struct CodexRejectedCatalogLeafV0 {
    pub(super) source_path: PathBuf,
    pub(super) source_root_lineage: Option<[u8; 32]>,
    pub(super) authority_path: PathBuf,
    pub(super) observation: CodexFileObservation,
    pub(super) jsonl_observation: JsonlFileObservation,
    pub(super) reason: &'static str,
}

pub(crate) struct CodexDeferredSessionTreeInventoryV0 {
    pub(super) sources: Vec<CodexSessionPlanV0>,
    pub(super) rejected_leaves: Vec<CodexRejectedCatalogLeafV0>,
}

fn discover_codex_session_tree_metadata_inventory_v0(
    session_roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexDeferredSessionTreeInventoryV0> {
    let normalized_roots = normalized_session_roots(session_roots)?;
    let mut leaves = Vec::new();
    let mut authorities = Vec::with_capacity(normalized_roots.len());
    for session_root in &normalized_roots {
        let (root, mut root_leaves) = discover_codex_metadata_inventory_root_v0(session_root)?;
        crate::provider::codex::catalog::ensure_catalog_source_bound(
            leaves.len().saturating_add(root_leaves.len()),
        )?;
        leaves.append(&mut root_leaves);
        authorities.push(root);
    }

    #[cfg(any(test, feature = "test-support"))]
    run_after_codex_metadata_inventory_hook();

    let mut catalog_sources = Vec::with_capacity(leaves.len());
    let mut rejected_leaves = Vec::new();
    for leaf in leaves {
        match catalog_source_from_path_hint(&leaf) {
            Ok(source) => catalog_sources.push(source),
            Err(CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(_)))
            | Err(CodexSourceBackedErrorV0::MissingNativeSessionId { .. }) => {
                rejected_leaves.push(CodexRejectedCatalogLeafV0 {
                    source_path: leaf.source_path,
                    source_root_lineage: Some(codex_session_tree_source_root_lineage(
                        leaf.authority.named_path(),
                    )?),
                    authority_path: leaf.relative_path,
                    observation: leaf.observation,
                    jsonl_observation: leaf.jsonl_observation,
                    reason: "missing or conflicting Codex session owner",
                });
            }
            Err(error) => return Err(error),
        }
    }
    for authority in &authorities {
        authority.revalidate()?;
    }

    let mut sources = bind_source_keys(catalog_sources)?;
    sort_bound_sources(&mut sources);
    coalesce_codex_session_representations(&mut sources);
    rejected_leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    Ok(CodexDeferredSessionTreeInventoryV0 {
        sources,
        rejected_leaves,
    })
}

fn discover_codex_metadata_inventory_root_v0(
    session_root: &Path,
) -> CodexSourceBackedResultV0<(ProviderSourceRoot, Vec<CodexMetadataInventoryLeafV0>)> {
    let authority = ProviderSourceRoot::open(session_root)?;
    let mut leaves = Vec::new();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    let mut directory_observations = Vec::new();
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
                    if crate::provider::codex::catalog::is_codex_session_rollout_path(
                        &source_path,
                    ) =>
                {
                    ctx_history_source_io::provider_path_identity(&source_path)
                        .map_err(CaptureError::from)?;
                    let observation = opened_codex_file_observation(&source_path, opened.file())?;
                    let jsonl_observation = observe_opened_file(&source_path, &opened)?;
                    let after = opened_codex_file_observation(&source_path, opened.file())?;
                    if !observation.admits_append_only_growth(&after) {
                        return Err(CodexSourceBackedErrorV0::Capture(
                            CaptureError::SourceChangedDuringCapture,
                        ));
                    }
                    opened.revalidate_leaf()?;
                    leaves.push(CodexMetadataInventoryLeafV0 {
                        source_path,
                        relative_path,
                        observation,
                        jsonl_observation,
                        authority: authority.clone(),
                    });
                    crate::provider::codex::catalog::ensure_catalog_source_bound(leaves.len())?;
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
        directory_observations.push((
            relative_directory.clone(),
            directory.authority_fingerprint(),
        ));
        child_directories.reverse();
        pending.extend(child_directories);
    }
    // Reopen every visited directory after the complete walk and compare its
    // exact metadata stamp. This bounded second pass catches a nested source
    // that reappears after its directory was enumerated without retaining up
    // to 32,768 directory descriptors for the duration of discovery.
    for (relative_directory, expected) in directory_observations {
        let current = authority.open_directory(&relative_directory)?;
        if current.authority_fingerprint() != expected {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
        current.revalidate()?;
    }
    authority.revalidate()?;
    Ok((authority, leaves))
}

fn catalog_source_from_path_hint(
    leaf: &CodexMetadataInventoryLeafV0,
) -> CodexSourceBackedResultV0<CodexCatalogSource> {
    let native_session_id =
        match crate::provider::codex::catalog::codex_canonical_session_id_from_path(
            &leaf.source_path,
        ) {
            Some(native_session_id) => native_session_id,
            None => {
                let opened = leaf.authority.open_file(&leaf.relative_path)?;
                let admitted = opened_codex_file_observation(&leaf.source_path, opened.file())?;
                if !leaf.observation.admits_append_only_growth(&admitted) {
                    return Err(CodexSourceBackedErrorV0::Capture(
                        CaptureError::SourceChangedDuringCapture,
                    ));
                }
                let native_session_id =
                    crate::provider::codex::catalog::probe_codex_native_session_id(
                        &leaf.source_path,
                        &opened,
                        leaf.observation.len,
                    );
                opened.revalidate_leaf()?;
                native_session_id?
                    .or_else(|| codex_uncompressed_native_session_id_path_hint(&leaf.source_path))
                    .ok_or_else(|| CodexSourceBackedErrorV0::MissingNativeSessionId {
                        path: leaf.source_path.clone(),
                    })?
            }
        };
    Ok(CodexCatalogSource {
        source_path: leaf.source_path.clone(),
        source_root_lineage: Some(codex_session_tree_source_root_lineage(
            leaf.authority.named_path(),
        )?),
        catalog_observation: leaf.observation.clone(),
        carried_jsonl_observation: Some(leaf.jsonl_observation.clone()),
        catalog_prefix_sha256: None,
        catalog_native_session_id: Some(native_session_id.clone()),
        authority_root: Some(leaf.authority.clone()),
        authority_relative_path: Some(leaf.relative_path.clone()),
    })
}

pub(super) fn codex_native_session_id_path_hint(path: &Path) -> Option<String> {
    if let Some(native_session_id) =
        crate::provider::codex::catalog::codex_canonical_session_id_from_path(path)
    {
        return Some(native_session_id);
    }
    let stem = crate::provider::codex::catalog::codex_session_file_stem(path)?;
    (!stem.trim().is_empty()).then(|| stem.to_owned())
}

fn codex_uncompressed_native_session_id_path_hint(path: &Path) -> Option<String> {
    (!crate::provider::codex::catalog::is_codex_compressed_session_rollout_path(path))
        .then(|| codex_native_session_id_path_hint(path))
        .flatten()
}

pub(super) fn codex_terminal_native_session_id_hint(
    path: &Path,
    authority: &ProviderSourceRoot,
    authority_path: &Path,
) -> CodexSourceBackedResultV0<Option<String>> {
    let opened = authority.open_file(authority_path)?;
    let observation = opened_codex_file_observation(path, opened.file())?;
    let native_session_id = match crate::provider::codex::catalog::probe_codex_native_session_id(
        path,
        &opened,
        observation.len,
    ) {
        Ok(native_session_id) => native_session_id,
        // Opening discovery already binds malformed ownership as a
        // rejected leaf. A terminal hint must abstain for that same path
        // rather than promote one quarantined file into a route failure.
        Err(CaptureError::InvalidPayload(_)) => None,
        Err(error) => return Err(error.into()),
    };
    opened.revalidate_leaf()?;
    Ok(native_session_id.or_else(|| codex_uncompressed_native_session_id_path_hint(path)))
}

fn normalized_session_roots(session_roots: &[PathBuf]) -> CodexSourceBackedResultV0<Vec<PathBuf>> {
    let mut normalized_roots = session_roots
        .iter()
        .map(|root| absolute_lexical_path(root).map_err(CodexSourceBackedErrorV0::from))
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
            .then_with(|| {
                codex_representation_rank(&left.0).cmp(&codex_representation_rank(&right.0))
            })
            .then_with(|| left.0.source_path.cmp(&right.0.source_path))
    });
}

fn coalesce_codex_session_representations(
    sources: &mut Vec<(CodexCatalogSource, SourceKey, String)>,
) {
    sources.dedup_by(|right, left| left.1.exact_descriptor_eq(&right.1));
}

fn codex_representation_rank(source: &CodexCatalogSource) -> u8 {
    u8::from(
        crate::provider::codex::catalog::is_codex_compressed_session_rollout_path(
            &source.source_path,
        ),
    )
}

pub fn codex_session_root_rank(root: &Path) -> u8 {
    match root.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("sessions") => 0,
        Some("archived_sessions") => 1,
        _ => 2,
    }
}

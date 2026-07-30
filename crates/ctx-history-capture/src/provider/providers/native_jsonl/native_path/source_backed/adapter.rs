use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey,
    StableEntityId, TypedKey,
};
use sha2::{Digest, Sha256};

use super::{
    decode_certificate, decode_previous, DirectJsonlCheckpoint, DirectJsonlProjector,
    DirectJsonlSession, DirectJsonlSourceBackedError, DirectJsonlSourceBackedResult,
    DirectJsonlSourceReader, ProjectedLine, DIRECT_JSONL_DISCOVERY_REVISION,
    DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE, DIRECT_JSONL_INVENTORY_REVISION_KIND,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
    DIRECT_JSONL_SOURCE_IDENTITY_VERSION,
};
use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    provider::source_backed::family::jsonl::{
        observe_opened_file, probe_first_record, JsonlFileObservation, JsonlProbe, JsonlReader,
        JsonlSourceChange, JsonlSourceIdentity,
    },
    CaptureError, ProviderJsonlInventoryLimit, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

const DIRECT_JSONL_MAX_DIRECTORY_DEPTH: usize = 128;
const _: fn(
    &super::super::super::traversal::NativeJsonlSourceFile,
) -> &Arc<OpenedProviderSourceFile> = super::super::super::traversal::NativeJsonlSourceFile::opened;

#[cfg(test)]
std::thread_local! {
    static DIRECT_JSONL_INVENTORY_TRAVERSALS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_inventory_traversals() {
    DIRECT_JSONL_INVENTORY_TRAVERSALS.set(0);
}

#[cfg(test)]
pub(super) fn inventory_traversals() -> usize {
    DIRECT_JSONL_INVENTORY_TRAVERSALS.get()
}

#[cfg(test)]
fn record_inventory_traversal() {
    DIRECT_JSONL_INVENTORY_TRAVERSALS
        .set(DIRECT_JSONL_INVENTORY_TRAVERSALS.get().saturating_add(1));
}

#[derive(Default)]
struct DirectJsonlInventoryBudget {
    directories: usize,
    metadata_entries: usize,
}

struct DirectJsonlDirectoryTraversal<'capture> {
    adapter: DirectJsonlSourceAdapter,
    source_root: &'capture Path,
    authority: &'capture Arc<DirectJsonlAuthority>,
    leaves: &'capture mut Vec<DirectJsonlInventoryLeaf>,
    failures: &'capture mut Vec<DirectJsonlInventoryFailure>,
    budget: &'capture mut DirectJsonlInventoryBudget,
}

impl DirectJsonlDirectoryTraversal<'_> {
    fn visit(
        &mut self,
        absolute_path: &Path,
        relative_path: &Path,
        directory: &ProviderSourceDirectory,
        depth: usize,
    ) -> DirectJsonlSourceBackedResult<()> {
        if depth > DIRECT_JSONL_MAX_DIRECTORY_DEPTH {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: absolute_path.to_path_buf(),
                reason: "provider transcript directory nesting exceeds the supported limit",
            }
            .into());
        }
        self.budget.directories = self.budget.directories.saturating_add(1);
        if self.budget.directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
            return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::Directories,
                maximum: PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
                observed: self.budget.directories,
            }
            .into());
        }
        for name in directory.entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)? {
            self.budget.metadata_entries = self.budget.metadata_entries.saturating_add(1);
            if self.budget.metadata_entries > PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES {
                return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                    limit: ProviderJsonlInventoryLimit::MetadataEntries,
                    maximum: PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
                    observed: self.budget.metadata_entries,
                }
                .into());
            }
            let child_path = absolute_path.join(&name);
            let child_relative_path = relative_path.join(&name);
            let selected = selected_file(self.adapter.provider, directory, &child_path, &name)?;
            let opened = match directory.open_child(&name) {
                Ok(opened) => opened,
                Err(_error) if selected && self.adapter.provider == CaptureProvider::Tabnine => {
                    self.failures
                        .push(DirectJsonlInventoryFailure { path: child_path });
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match opened {
                OpenedProviderSourcePath::Directory(child_directory) => self.visit(
                    &child_path,
                    &child_relative_path,
                    &child_directory,
                    depth.saturating_add(1),
                )?,
                OpenedProviderSourcePath::File(file) if selected => {
                    let admitted = (|| {
                        let observation = observe_opened_file(&child_path, &file)?;
                        admit_leaf(
                            self.adapter,
                            child_path.clone(),
                            child_relative_path,
                            observation,
                            Arc::clone(self.authority),
                            self.source_root,
                            self.leaves,
                        )
                    })();
                    if let Err(error) = admitted {
                        if self.adapter.provider == CaptureProvider::Tabnine {
                            self.failures
                                .push(DirectJsonlInventoryFailure { path: child_path });
                        } else {
                            return Err(error);
                        }
                    }
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
        Ok(())
    }
}

fn selected_file(
    provider: CaptureProvider,
    directory: &ProviderSourceDirectory,
    path: &Path,
    name: &OsStr,
) -> DirectJsonlSourceBackedResult<bool> {
    let full_transcript_is_regular =
        if provider == CaptureProvider::Antigravity && name == OsStr::new("transcript.jsonl") {
            match directory.open_child(OsStr::new("transcript_full.jsonl")) {
                Ok(OpenedProviderSourcePath::File(file)) => {
                    file.revalidate()?;
                    true
                }
                Ok(OpenedProviderSourcePath::Directory(_)) | Err(_) => false,
            }
        } else {
            false
        };
    Ok(super::super::super::dialect::native_jsonl_file_is_selected(
        provider,
        path,
        full_transcript_is_regular,
    ))
}

fn admit_leaf(
    adapter: DirectJsonlSourceAdapter,
    path: PathBuf,
    authority_path: PathBuf,
    observation: JsonlFileObservation,
    authority: Arc<DirectJsonlAuthority>,
    source_root: &Path,
    leaves: &mut Vec<DirectJsonlInventoryLeaf>,
) -> DirectJsonlSourceBackedResult<()> {
    if leaves.len() == PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS {
        return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::EligiblePaths,
            maximum: PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
            observed: leaves.len().saturating_add(1),
        }
        .into());
    }
    if path_key(&path).len() > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
        return Err(CaptureError::InvalidPayload(
            "direct JSONL inventory path exceeds the encoded byte limit".to_owned(),
        )
        .into());
    }
    leaves.push(DirectJsonlInventoryLeaf {
        provider: adapter.provider,
        source_format: adapter.source_format,
        source_root: source_root.to_path_buf(),
        route_key: relative_route_key(source_root, &path),
        path,
        authority,
        authority_path,
        observation,
    });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectJsonlSourceAdapter {
    pub(super) provider: CaptureProvider,
    pub(super) source_format: &'static str,
    pub(super) schema_variant: &'static str,
}

impl DirectJsonlSourceAdapter {
    pub(crate) const fn new(
        provider: CaptureProvider,
        source_format: &'static str,
        schema_variant: &'static str,
    ) -> Self {
        Self {
            provider,
            source_format,
            schema_variant,
        }
    }

    pub(crate) fn source_format(self) -> &'static str {
        self.source_format
    }

    pub(crate) fn discover(
        self,
        root: impl AsRef<Path>,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceInventory> {
        #[cfg(test)]
        record_inventory_traversal();
        let root = root.as_ref();
        let opened = match open_provider_source_path(root) {
            Ok(opened) => opened,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return self.missing_inventory(root);
            }
            Err(error) => return Err(error.into()),
        };
        let mut leaves = Vec::new();
        let mut failures = Vec::new();
        let mut budget = DirectJsonlInventoryBudget::default();
        let authority = match opened {
            OpenedProviderSourcePath::Directory(directory) => {
                let authority = Arc::new(DirectJsonlAuthority {
                    root: Arc::new(directory.authority_root()),
                });
                DirectJsonlDirectoryTraversal {
                    adapter: self,
                    source_root: root,
                    authority: &authority,
                    leaves: &mut leaves,
                    failures: &mut failures,
                    budget: &mut budget,
                }
                .visit(root, Path::new(""), &directory, 0)?;
                authority.revalidate()?;
                authority
            }
            OpenedProviderSourcePath::File(opened_file) => {
                let opening = observe_opened_file(root, &opened_file)?;
                drop(opened_file);
                let parent =
                    root.parent()
                        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                            path: root.to_path_buf(),
                            reason: "provider transcript file has no authority parent",
                        })?;
                let name = root.file_name().ok_or_else(|| {
                    CaptureError::InvalidProviderTranscriptPath {
                        path: root.to_path_buf(),
                        reason: "provider transcript file has no leaf name",
                    }
                })?;
                let authority = Arc::new(DirectJsonlAuthority {
                    root: Arc::new(ProviderSourceRoot::open(parent)?),
                });
                let authority_path = PathBuf::from(name);
                let reopened = authority.root.open_file(&authority_path)?;
                let observation = observe_opened_file(root, &reopened)?;
                if observation != opening {
                    return Err(CaptureError::SourceChangedDuringCapture.into());
                }
                if super::super::super::dialect::native_jsonl_file_is_selected(
                    self.provider,
                    root,
                    false,
                ) {
                    admit_leaf(
                        self,
                        root.to_path_buf(),
                        authority_path,
                        observation,
                        Arc::clone(&authority),
                        root,
                        &mut leaves,
                    )?;
                }
                reopened.revalidate()?;
                authority.revalidate()?;
                authority
            }
        };

        let canonical_root = root.to_path_buf();
        leaves.sort_by_key(|leaf| leaf.route_key.clone());
        let observation = inventory_observation(self, &canonical_root, false, &leaves, &failures)?;
        Ok(DirectJsonlSourceInventory {
            adapter: self,
            observation,
            root_missing: false,
            authority: Some(authority),
            leaves,
            failures,
        })
    }

    fn missing_inventory(
        self,
        root: &Path,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceInventory> {
        let observation = inventory_observation(self, root, true, &[], &[])?;
        Ok(DirectJsonlSourceInventory {
            adapter: self,
            observation,
            root_missing: true,
            authority: None,
            leaves: Vec::new(),
            failures: Vec::new(),
        })
    }

    pub(crate) fn select_leaf(
        self,
        leaf: &DirectJsonlInventoryLeaf,
        imported_at: DateTime<Utc>,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSelectedLeaf> {
        if leaf.provider != self.provider || leaf.source_format != self.source_format {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let source_file = leaf.open_verified()?;
        let mut projector = DirectJsonlProjector::new(
            self.provider,
            self.source_format,
            &leaf.path,
            Some(leaf.source_root.clone()),
            imported_at,
            None,
        )?;
        let (projected, probe) = probe_first_record(&leaf.path, &source_file, |record| {
            let projected = projector.project_record(record)?;
            if !projected.rejections.is_empty() {
                return Err(DirectJsonlSourceBackedError::RejectedSource {
                    path: leaf.path.clone(),
                    rejected: projected.rejections.len(),
                });
            }
            Ok(projected)
        })?;
        let session = projector
            .session()
            .cloned()
            .ok_or_else(|| DirectJsonlSourceBackedError::MissingNativeSession(leaf.path.clone()))?;
        let (source, session_id) = direct_jsonl_session_identity(self, &session.native_session_id)?;
        source_file.revalidate()?;
        Ok(DirectJsonlSelectedLeaf {
            adapter: self,
            leaf: leaf.clone(),
            source,
            session_id,
            session,
            imported_at,
            source_file: Some(source_file),
            probe: Some(DirectJsonlProbe {
                physical: probe,
                projected,
            }),
        })
    }

    pub(crate) fn open_selected(
        self,
        mut selected: DirectJsonlSelectedLeaf,
        previous: Option<&CertifiedSource>,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceReader> {
        if selected.adapter != self {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let source_file = selected
            .source_file
            .take()
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        let previous_checkpoint =
            previous.and_then(|base| decode_previous(self, &selected, base).ok());
        let reader = JsonlReader::open(
            self.physical_identity(&selected.source, &selected.leaf.path),
            source_file,
            previous_checkpoint
                .as_ref()
                .map(|checkpoint| &checkpoint.physical),
            selected.probe.as_ref().map(|probe| probe.physical.clone()),
        )?;
        self.build_reader(selected, previous, previous_checkpoint, reader)
    }

    pub(crate) fn open_leaf(
        self,
        leaf: &DirectJsonlInventoryLeaf,
        imported_at: DateTime<Utc>,
        previous: Option<&CertifiedSource>,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceReader> {
        if let Some((base, checkpoint)) = previous
            .and_then(|base| {
                decode_certificate(self, base)
                    .ok()
                    .map(|checkpoint| (base, checkpoint))
            })
            .filter(|(_, checkpoint)| {
                checkpoint.physical.identity().source_path().as_path() == leaf.path
            })
        {
            let source_file = leaf.open_verified()?;
            let session = checkpoint
                .session
                .clone()
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
            let (source, session_id) =
                direct_jsonl_session_identity(self, &session.native_session_id)?;
            source.validate_exact_descriptor(base.observation().source())?;
            let mut selected = DirectJsonlSelectedLeaf {
                adapter: self,
                leaf: leaf.clone(),
                source,
                session_id,
                session,
                imported_at,
                source_file: None,
                probe: None,
            };
            let mut reader = JsonlReader::open(
                self.physical_identity(&selected.source, &selected.leaf.path),
                source_file,
                Some(&checkpoint.physical),
                None,
            )?;
            if reader.source_change() == JsonlSourceChange::Replace {
                let mut replacement = self.select_leaf(leaf, imported_at)?;
                let probe = replacement
                    .probe
                    .as_ref()
                    .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
                reader.restart_replacement(
                    self.physical_identity(&replacement.source, &replacement.leaf.path),
                    probe.physical.clone(),
                )?;
                replacement.source_file.take();
                selected = replacement;
            }
            return self.build_reader(selected, Some(base), Some(checkpoint), reader);
        }
        let selected = self.select_leaf(leaf, imported_at)?;
        self.open_selected(selected, previous)
    }

    fn build_reader(
        self,
        mut selected: DirectJsonlSelectedLeaf,
        previous: Option<&CertifiedSource>,
        previous_checkpoint: Option<DirectJsonlCheckpoint>,
        reader: JsonlReader,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceReader> {
        if reader.observation() != &selected.leaf.observation {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let disposition = match reader.source_change() {
            JsonlSourceChange::Cold if previous.is_some() => DirectJsonlDisposition::Replace,
            JsonlSourceChange::Cold => DirectJsonlDisposition::Cold,
            JsonlSourceChange::Unchanged => DirectJsonlDisposition::Unchanged,
            JsonlSourceChange::Append => DirectJsonlDisposition::Append,
            JsonlSourceChange::Replace => DirectJsonlDisposition::Replace,
        };
        let resumed = matches!(
            disposition,
            DirectJsonlDisposition::Unchanged | DirectJsonlDisposition::Append
        )
        .then_some(previous_checkpoint)
        .flatten();
        if matches!(
            disposition,
            DirectJsonlDisposition::Unchanged | DirectJsonlDisposition::Append
        ) && resumed.is_none()
        {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        let (
            accepted_events,
            accepted_file_touches,
            rejected_records,
            represented_physical_records,
            ignored_records,
            indexed_documents,
            resumed_session,
        ) = resumed
            .as_ref()
            .map_or((0, 0, 0, 0, 0, 0, None), |checkpoint| {
                (
                    checkpoint.accepted_events,
                    checkpoint.accepted_file_touches,
                    checkpoint.rejected_records,
                    checkpoint.represented_physical_records,
                    checkpoint.ignored_records,
                    checkpoint.indexed_documents,
                    checkpoint.session.clone(),
                )
            });
        let projector = DirectJsonlProjector::new(
            self.provider,
            self.source_format,
            &selected.leaf.path,
            Some(selected.leaf.source_root.clone()),
            selected.imported_at,
            resumed_session.or_else(|| Some(selected.session.clone())),
        )?;
        let pending_projected = matches!(
            disposition,
            DirectJsonlDisposition::Cold | DirectJsonlDisposition::Replace
        )
        .then(|| selected.probe.take().map(|probe| probe.projected))
        .flatten();
        Ok(DirectJsonlSourceReader {
            adapter: self,
            selected,
            reader,
            projector,
            base: previous.cloned(),
            disposition,
            accepted_events,
            accepted_file_touches,
            rejected_records,
            represented_physical_records,
            ignored_records,
            indexed_documents,
            pending_projected,
            exhausted: false,
        })
    }

    pub(super) fn certificate_belongs_to_leaf(
        self,
        leaf: &DirectJsonlInventoryLeaf,
        certificate: &CertifiedSource,
    ) -> bool {
        decode_certificate(self, certificate).is_ok_and(|checkpoint| {
            checkpoint.physical.identity().source_path().as_path() == leaf.path
        })
    }

    pub(super) fn physical_identity(self, source: &SourceKey, path: &Path) -> JsonlSourceIdentity {
        JsonlSourceIdentity::new(
            self.provider.as_str(),
            DIRECT_JSONL_NATIVEPATH_PARSER_REVISION,
            DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
            source.exact_descriptor_digest(),
            path,
        )
    }

    pub(super) fn owns(self, source: &SourceKey) -> bool {
        source.provider() == self.provider.as_str() && source.source_format() == self.source_format
    }

    pub(super) fn source_key(
        self,
        native_session_id: &str,
    ) -> DirectJsonlSourceBackedResult<SourceKey> {
        let anchor = SourceAnchor::provider_native(
            format!("{}.direct-jsonl-session", self.provider.as_str()),
            TypedKey::utf8(native_session_id)?,
        )?;
        Ok(SourceKey::derive(
            self.provider.as_str(),
            self.source_format,
            self.schema_variant,
            DIRECT_JSONL_SOURCE_IDENTITY_VERSION,
            anchor,
        )?)
    }
}

#[derive(Debug)]
pub(crate) struct DirectJsonlSourceInventory {
    pub(super) adapter: DirectJsonlSourceAdapter,
    pub(super) observation: SourceInventoryObservation,
    pub(super) root_missing: bool,
    authority: Option<Arc<DirectJsonlAuthority>>,
    pub(super) leaves: Vec<DirectJsonlInventoryLeaf>,
    pub(super) failures: Vec<DirectJsonlInventoryFailure>,
}

impl DirectJsonlSourceInventory {
    pub(crate) fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub(crate) fn leaves(&self) -> &[DirectJsonlInventoryLeaf] {
        &self.leaves
    }

    pub(crate) fn failures(&self) -> &[DirectJsonlInventoryFailure] {
        &self.failures
    }

    pub(super) fn is_exact_complete(&self) -> bool {
        !self.root_missing
            && self.failures.is_empty()
            && self
                .leaves
                .iter()
                .all(|leaf| leaf.observation.supports_exact_revalidation())
    }

    pub(crate) fn certify_against(
        &self,
        closing: &Self,
        sources: Vec<SourceKey>,
    ) -> DirectJsonlSourceBackedResult<CertifiedSourceInventory> {
        if let Some(authority) = &self.authority {
            authority.revalidate()?;
        }
        if let Some(authority) = &closing.authority {
            authority.revalidate()?;
        }
        if self.adapter != closing.adapter
            || self.root_missing
            || closing.root_missing
            || !self.failures.is_empty()
            || !closing.failures.is_empty()
            || self.observation != closing.observation
        {
            return Err(DirectJsonlSourceBackedError::IncompleteInventory);
        }
        Ok(CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            DIRECT_JSONL_DISCOVERY_REVISION,
            sources,
        )?)
    }
}

#[derive(Debug)]
struct DirectJsonlAuthority {
    root: Arc<ProviderSourceRoot>,
}

impl DirectJsonlAuthority {
    fn revalidate(&self) -> DirectJsonlSourceBackedResult<()> {
        self.root.revalidate()?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct DirectJsonlInventoryFailure {
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct DirectJsonlInventoryLeaf {
    pub(super) provider: CaptureProvider,
    pub(super) source_format: &'static str,
    pub(super) source_root: PathBuf,
    pub(super) route_key: Vec<u8>,
    pub(super) path: PathBuf,
    authority: Arc<DirectJsonlAuthority>,
    authority_path: PathBuf,
    pub(super) observation: JsonlFileObservation,
}

impl DirectJsonlInventoryLeaf {
    pub(super) fn open_verified(
        &self,
    ) -> DirectJsonlSourceBackedResult<Arc<OpenedProviderSourceFile>> {
        self.authority.revalidate()?;
        let opened = self.authority.root.open_file(&self.authority_path)?;
        if observe_opened_file(&self.path, &opened)? != self.observation {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        self.authority.revalidate()?;
        Ok(Arc::new(opened))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DirectJsonlSelectedLeaf {
    pub(super) adapter: DirectJsonlSourceAdapter,
    pub(super) leaf: DirectJsonlInventoryLeaf,
    pub(super) source: SourceKey,
    pub(super) session_id: StableEntityId,
    pub(super) session: DirectJsonlSession,
    pub(super) imported_at: DateTime<Utc>,
    source_file: Option<Arc<OpenedProviderSourceFile>>,
    probe: Option<DirectJsonlProbe>,
}

#[derive(Debug, Clone)]
struct DirectJsonlProbe {
    physical: JsonlProbe,
    projected: ProjectedLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectJsonlDisposition {
    Cold,
    Unchanged,
    Append,
    Replace,
}

pub(super) fn direct_jsonl_session_identity(
    adapter: DirectJsonlSourceAdapter,
    native_session_id: &str,
) -> DirectJsonlSourceBackedResult<(SourceKey, StableEntityId)> {
    let source = adapter.source_key(native_session_id)?;
    let native_session_key = NativeSessionKey::native_id(
        format!("{}.direct-jsonl-session", adapter.provider.as_str()),
        TypedKey::utf8(native_session_id)?,
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "direct-jsonl-session",
        native_session_key: &native_session_key,
    })?;
    Ok((source, session_id))
}

fn inventory_observation(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    root_missing: bool,
    leaves: &[DirectJsonlInventoryLeaf],
    failures: &[DirectJsonlInventoryFailure],
) -> DirectJsonlSourceBackedResult<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.direct-jsonl.inventory\0");
    digest.update([u8::from(root_missing)]);
    digest.update((leaves.len() as u64).to_be_bytes());
    for leaf in leaves {
        digest.update((leaf.route_key.len() as u64).to_be_bytes());
        digest.update(&leaf.route_key);
        let observation = serde_json::to_vec(&leaf.observation)?;
        digest.update((observation.len() as u64).to_be_bytes());
        digest.update(observation);
    }
    digest.update((failures.len() as u64).to_be_bytes());
    for failure in failures {
        let path = path_key(&failure.path);
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
    }
    Ok(SourceInventoryObservation::new(
        adapter.provider.as_str(),
        DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(path_key(root))?,
        DIRECT_JSONL_INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?)
}

fn relative_route_key(root: &Path, path: &Path) -> Vec<u8> {
    path.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(path_key)
        .or_else(|| {
            path.file_name()
                .map(|name| name.as_encoded_bytes().to_vec())
        })
        .unwrap_or_else(|| path_key(path))
}

fn path_key(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}

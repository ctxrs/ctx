use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, EventHydrationRequest, EventIdentityInput,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, PositionStability,
    ProjectionContractError, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    reader::{hydrated_direct_jsonl_lexical_text, DirectJsonlProjector},
    DirectJsonlEvent, DirectJsonlRejection, DirectJsonlSession,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION,
};
use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    provider::source_backed::family::jsonl::{
        observe_opened_file, probe_first_record, visit_verified_ranges, JsonlFamilyAdapter,
        JsonlFamilyAppendMode, JsonlFamilyHydrator, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlFamilyRejectedLeaf, JsonlHydrationRange, JsonlRecordRef,
    },
    CaptureError, ProviderJsonlInventoryLimit, ProviderSourceFailureKind, Result,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

const DIRECT_JSONL_SOURCE_IDENTITY_VERSION: u32 = 1;
const DIRECT_JSONL_MAX_DIRECTORY_DEPTH: usize = 128;
const DIRECT_JSONL_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;
const DIRECT_JSONL_MAX_TOUCHED_FILES: usize = 256;
const DIRECT_JSONL_MAX_EXPANDED_RECORD_UNITS: usize = 64;
const DIRECT_JSONL_MAX_EXPANDED_RECORD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
enum DirectJsonlAdapterError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("direct JSONL leaf {0:?} has no provider-native session identity")]
    MissingNativeSession(PathBuf),
    #[error("direct JSONL leaf changed provider-native session identity")]
    NativeSessionChanged,
    #[error("direct JSONL record expansion does not reconcile")]
    CountMismatch,
    #[error("direct JSONL event has no exact source-record evidence")]
    MissingRecordEvidence,
    #[error("direct JSONL locator does not belong to this adapter and certified leaf")]
    InvalidLocator,
    #[error("direct JSONL locator range exceeds the bounded provider record size")]
    LocatorRangeTooLarge,
    #[error("direct JSONL locator no longer selects a retained lexical event")]
    LocatorRecordNotRetained,
}

type DirectJsonlAdapterResult<T> = std::result::Result<T, DirectJsonlAdapterError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectJsonlFamilyBinding {
    source_root: PathBuf,
    route_key: Vec<u8>,
    session: DirectJsonlSession,
}

#[derive(Debug, Clone, Serialize)]
struct DirectJsonlRejectedLeafProof {
    route_key: Vec<u8>,
    physical_ordinal: u64,
    record_digest: [u8; 32],
    rejections: Vec<DirectJsonlRejection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectJsonlFamilyAdapter {
    provider: CaptureProvider,
    source_format: &'static str,
    schema_variant: &'static str,
}

impl DirectJsonlFamilyAdapter {
    pub(super) const fn new(
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

    fn source_key(self, native_session_id: &str) -> DirectJsonlAdapterResult<SourceKey> {
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

    fn session_identity(
        self,
        native_session_id: &str,
    ) -> DirectJsonlAdapterResult<(SourceKey, StableEntityId)> {
        let source = self.source_key(native_session_id)?;
        let native_session_key = NativeSessionKey::native_id(
            format!("{}.direct-jsonl-session", self.provider.as_str()),
            TypedKey::utf8(native_session_id)?,
        )?;
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "direct-jsonl-session",
            native_session_key: &native_session_key,
        })?;
        Ok((source, session_id))
    }

    fn discover_family(self, root: &Path) -> Result<JsonlFamilyInventory> {
        let opened = match open_provider_source_path(root) {
            Ok(opened) => opened,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider, root);
            }
            Err(error) => return Err(error),
        };
        let mut leaves = Vec::new();
        let mut rejected_leaves = Vec::new();
        let mut budget = DirectJsonlInventoryBudget::default();
        let authority = match opened {
            OpenedProviderSourcePath::Directory(directory) => {
                let authority = Arc::new(directory.authority_root());
                DirectJsonlDirectoryTraversal {
                    adapter: self,
                    source_root: root,
                    authority: &authority,
                    leaves: &mut leaves,
                    rejected_leaves: &mut rejected_leaves,
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
                let authority = Arc::new(ProviderSourceRoot::open(parent)?);
                let authority_path = PathBuf::from(name);
                let reopened = authority.open_file(&authority_path)?;
                let observation = observe_opened_file(root, &reopened)?;
                if observation != opening {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                if super::super::dialect::native_jsonl_file_is_selected(self.provider, root, false)
                {
                    bind_opened_leaf(
                        self,
                        root,
                        root.to_path_buf(),
                        authority_path,
                        Arc::clone(&authority),
                        reopened,
                        &mut leaves,
                        &mut rejected_leaves,
                    )?;
                }
                authority.revalidate()?;
                authority
            }
        };
        JsonlFamilyInventory::present_with_rejected(
            self.provider,
            root,
            authority,
            leaves,
            rejected_leaves,
        )
    }
}

impl JsonlFamilyAdapter for DirectJsonlFamilyAdapter {
    fn provider(&self) -> CaptureProvider {
        self.provider
    }

    fn source_format(&self) -> &'static str {
        self.source_format
    }

    fn schema_variant(&self) -> &'static str {
        self.schema_variant
    }

    fn parser_revision(&self) -> &'static str {
        DIRECT_JSONL_NATIVEPATH_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        self.discover_family(root)
    }

    fn discovery_error_kind(
        &self,
        error: &CaptureError,
    ) -> crate::provider::source_backed::SourceBackedRouteErrorKind {
        if matches!(
            error,
            CaptureError::ProviderSource {
                kind: ProviderSourceFailureKind::Io,
                ..
            }
        ) {
            crate::provider::source_backed::SourceBackedRouteErrorKind::Unavailable
        } else {
            crate::provider::source_backed::SourceBackedRouteErrorKind::InvalidSource
        }
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        DirectJsonlFamilyProjector::new(*self, leaf, imported_at)
            .map(|projector| Box::new(projector) as Box<dyn JsonlFamilyProjector>)
            .map_err(capture_error)
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        DirectJsonlFamilyHydrator::new(*self, leaf, source_file)
            .map(|hydrator| Box::new(hydrator) as Box<dyn JsonlFamilyHydrator>)
            .map_err(map_hydration_error)
    }
}

#[derive(Default)]
struct DirectJsonlInventoryBudget {
    directories: usize,
    metadata_entries: usize,
}

struct DirectJsonlDirectoryTraversal<'capture> {
    adapter: DirectJsonlFamilyAdapter,
    source_root: &'capture Path,
    authority: &'capture Arc<ProviderSourceRoot>,
    leaves: &'capture mut Vec<JsonlFamilyLeaf>,
    rejected_leaves: &'capture mut Vec<JsonlFamilyRejectedLeaf>,
    budget: &'capture mut DirectJsonlInventoryBudget,
}

impl DirectJsonlDirectoryTraversal<'_> {
    fn visit(
        &mut self,
        absolute_path: &Path,
        relative_path: &Path,
        directory: &ProviderSourceDirectory,
        depth: usize,
    ) -> Result<()> {
        if depth > DIRECT_JSONL_MAX_DIRECTORY_DEPTH {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: absolute_path.to_path_buf(),
                reason: "provider transcript directory nesting exceeds the supported limit",
            });
        }
        self.budget.directories = self.budget.directories.saturating_add(1);
        if self.budget.directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
            return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::Directories,
                maximum: PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
                observed: self.budget.directories,
            });
        }
        for name in directory.entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)? {
            self.budget.metadata_entries = self.budget.metadata_entries.saturating_add(1);
            if self.budget.metadata_entries > PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES {
                return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                    limit: ProviderJsonlInventoryLimit::MetadataEntries,
                    maximum: PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
                    observed: self.budget.metadata_entries,
                });
            }
            let child_path = absolute_path.join(&name);
            let child_relative_path = relative_path.join(&name);
            let selected = selected_file(self.adapter.provider, directory, &child_path, &name)?;
            let opened = match directory.open_child(&name) {
                Ok(opened) => opened,
                Err(error) if selected && self.adapter.provider == CaptureProvider::Tabnine => {
                    return Err(tabnine_unavailable_source(&child_path, error));
                }
                Err(error) => return Err(error),
            };
            match opened {
                OpenedProviderSourcePath::Directory(child_directory) => self.visit(
                    &child_path,
                    &child_relative_path,
                    &child_directory,
                    depth.saturating_add(1),
                )?,
                OpenedProviderSourcePath::File(file) if selected => {
                    if let Err(error) = bind_opened_leaf(
                        self.adapter,
                        self.source_root,
                        child_path.clone(),
                        child_relative_path,
                        Arc::clone(self.authority),
                        file,
                        self.leaves,
                        self.rejected_leaves,
                    ) {
                        if self.adapter.provider == CaptureProvider::Tabnine {
                            return Err(tabnine_unavailable_source(&child_path, error));
                        }
                        return Err(error);
                    }
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
        Ok(())
    }
}

fn tabnine_unavailable_source(path: &Path, error: CaptureError) -> CaptureError {
    CaptureError::ProviderSource {
        provider: CaptureProvider::Tabnine.as_str(),
        path: path.to_path_buf(),
        kind: ProviderSourceFailureKind::Io,
        detail: error.to_string(),
    }
}

fn selected_file(
    provider: CaptureProvider,
    directory: &ProviderSourceDirectory,
    path: &Path,
    name: &OsStr,
) -> Result<bool> {
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
    Ok(super::super::dialect::native_jsonl_file_is_selected(
        provider,
        path,
        full_transcript_is_regular,
    ))
}

#[allow(clippy::too_many_arguments)]
fn bind_opened_leaf(
    adapter: DirectJsonlFamilyAdapter,
    source_root: &Path,
    path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot>,
    source_file: OpenedProviderSourceFile,
    leaves: &mut Vec<JsonlFamilyLeaf>,
    rejected_leaves: &mut Vec<JsonlFamilyRejectedLeaf>,
) -> Result<()> {
    if leaves.len().saturating_add(rejected_leaves.len())
        == PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS
    {
        return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::EligiblePaths,
            maximum: PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
            observed: leaves
                .len()
                .saturating_add(rejected_leaves.len())
                .saturating_add(1),
        });
    }
    if path_key(&path).len() > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
        return Err(CaptureError::InvalidPayload(
            "direct JSONL inventory path exceeds the encoded byte limit".to_owned(),
        ));
    }
    let source_file = Arc::new(source_file);
    let mut projector = DirectJsonlProjector::new(
        adapter.provider,
        adapter.source_format,
        &path,
        Some(source_root.to_path_buf()),
        DateTime::<Utc>::UNIX_EPOCH,
        None,
    )?;
    let ((rejections, physical_ordinal, record_digest), probe) =
        probe_first_record(&path, &source_file, |record| {
            let evidence = record.evidence();
            Ok::<_, DirectJsonlAdapterError>((
                projector.identify_record(record)?,
                evidence.physical_ordinal(),
                evidence.record_digest(),
            ))
        })
        .map_err(capture_error)?;
    source_file.revalidate_same_object()?;
    let route_key = relative_route_key(source_root, &path);
    if !rejections.is_empty() {
        let proof = DirectJsonlRejectedLeafProof {
            route_key,
            physical_ordinal,
            record_digest,
            rejections,
        };
        rejected_leaves.push(JsonlFamilyRejectedLeaf::bind_observed(
            path,
            authority_path,
            TypedKey::bytes(serde_json::to_vec(&proof)?)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        ));
        return Ok(());
    }
    let session = projector
        .session()
        .cloned()
        .ok_or_else(|| DirectJsonlAdapterError::MissingNativeSession(path.clone()))
        .map_err(capture_error)?;
    let source = adapter
        .source_key(&session.native_session_id)
        .map_err(capture_error)?;
    let binding = DirectJsonlFamilyBinding {
        source_root: source_root.to_path_buf(),
        route_key,
        session,
    };
    leaves.push(JsonlFamilyLeaf::bind_observed(
        source,
        path,
        authority,
        authority_path,
        TypedKey::bytes(serde_json::to_vec(&binding)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        probe.observation().clone(),
    ));
    Ok(())
}

struct DirectJsonlFamilyProjector {
    adapter: DirectJsonlFamilyAdapter,
    source: SourceKey,
    source_path: PathBuf,
    route_key: Vec<u8>,
    bound_session: DirectJsonlSession,
    session_id: StableEntityId,
    projector: DirectJsonlProjector,
    rejected_records: u64,
}

impl DirectJsonlFamilyProjector {
    fn new(
        adapter: DirectJsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        imported_at: DateTime<Utc>,
    ) -> DirectJsonlAdapterResult<Self> {
        let binding = decode_binding(leaf)?;
        let (source, session_id) = adapter.session_identity(&binding.session.native_session_id)?;
        source.validate_exact_descriptor(leaf.source())?;
        let projector = DirectJsonlProjector::new(
            adapter.provider,
            adapter.source_format,
            leaf.source_path(),
            Some(binding.source_root.clone()),
            imported_at,
            Some(binding.session.clone()),
        )?;
        Ok(Self {
            adapter,
            source,
            source_path: leaf.source_path().to_path_buf(),
            route_key: binding.route_key,
            bound_session: binding.session,
            session_id,
            projector,
            rejected_records: 0,
        })
    }

    fn validate_session(&self) -> DirectJsonlAdapterResult<&DirectJsonlSession> {
        let session = self
            .projector
            .session()
            .ok_or(DirectJsonlAdapterError::NativeSessionChanged)?;
        if !same_session_identity(session, &self.bound_session) {
            return Err(DirectJsonlAdapterError::NativeSessionChanged);
        }
        Ok(session)
    }
}

impl JsonlFamilyProjector for DirectJsonlFamilyProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let projected = self.projector.project_record(record)?;
        let expanded_units = projected
            .events
            .iter()
            .map(|event| 1_usize.saturating_add(event.touches.len()))
            .sum::<usize>()
            .saturating_add(projected.rejections.len())
            .max(1);
        if expanded_units > DIRECT_JSONL_MAX_EXPANDED_RECORD_UNITS
            || projected.serialized_bytes > DIRECT_JSONL_MAX_EXPANDED_RECORD_BYTES
        {
            return Err(CaptureError::InvalidPayload(format!(
                "{} expands past a certified direct JSONL record boundary",
                self.source_path.display()
            )));
        }
        if !projected.rejections.is_empty() {
            if !projected.events.is_empty() {
                return Err(capture_error(DirectJsonlAdapterError::CountMismatch));
            }
            let rejected = u64::try_from(projected.rejections.len())
                .map_err(|_| capture_error(DirectJsonlAdapterError::CountMismatch))?;
            self.rejected_records = self
                .rejected_records
                .checked_add(rejected)
                .ok_or_else(|| capture_error(DirectJsonlAdapterError::CountMismatch))?;
            return Ok(());
        }
        let session = self.validate_session().map_err(capture_error)?.clone();
        for event in projected.events {
            emit(
                project_event(
                    self.adapter,
                    &self.source,
                    self.session_id,
                    &self.source_path,
                    &self.route_key,
                    &session,
                    event,
                )
                .map_err(capture_error)?,
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.validate_session().map(|_| ()).map_err(capture_error)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
}

struct DirectJsonlFamilyHydrator {
    adapter: DirectJsonlFamilyAdapter,
    source: SourceKey,
    source_path: PathBuf,
    source_root: PathBuf,
    route_key: Vec<u8>,
    session: DirectJsonlSession,
    source_file: Arc<OpenedProviderSourceFile>,
}

impl DirectJsonlFamilyHydrator {
    fn new(
        adapter: DirectJsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> DirectJsonlAdapterResult<Self> {
        let binding = decode_binding(leaf)?;
        let source = adapter.source_key(&binding.session.native_session_id)?;
        source.validate_exact_descriptor(leaf.source())?;
        validate_opened_identity(adapter, leaf.source_path(), &binding, &source_file, &source)?;
        Ok(Self {
            adapter,
            source,
            source_path: leaf.source_path().to_path_buf(),
            source_root: binding.source_root,
            route_key: binding.route_key,
            session: binding.session,
            source_file,
        })
    }
}

impl JsonlFamilyHydrator for DirectJsonlFamilyHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let hydrated = (|| {
            let (sub_ordinal, range) = hydration_locator_range(
                self.adapter,
                &self.source,
                &self.session.native_session_id,
                &self.route_key,
                request,
            )?;
            let mut records = visit_verified_ranges(
                &self.source_path,
                &self.source_file,
                &[range],
                |_index, bytes| {
                    let value: serde_json::Value = serde_json::from_slice(bytes)?;
                    let display_text = hydrated_direct_jsonl_lexical_text(
                        self.adapter.provider,
                        &value,
                        sub_ordinal,
                    )?
                    .ok_or(DirectJsonlAdapterError::LocatorRecordNotRetained)?;
                    Ok::<_, DirectJsonlAdapterError>(HydratedProviderRecord {
                        event_id: request.event_id(),
                        provider_bytes: display_text.into_bytes(),
                    })
                },
            )?;
            records
                .pop()
                .ok_or(DirectJsonlAdapterError::LocatorRecordNotRetained)
        })();
        hydrated.map_err(map_hydration_error)
    }

    fn finish(&mut self) -> std::result::Result<(), HydrationFailure> {
        let binding = DirectJsonlFamilyBinding {
            source_root: self.source_root.clone(),
            route_key: self.route_key.clone(),
            session: self.session.clone(),
        };
        validate_opened_identity(
            self.adapter,
            &self.source_path,
            &binding,
            &self.source_file,
            &self.source,
        )
        .map_err(map_hydration_error)
    }
}

fn validate_opened_identity(
    adapter: DirectJsonlFamilyAdapter,
    source_path: &Path,
    binding: &DirectJsonlFamilyBinding,
    source_file: &Arc<OpenedProviderSourceFile>,
    expected_source: &SourceKey,
) -> DirectJsonlAdapterResult<()> {
    let mut projector = DirectJsonlProjector::new(
        adapter.provider,
        adapter.source_format,
        source_path,
        Some(binding.source_root.clone()),
        DateTime::<Utc>::UNIX_EPOCH,
        None,
    )?;
    let (rejections, _) = probe_first_record(source_path, source_file, |record| {
        projector.identify_record(record)
    })?;
    if !rejections.is_empty() {
        return Err(DirectJsonlAdapterError::NativeSessionChanged);
    }
    let session = projector
        .session()
        .ok_or(DirectJsonlAdapterError::NativeSessionChanged)?;
    let source = adapter.source_key(&session.native_session_id)?;
    if !source.exact_descriptor_eq(expected_source)
        || !same_session_identity(session, &binding.session)
    {
        return Err(DirectJsonlAdapterError::NativeSessionChanged);
    }
    source_file.revalidate_same_object()?;
    Ok(())
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> DirectJsonlAdapterResult<DirectJsonlFamilyBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(DirectJsonlAdapterError::CountMismatch);
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn same_session_identity(left: &DirectJsonlSession, right: &DirectJsonlSession) -> bool {
    left.native_session_id == right.native_session_id
        && left.provider_session_id == right.provider_session_id
        && left.parent_provider_session_id == right.parent_provider_session_id
        && left.root_provider_session_id == right.root_provider_session_id
}

fn project_event(
    adapter: DirectJsonlFamilyAdapter,
    source: &SourceKey,
    session_id: StableEntityId,
    source_path: &Path,
    route_key: &[u8],
    session: &DirectJsonlSession,
    event: DirectJsonlEvent,
) -> DirectJsonlAdapterResult<LexicalDocument> {
    let evidence = &event.source_record;
    let byte_length = evidence
        .byte_end_exclusive
        .checked_sub(evidence.byte_start)
        .ok_or(DirectJsonlAdapterError::MissingRecordEvidence)?;
    if byte_length == 0 {
        return Err(DirectJsonlAdapterError::MissingRecordEvidence);
    }
    let native_item_key = if let Some(native_record_id) = event.native_record_id.as_deref() {
        NativeItemKey::native_id(
            format!("{}.direct-jsonl-event", adapter.provider.as_str()),
            TypedKey::utf8(native_record_id)?,
        )?
    } else {
        NativeItemKey::certified_position(
            format!("{}.direct-jsonl-ordinal", adapter.provider.as_str()),
            TypedKey::U64(event.raw_ordinal),
            PositionStability::AppendStable,
        )?
    };
    let subrecord_selector = (event.sub_ordinal != 0)
        .then(|| {
            SubrecordSelector::certified_position(
                "direct-jsonl-subrecord",
                TypedKey::U64(u64::from(event.sub_ordinal)),
                PositionStability::StableSlot,
            )
        })
        .transpose()?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "direct-jsonl-event",
        native_item_key: &native_item_key,
        subrecord_selector: subrecord_selector.as_ref(),
    })?;
    let native_event_key = TypedKey::composite(vec![
        event
            .native_record_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::U64(event.raw_ordinal)),
        TypedKey::U64(u64::from(event.sub_ordinal)),
        TypedKey::bytes(route_key.to_vec())?,
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: evidence.byte_start,
            byte_length,
            physical_ordinal: event.raw_ordinal,
            native_session_key: Some(TypedKey::utf8(&session.native_session_id)?),
            native_event_key: Some(native_event_key),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        evidence.record_digest,
    )?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| adapter.session_identity(parent).map(|(_, id)| id))
        .transpose()?;
    let root_session_id = match session.root_provider_session_id.as_deref() {
        Some(root) if root == session.native_session_id || root == session.provider_session_id => {
            session_id
        }
        Some(root) => adapter.session_identity(root)?.1,
        None => session_id,
    };
    let body = if event.lexical_text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        event.lexical_text
    };
    let touched_files = event
        .touches
        .into_iter()
        .map(|touch| touch.path)
        .filter(|path| path.len() <= DIRECT_JSONL_DOCUMENT_METADATA_BYTES)
        .take(DIRECT_JSONL_MAX_TOUCHED_FILES)
        .collect();
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: source_path.to_str().map(str::to_owned),
        agent_type: session.agent_type.as_str().to_owned(),
        is_primary: session.is_primary,
        event_sequence: event.provider_event_sequence_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: Some(event.role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: session.cwd.clone(),
        touched_files,
    })
}

fn hydration_locator_range(
    adapter: DirectJsonlFamilyAdapter,
    source: &SourceKey,
    native_session_id: &str,
    route_key: &[u8],
    request: &EventHydrationRequest,
) -> DirectJsonlAdapterResult<(u32, JsonlHydrationRange)> {
    let locator = request.locator();
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.source().provider() != adapter.provider.as_str()
        || locator.source().source_format() != adapter.source_format
        || locator.source().schema_variant() != adapter.schema_variant
        || locator.source().provider_identity_version() != DIRECT_JSONL_SOURCE_IDENTITY_VERSION
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(native_session_id.to_owned())) {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    }
    let Some(TypedKey::Composite(event_key)) = native_event_key else {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    };
    if event_key.len() != 3 || event_key.get(2) != Some(&TypedKey::Bytes(route_key.to_vec())) {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    }
    let Some(TypedKey::U64(sub_ordinal)) = event_key.get(1) else {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    };
    let sub_ordinal =
        u32::try_from(*sub_ordinal).map_err(|_| DirectJsonlAdapterError::InvalidLocator)?;
    let (_, session_id) = adapter.session_identity(native_session_id)?;
    let native_item_key = match event_key.first() {
        Some(TypedKey::Utf8(native_record_id)) => NativeItemKey::native_id(
            format!("{}.direct-jsonl-event", adapter.provider.as_str()),
            TypedKey::utf8(native_record_id)?,
        )?,
        Some(TypedKey::U64(raw_ordinal)) if raw_ordinal == physical_ordinal => {
            NativeItemKey::certified_position(
                format!("{}.direct-jsonl-ordinal", adapter.provider.as_str()),
                TypedKey::U64(*raw_ordinal),
                PositionStability::AppendStable,
            )?
        }
        _ => return Err(DirectJsonlAdapterError::InvalidLocator),
    };
    let subrecord_selector = (sub_ordinal != 0)
        .then(|| {
            SubrecordSelector::certified_position(
                "direct-jsonl-subrecord",
                TypedKey::U64(u64::from(sub_ordinal)),
                PositionStability::StableSlot,
            )
        })
        .transpose()?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "direct-jsonl-event",
        native_item_key: &native_item_key,
        subrecord_selector: subrecord_selector.as_ref(),
    })?;
    if event_id != request.event_id() {
        return Err(DirectJsonlAdapterError::InvalidLocator);
    }
    let byte_length =
        usize::try_from(*byte_length).map_err(|_| DirectJsonlAdapterError::LocatorRangeTooLarge)?;
    let range = JsonlHydrationRange::new(*byte_offset, byte_length, *locator.record_digest())
        .map_err(|_| DirectJsonlAdapterError::LocatorRangeTooLarge)?;
    Ok((sub_ordinal, range))
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

fn capture_error(error: DirectJsonlAdapterError) -> CaptureError {
    match error {
        DirectJsonlAdapterError::Capture(error) => error,
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}

fn map_hydration_error(error: DirectJsonlAdapterError) -> HydrationFailure {
    let kind = match error {
        DirectJsonlAdapterError::InvalidLocator
        | DirectJsonlAdapterError::LocatorRangeTooLarge
        | DirectJsonlAdapterError::Projection(_)
        | DirectJsonlAdapterError::Resolver(_) => HydrationFailureKind::InvalidLocator,
        DirectJsonlAdapterError::Capture(_)
        | DirectJsonlAdapterError::Json(_)
        | DirectJsonlAdapterError::MissingNativeSession(_)
        | DirectJsonlAdapterError::NativeSessionChanged
        | DirectJsonlAdapterError::CountMismatch
        | DirectJsonlAdapterError::MissingRecordEvidence
        | DirectJsonlAdapterError::LocatorRecordNotRetained => {
            HydrationFailureKind::StaleRecordEvidence
        }
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

#[cfg(test)]
#[path = "source_backed_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(super) use test_support::assert_source_backed_fixture;

#[cfg(test)]
#[path = "source_backed_architecture_tests.rs"]
mod architecture_tests;

#[cfg(all(test, unix))]
#[path = "source_backed_lifecycle_tests.rs"]
mod lifecycle_tests;

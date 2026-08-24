use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    admit_provider_declared_fact, derive_event_id, derive_native_session_id, CaptureProvider,
    CoreActivity, CoreRecord, CoreRecordError, EventIdentityInput, LiteralFactKind, NativeItemKey,
    PositionStability, ProjectionContractError, ProviderDeclaredFact, SourceKey, StableEntityId,
    SubrecordSelector, TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    copilot, reader::DirectJsonlProjector, DirectJsonlEvent, DirectJsonlRejection,
    DirectJsonlRetryDiscriminator, DirectJsonlSession,
};
use crate::{
    NativeJsonlError as CaptureError, NativeJsonlRuntime, ProviderJsonlInventoryLimit, Result,
};
use ctx_history_capture_model::ProviderSourceFailureKind;
use ctx_history_capture_runtime::{
    BaseEventLookup, SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_jsonl::{
    fit_jsonl_activity, observe_opened_file, probe_first_record, FallbackEventIdentityState,
    JsonlActivityObservedBytes, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyError,
    JsonlFamilyInventory, JsonlFamilyLeaf, JsonlFamilyProjectionMode, JsonlFamilyProjector,
    JsonlFamilyRejectedLeaf, JsonlFamilyWorkerContext, JsonlOversizedRecordPolicy, JsonlRecordRef,
    JsonlRuntimeLookup, OpenedProviderSourceFile, OpenedProviderSourcePath,
    ProviderSourceDirectory, ProviderSourceRoot,
};
use ctx_history_source_io::{
    open_provider_source_path_mapped as open_provider_source_path,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

const DIRECT_JSONL_SOURCE_IDENTITY_VERSION: u32 = 1;
const DIRECT_JSONL_MAX_DIRECTORY_DEPTH: usize = 128;
const DIRECT_JSONL_EVENT_IDENTITY_REVISION: &str = "direct-jsonl-content-occurrence-v2";

#[derive(Debug, Error)]
enum DirectJsonlAdapterError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Core(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("direct JSONL leaf {0:?} has no provider-native session identity")]
    MissingNativeSession(PathBuf),
    #[error("direct JSONL leaf changed provider-native session identity")]
    NativeSessionChanged,
    #[error("direct JSONL record expansion does not reconcile")]
    CountMismatch,
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

#[derive(Debug, PartialEq, Eq)]
pub struct DirectJsonlFamilyAdapter<R: NativeJsonlRuntime> {
    provider: CaptureProvider,
    source_format: &'static str,
    schema_variant: &'static str,
    parser_revision: &'static str,
    runtime: std::marker::PhantomData<R>,
}

impl<R: NativeJsonlRuntime> Copy for DirectJsonlFamilyAdapter<R> {}

impl<R: NativeJsonlRuntime> Clone for DirectJsonlFamilyAdapter<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: NativeJsonlRuntime> DirectJsonlFamilyAdapter<R> {
    pub(super) const fn new(
        provider: CaptureProvider,
        source_format: &'static str,
        schema_variant: &'static str,
        parser_revision: &'static str,
    ) -> Self {
        Self {
            provider,
            source_format,
            schema_variant,
            parser_revision,
            runtime: std::marker::PhantomData,
        }
    }

    const fn effective_parser_revision(self) -> &'static str {
        match self.provider {
            CaptureProvider::CopilotCli => copilot::COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION,
            _ => self.parser_revision,
        }
    }

    fn source_key(self, native_session_id: &str) -> DirectJsonlAdapterResult<SourceKey> {
        Ok(SourceKey::derive_provider_native(
            self.provider.as_str(),
            self.source_format,
            self.schema_variant,
            DIRECT_JSONL_SOURCE_IDENTITY_VERSION,
            format!("{}.direct-jsonl-session", self.provider.as_str()),
            TypedKey::utf8(native_session_id)?,
        )?)
    }

    fn session_identity(
        self,
        native_session_id: &str,
    ) -> DirectJsonlAdapterResult<(SourceKey, StableEntityId)> {
        let source = self.source_key(native_session_id)?;
        let session_id = derive_native_session_id(
            &source,
            "direct-jsonl-session",
            format!("{}.direct-jsonl-session", self.provider.as_str()),
            TypedKey::utf8(native_session_id)?,
        )?;
        Ok((source, session_id))
    }

    fn discover_family(self, root: &Path) -> Result<JsonlFamilyInventory<CaptureError>> {
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
                DirectJsonlDirectoryTraversal::<R> {
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

impl<R: NativeJsonlRuntime> JsonlFamilyAdapter for DirectJsonlFamilyAdapter<R> {
    type Runtime = R;

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
        self.effective_parser_revision()
    }

    fn event_identity_revision(&self) -> &'static str {
        DIRECT_JSONL_EVENT_IDENTITY_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        match self.provider {
            CaptureProvider::CopilotCli => JsonlFamilyAppendMode::Replacement,
            _ => JsonlFamilyAppendMode::CertifiedSuffix,
        }
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        match self.provider {
            CaptureProvider::CopilotCli => JsonlOversizedRecordPolicy::RejectRecord,
            _ => JsonlOversizedRecordPolicy::RejectSource,
        }
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory<CaptureError>> {
        self.discover_family(root)
    }

    fn discovery_error_kind(
        &self,
        error: &CaptureError,
    ) -> ctx_history_capture_runtime::SourceBackedRouteErrorKind {
        if matches!(
            error,
            CaptureError::ProviderSource {
                kind: ProviderSourceFailureKind::Io,
                ..
            }
        ) {
            ctx_history_capture_runtime::SourceBackedRouteErrorKind::Unavailable
        } else {
            ctx_history_capture_runtime::SourceBackedRouteErrorKind::InvalidSource
        }
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf<CaptureError>,
        source_file: Arc<OpenedProviderSourceFile<CaptureError>>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        DirectJsonlFamilyProjector::new(
            *self,
            leaf,
            &source_file,
            imported_at,
            None,
            JsonlFamilyProjectionMode::Cold,
        )
        .map(|projector| Box::new(projector) as Box<dyn JsonlFamilyProjector<Runtime = R>>)
        .map_err(capture_error)
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf<CaptureError>,
        source_file: Arc<OpenedProviderSourceFile<CaptureError>>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<JsonlRuntimeLookup<R>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "direct JSONL adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        DirectJsonlFamilyProjector::new(
            *self,
            leaf,
            &source_file,
            imported_at,
            base_event_lookup,
            mode,
        )
        .map(|projector| Box::new(projector) as Box<dyn JsonlFamilyProjector<Runtime = R>>)
        .map_err(capture_error)
    }
}

#[derive(Default)]
struct DirectJsonlInventoryBudget {
    directories: usize,
    metadata_entries: usize,
}

struct DirectJsonlDirectoryTraversal<'capture, R: NativeJsonlRuntime> {
    adapter: DirectJsonlFamilyAdapter<R>,
    source_root: &'capture Path,
    authority: &'capture Arc<ProviderSourceRoot<CaptureError>>,
    leaves: &'capture mut Vec<JsonlFamilyLeaf<CaptureError>>,
    rejected_leaves: &'capture mut Vec<JsonlFamilyRejectedLeaf>,
    budget: &'capture mut DirectJsonlInventoryBudget,
}

impl<R: NativeJsonlRuntime> DirectJsonlDirectoryTraversal<'_, R> {
    fn visit(
        &mut self,
        absolute_path: &Path,
        relative_path: &Path,
        directory: &ProviderSourceDirectory<CaptureError>,
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
                    return Err(R::tabnine_unavailable_source(&child_path, error));
                }
                // A link-like or non-regular entry that can never hold a
                // transcript (for example a `CLAUDE.md -> AGENTS.md` symlink
                // inside a Copilot CLI `files/` checkout, or a socket beside
                // transcripts) must not mark the whole source unreadable. The
                // entry is never followed, so skipping it preserves the
                // no-follow boundary; a selected transcript path stays
                // fail-closed below.
                Err(error) if membership_open_error_is_ignorable(selected, &error) => {
                    continue;
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
                            return Err(R::tabnine_unavailable_source(&child_path, error));
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

fn membership_open_error_is_ignorable(selected: bool, error: &CaptureError) -> bool {
    !selected && error.is_ignorable_membership_entry()
}

fn selected_file(
    provider: CaptureProvider,
    directory: &ProviderSourceDirectory<CaptureError>,
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
fn bind_opened_leaf<R: NativeJsonlRuntime>(
    adapter: DirectJsonlFamilyAdapter<R>,
    source_root: &Path,
    path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot<CaptureError>>,
    source_file: OpenedProviderSourceFile<CaptureError>,
    leaves: &mut Vec<JsonlFamilyLeaf<CaptureError>>,
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
        let rejected_records = u64::try_from(rejections.len()).map_err(|_| {
            CaptureError::SystemInvariant("direct JSONL rejected-record count overflow")
        })?;
        let proof = DirectJsonlRejectedLeafProof {
            route_key,
            physical_ordinal,
            record_digest,
            rejections,
        };
        rejected_leaves.push(JsonlFamilyRejectedLeaf::bind_observed(
            path,
            authority_path,
            probe.observation().clone(),
            TypedKey::bytes(serde_json::to_vec(&proof)?)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            rejected_records,
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

struct DirectJsonlFamilyProjector<R: NativeJsonlRuntime> {
    adapter: DirectJsonlFamilyAdapter<R>,
    source: SourceKey,
    bound_session: DirectJsonlSession,
    session_id: StableEntityId,
    projector: DirectJsonlProjector,
    fallback_identities: FallbackEventIdentityState<JsonlRuntimeLookup<R>, CaptureError>,
    rejected_records: u64,
    record_rejections: SourceBackedRecordRejectionDrafts,
    accepted_event_ids: HashSet<StableEntityId>,
    append_base_event_lookup: Option<JsonlRuntimeLookup<R>>,
    source_selector: String,
}

impl<R: NativeJsonlRuntime> DirectJsonlFamilyProjector<R> {
    fn new(
        adapter: DirectJsonlFamilyAdapter<R>,
        leaf: &JsonlFamilyLeaf<CaptureError>,
        _source_file: &OpenedProviderSourceFile<CaptureError>,
        imported_at: DateTime<Utc>,
        base_event_lookup: Option<JsonlRuntimeLookup<R>>,
        mode: JsonlFamilyProjectionMode,
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
        let append_base_event_lookup = (mode == JsonlFamilyProjectionMode::CertifiedAppend)
            .then(|| base_event_lookup.clone())
            .flatten();
        let fallback_identities = FallbackEventIdentityState::new(
            source.clone(),
            session_id,
            "direct-jsonl-event",
            format!("{}.direct-jsonl-fallback", adapter.provider.as_str()),
            DIRECT_JSONL_EVENT_IDENTITY_REVISION,
            mode.into(),
            base_event_lookup,
        )?;
        Ok(Self {
            adapter,
            source,
            bound_session: binding.session,
            session_id,
            projector,
            fallback_identities,
            rejected_records: 0,
            record_rejections: SourceBackedRecordRejectionDrafts::default(),
            accepted_event_ids: HashSet::new(),
            append_base_event_lookup,
            source_selector: leaf.source_path().display().to_string(),
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

    fn reject_record(&mut self, raw_ordinal: u64, detail: String) -> Result<()> {
        self.rejected_records = self
            .rejected_records
            .checked_add(1)
            .ok_or_else(|| capture_error(DirectJsonlAdapterError::CountMismatch))?;
        self.record_rejections
            .record(SourceBackedRecordRejectionDraft {
                source: self.source.clone(),
                provider: self.adapter.provider,
                source_selector: self.source_selector.clone(),
                line_number: raw_ordinal.saturating_add(1),
                payload_type: None,
                class: SourceBackedRecordRejectionClass::MalformedRecord,
                detail,
            });
        Ok(())
    }

    fn conflicts_with_accepted_identity(&self, records: &[CoreRecord]) -> Result<bool> {
        let mut record_event_ids = HashSet::with_capacity(records.len());
        for record in records {
            if !record_event_ids.insert(record.event_id)
                || self.accepted_event_ids.contains(&record.event_id)
            {
                return Ok(true);
            }
            if let Some(base_event_lookup) = self.append_base_event_lookup.as_ref() {
                let exists = base_event_lookup
                    .contains(record.event_id.as_uuid())
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
                if exists {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

impl<R: NativeJsonlRuntime> JsonlFamilyProjector for DirectJsonlFamilyProjector<R> {
    type Runtime = R;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let projected = self.projector.project_record(record)?;
        if !projected.rejections.is_empty() {
            if !projected.events.is_empty() {
                return Err(capture_error(DirectJsonlAdapterError::CountMismatch));
            }
            for rejection in projected.rejections {
                self.reject_record(rejection.raw_ordinal, rejection.reason)?;
            }
            return Ok(());
        }
        let session = self.validate_session().map_err(capture_error)?.clone();
        let raw_ordinal = projected.events.first().map(|event| event.raw_ordinal);
        if projected
            .events
            .iter()
            .any(|event| Some(event.raw_ordinal) != raw_ordinal)
        {
            return Err(capture_error(DirectJsonlAdapterError::CountMismatch));
        }
        let mut records = Vec::with_capacity(projected.events.len());
        for event in projected.events {
            records.push(
                project_event(
                    self.adapter,
                    &self.source,
                    self.session_id,
                    &session,
                    &mut self.fallback_identities,
                    event,
                )
                .map_err(capture_error)?,
            );
        }
        if self.conflicts_with_accepted_identity(&records)? {
            let raw_ordinal =
                raw_ordinal.ok_or_else(|| capture_error(DirectJsonlAdapterError::CountMismatch))?;
            self.reject_record(
                raw_ordinal,
                "provider record reused an event identity already accepted from this source"
                    .to_owned(),
            )?;
            return Ok(());
        }
        self.accepted_event_ids
            .extend(records.iter().map(|record| record.event_id));
        for record in records {
            emit(record)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.validate_session().map_err(capture_error)?;
        self.fallback_identities.finish()
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        std::mem::take(&mut self.record_rejections)
    }
}

fn decode_binding(
    leaf: &JsonlFamilyLeaf<CaptureError>,
) -> DirectJsonlAdapterResult<DirectJsonlFamilyBinding> {
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

fn project_event<R: NativeJsonlRuntime>(
    adapter: DirectJsonlFamilyAdapter<R>,
    source: &SourceKey,
    session_id: StableEntityId,
    session: &DirectJsonlSession,
    fallback_identities: &mut FallbackEventIdentityState<JsonlRuntimeLookup<R>, CaptureError>,
    event: DirectJsonlEvent,
) -> DirectJsonlAdapterResult<CoreRecord> {
    let subrecord_selector = match &event.stable_retry_discriminator {
        Some(DirectJsonlRetryDiscriminator::FactoryDroidToolResult { tool_use_id }) => {
            Some(SubrecordSelector::native_id(
                "factory-ai-droid.retry-tool-result",
                TypedKey::utf8(tool_use_id)?,
            )?)
        }
        None if event.sub_ordinal != 0 => Some(SubrecordSelector::certified_position(
            "direct-jsonl-subrecord",
            TypedKey::U64(u64::from(event.sub_ordinal)),
            PositionStability::StableSlot,
        )?),
        None => None,
    };
    let (native_item_key, native_record_key) =
        if let Some(native_record_id) = event.native_record_id.as_deref() {
            (
                NativeItemKey::native_id(
                    format!("{}.direct-jsonl-event", adapter.provider.as_str()),
                    TypedKey::utf8(native_record_id)?,
                )?,
                TypedKey::utf8(native_record_id)?,
            )
        } else {
            let assignment = fallback_identities.assign(
                TypedKey::utf8(&event.provider_event_hash)?,
                subrecord_selector.as_ref(),
            )?;
            (
                assignment.native_item_key().clone(),
                assignment.native_event_id().clone(),
            )
        };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "direct-jsonl-event",
        native_item_key: &native_item_key,
        subrecord_selector: subrecord_selector.as_ref(),
    })?;
    let native_subrecord_key = match &event.stable_retry_discriminator {
        Some(DirectJsonlRetryDiscriminator::FactoryDroidToolResult { tool_use_id }) => {
            TypedKey::composite(vec![
                TypedKey::utf8("factory-ai-droid.retry-tool-result")?,
                TypedKey::utf8(tool_use_id)?,
            ])?
        }
        None => TypedKey::U64(u64::from(event.sub_ordinal)),
    };
    let native_event_key = TypedKey::composite(vec![native_record_key, native_subrecord_key])?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| adapter.session_identity(parent).map(|(_, id)| id))
        .transpose()?;
    let root_session_id = match session.root_provider_session_id.as_deref() {
        Some(root) if root == session.native_session_id || root == session.provider_session_id => {
            Some(session_id)
        }
        Some(root) => Some(adapter.session_identity(root)?.1),
        None => None,
    };
    let body = if event.lexical_text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        event.lexical_text.clone()
    };
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event.provider_event_sequence_index,
        event.event_type.as_str(),
        adapter.effective_parser_revision(),
        body,
    )?;
    record.parent_session_id = parent_session_id;
    record.root_session_id = root_session_id;
    record.session_relationship = session.session_relationship;
    record.provider_session_id = Some(session.provider_session_id.clone());
    record.native_event_id = Some(native_event_key);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.agent_scope = session.agent_scope;
    record.content.structured_content = Some(event.native_value);
    let mut activity = event.activity;
    let mut facts = Vec::new();
    if let Some(cwd) = &session.cwd {
        push_admitted_fact(&mut facts, LiteralFactKind::SessionCwd, cwd.clone());
    }
    if let Some(activity) = activity.as_mut() {
        for fact in std::mem::take(&mut activity.facts) {
            push_admitted_fact(&mut facts, fact.kind, fact.value);
        }
    }
    for fact in event.facts {
        push_admitted_fact(&mut facts, fact.kind, fact.value);
    }
    if !facts.is_empty() {
        activity
            .get_or_insert_with(|| CoreActivity {
                revision: CORE_ACTIVITY_REVISION,
                provider_call_id: None,
                invocation: None,
                result: None,
                facts: Vec::new(),
            })
            .facts = facts;
    }
    if activity.as_ref().is_some_and(|activity| {
        activity.invocation.is_none() && activity.result.is_none() && activity.facts.is_empty()
    }) {
        activity = None;
    }
    record.content.activity = activity;
    if event.lexical_text.trim().is_empty() {
        record.content.normalized_body = None;
    }
    fit_activity_to_content_budget(&mut record)?;
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn push_admitted_fact(facts: &mut Vec<ProviderDeclaredFact>, kind: LiteralFactKind, value: String) {
    if let Some(fact) = admit_provider_declared_fact(kind, value, facts.len()) {
        facts.push(fact);
    }
}

fn fit_activity_to_content_budget(record: &mut CoreRecord) -> DirectJsonlAdapterResult<()> {
    fit_activity_within_content_budget(record, MAX_CORE_CONTENT_BYTES)
}

fn fit_activity_within_content_budget(
    record: &mut CoreRecord,
    maximum_bytes: usize,
) -> DirectJsonlAdapterResult<()> {
    if record.content.activity.is_none() || record.content.encoded_content_bytes()? <= maximum_bytes
    {
        return Ok(());
    }

    let content = &mut record.content;
    let body = content.normalized_body.as_deref().unwrap_or("");
    let structured_content = content.structured_content.as_ref();
    fit_jsonl_activity(
        body,
        structured_content,
        &mut content.activity,
        JsonlActivityObservedBytes::infer_from_present(),
        maximum_bytes,
    );
    Ok(())
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

#[cfg(test)]
#[path = "source_backed_result_tests.rs"]
mod result_tests;

#[cfg(test)]
#[path = "source_backed_authority_swap_tests.rs"]
mod authority_swap_tests;

#[cfg(test)]
#[path = "source_backed_copilot_tests.rs"]
mod copilot_tests;

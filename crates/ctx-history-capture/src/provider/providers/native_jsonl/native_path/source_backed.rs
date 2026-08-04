use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, McpExchangeContent, McpJsonCapture, McpPayloadOmissionReason,
    NativeItemKey, NativeSessionKey, PositionStability, ProjectionContractError,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, SubrecordSelector, TypedKey,
    MAX_CORE_CONTENT_BYTES,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    copilot, reader::DirectJsonlProjector, DirectJsonlEvent, DirectJsonlRejection,
    DirectJsonlRetryDiscriminator, DirectJsonlSession,
};
use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, probe_first_record, JsonlFamilyAdapter, JsonlFamilyAppendMode,
            JsonlFamilyInventory, JsonlFamilyLeaf, JsonlFamilyProjectionMode, JsonlFamilyProjector,
            JsonlFamilyRejectedLeaf, JsonlFamilyWorkerContext, JsonlOversizedRecordPolicy,
            JsonlRecordRef,
        },
        FallbackEventIdentityState,
    },
    CaptureError, ProviderJsonlInventoryLimit, ProviderSourceFailureKind, Result,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

const DIRECT_JSONL_SOURCE_IDENTITY_VERSION: u32 = 1;
const DIRECT_JSONL_MAX_DIRECTORY_DEPTH: usize = 128;
const DIRECT_JSONL_EVENT_IDENTITY_REVISION: &str = "direct-jsonl-content-occurrence-v1";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectJsonlFamilyAdapter {
    provider: CaptureProvider,
    source_format: &'static str,
    schema_variant: &'static str,
    parser_revision: &'static str,
}

impl DirectJsonlFamilyAdapter {
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
        }
    }

    const fn effective_parser_revision(self) -> &'static str {
        match self.provider {
            CaptureProvider::CopilotCli => copilot::COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION,
            _ => self.parser_revision,
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
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        DirectJsonlFamilyProjector::new(
            *self,
            leaf,
            &source_file,
            imported_at,
            None,
            JsonlFamilyProjectionMode::Cold,
        )
        .map(|projector| Box::new(projector) as Box<dyn JsonlFamilyProjector>)
        .map_err(capture_error)
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        .map(|projector| Box::new(projector) as Box<dyn JsonlFamilyProjector>)
        .map_err(capture_error)
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

struct DirectJsonlFamilyProjector {
    adapter: DirectJsonlFamilyAdapter,
    source: SourceKey,
    bound_session: DirectJsonlSession,
    session_id: StableEntityId,
    projector: DirectJsonlProjector,
    fallback_identities: FallbackEventIdentityState,
    rejected_records: u64,
}

impl DirectJsonlFamilyProjector {
    fn new(
        adapter: DirectJsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        source_file: &OpenedProviderSourceFile,
        imported_at: DateTime<Utc>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> DirectJsonlAdapterResult<Self> {
        let binding = decode_binding(leaf)?;
        let (source, session_id) = adapter.session_identity(&binding.session.native_session_id)?;
        source.validate_exact_descriptor(leaf.source())?;
        let mut projector = DirectJsonlProjector::new(
            adapter.provider,
            adapter.source_format,
            leaf.source_path(),
            Some(binding.source_root.clone()),
            imported_at,
            Some(binding.session.clone()),
        )?;
        let fallback_identities = FallbackEventIdentityState::new(
            source.clone(),
            session_id,
            "direct-jsonl-event",
            format!("{}.direct-jsonl-fallback", adapter.provider.as_str()),
            DIRECT_JSONL_EVENT_IDENTITY_REVISION,
            mode.into(),
            base_event_lookup,
        )?;
        if adapter.provider == CaptureProvider::CopilotCli {
            projector.set_copilot_mcp_tool_calls(copilot::copilot_mcp_tool_call_attributions(
                source_file,
            )?);
        }
        Ok(Self {
            adapter,
            source,
            bound_session: binding.session,
            session_id,
            projector,
            fallback_identities,
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
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let projected = self.projector.project_record(record)?;
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
                    &session,
                    &mut self.fallback_identities,
                    event,
                )
                .map_err(capture_error)?,
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if !self.projector.copilot_attribution_projection_matches() {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.validate_session().map_err(capture_error)?;
        self.fallback_identities.finish()
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
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
    session: &DirectJsonlSession,
    fallback_identities: &mut FallbackEventIdentityState,
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
            session_id
        }
        Some(root) => adapter.session_identity(root)?.1,
        None => session_id,
    };
    let has_tool_result = event
        .metadata
        .get("tool_result")
        .is_some_and(|value| !value.is_null());
    let body = if event.lexical_text.trim().is_empty() && has_tool_result {
        return Err(CaptureError::InvalidPayload(
            "direct JSONL selected result has no meaningful native content".to_owned(),
        )
        .into());
    } else if event.lexical_text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        event.lexical_text.clone()
    };
    let touches = event.touches;
    let entry_type = event.metadata.get("entry_type").cloned();
    let status = event.metadata.get("status").cloned();
    let model = event.metadata.get("model").cloned();
    let tokens = event.metadata.get("tokens").cloned();
    let tool_result = event.metadata.get("tool_result").cloned();
    let structured_content = (!touches.is_empty()
        || entry_type.as_ref().is_some_and(|value| !value.is_null())
        || status.as_ref().is_some_and(|value| !value.is_null())
        || model.as_ref().is_some_and(|value| !value.is_null())
        || tokens.as_ref().is_some_and(|value| !value.is_null())
        || tool_result.as_ref().is_some_and(|value| !value.is_null()))
    .then(|| {
        serde_json::json!({
            "entry_type": entry_type,
            "status": status,
            "model": model,
            "tokens": tokens,
            "file_touches": touches,
            "tool_result": tool_result,
        })
    });
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        root_session_id,
        source.clone(),
        event.provider_event_sequence_index,
        event.event_type.as_str(),
        session.agent_type.as_str(),
        session.is_primary,
        adapter.effective_parser_revision(),
        body,
    )?;
    record.parent_session_id = parent_session_id;
    record.provider_session_id = Some(session.provider_session_id.clone());
    record.native_event_id = Some(native_event_key);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.cwd = session.cwd.clone();
    record.mcp_tool_call = event.mcp_tool_call;
    record.content.structured_content = structured_content;
    record.content.mcp_exchange = event.mcp_exchange;
    fit_mcp_exchange_to_content_budget(&mut record)?;
    record.validate_contract()?;
    Ok(record)
}

fn fit_mcp_exchange_to_content_budget(record: &mut CoreRecord) -> DirectJsonlAdapterResult<()> {
    fit_mcp_exchange_within_content_budget(record, MAX_CORE_CONTENT_BYTES)
}

fn fit_mcp_exchange_within_content_budget(
    record: &mut CoreRecord,
    maximum_bytes: usize,
) -> DirectJsonlAdapterResult<()> {
    if record.content.mcp_exchange.is_none()
        || record.content.encoded_content_bytes()? <= maximum_bytes
    {
        return Ok(());
    }

    let exchange = record
        .content
        .mcp_exchange
        .as_ref()
        .ok_or(DirectJsonlAdapterError::CountMismatch)?;
    let arguments_candidate = omission_candidate(exchange, McpJsonCaptureTarget::Arguments)?;
    let payload_candidate = omission_candidate(exchange, McpJsonCaptureTarget::Payload)?;
    record.content.mcp_exchange = match (arguments_candidate, payload_candidate) {
        (Some(arguments), Some(payload)) if arguments.encoded_bytes <= payload.encoded_bytes => {
            Some(arguments.exchange)
        }
        (Some(_), Some(payload)) => Some(payload.exchange),
        (Some(arguments), None) => Some(arguments.exchange),
        (None, Some(payload)) => Some(payload.exchange),
        (None, None) => None,
    };

    if record.content.mcp_exchange.is_none()
        || record.content.encoded_content_bytes()? <= maximum_bytes
    {
        return Ok(());
    }

    let exchange = record
        .content
        .mcp_exchange
        .as_mut()
        .ok_or(DirectJsonlAdapterError::CountMismatch)?;
    if let Some(invocation) = exchange.invocation.as_mut() {
        omit_present_json(&mut invocation.arguments)?;
    }
    if let Some(response) = exchange.response.as_mut() {
        omit_present_json(&mut response.payload)?;
    }

    if record.content.encoded_content_bytes()? > maximum_bytes {
        // The normalized body and structured content remain authoritative. If
        // even the compact explicit-omission shape cannot coexist with them,
        // drop only the optional exchange annotation.
        record.content.mcp_exchange = None;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum McpJsonCaptureTarget {
    Arguments,
    Payload,
}

struct McpExchangeOmissionCandidate {
    exchange: McpExchangeContent,
    encoded_bytes: usize,
}

fn omission_candidate(
    exchange: &McpExchangeContent,
    target: McpJsonCaptureTarget,
) -> DirectJsonlAdapterResult<Option<McpExchangeOmissionCandidate>> {
    let mut candidate = exchange.clone();
    let capture = match target {
        McpJsonCaptureTarget::Arguments => candidate
            .invocation
            .as_mut()
            .map(|invocation| &mut invocation.arguments),
        McpJsonCaptureTarget::Payload => candidate
            .response
            .as_mut()
            .map(|response| &mut response.payload),
    };
    let Some(capture) = capture else {
        return Ok(None);
    };
    if !omit_present_json(capture)? {
        return Ok(None);
    }
    let encoded_bytes = serde_json::to_vec(&candidate)?.len();
    Ok(Some(McpExchangeOmissionCandidate {
        exchange: candidate,
        encoded_bytes,
    }))
}

fn omit_present_json(capture: &mut McpJsonCapture) -> DirectJsonlAdapterResult<bool> {
    let McpJsonCapture::Present { value } = capture else {
        return Ok(false);
    };
    let observed_encoded_bytes = u64::try_from(serde_json::to_vec(value)?.len()).ok();
    *capture = McpJsonCapture::Omitted {
        reason: McpPayloadOmissionReason::SizeLimit,
        observed_encoded_bytes,
    };
    Ok(true)
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
#[path = "source_backed_copilot_tests.rs"]
mod copilot_tests;

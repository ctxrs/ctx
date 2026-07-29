use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId,
    SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    reader::hydrated_direct_jsonl_lexical_text, DirectJsonlCheckpoint, DirectJsonlEvent,
    DirectJsonlFileObservation, DirectJsonlSession, DirectJsonlSourceChange,
};
use crate::{
    common::io::OpenedProviderSourceFile, CaptureError, ProviderJsonlInventoryLimit,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

const DIRECT_JSONL_SOURCE_IDENTITY_VERSION: u32 = 1;
const DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION: &str = "direct-native-jsonl-source-backed-v1";
const DIRECT_JSONL_SOURCE_FRONTIER_KIND: &str = "direct-native-jsonl-checkpoint-v1";
const DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE: &str = "direct-native-jsonl-provider-root-v1";
const DIRECT_JSONL_INVENTORY_REVISION_KIND: &str = "direct-native-jsonl-inventory-sha256-v1";
const DIRECT_JSONL_DISCOVERY_REVISION: &str = "direct-native-jsonl-discovery-v1";
const DIRECT_JSONL_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;
const DIRECT_JSONL_MAX_TOUCHED_FILES: usize = 256;

pub(crate) mod registration {
    use std::path::Path;

    use chrono::{DateTime, Utc};
    use ctx_history_core::{
        CaptureProvider, EventHydrationRequest, HydratedProviderRecord, HydrationFailure,
        HydrationFailureKind,
    };

    use super::{DirectJsonlCertifiedLeaf, DirectJsonlSourceAdapter};
    use crate::provider::source_backed::{
        captured_route_driver, executable_route, hydration_failure, invalid_route,
        provider_format_scope, route_error, ProviderCaptureSink, SourceBackedCoordinatorResult,
        SourceBackedProviderRegistry, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
    };
    use crate::ProviderSource;

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let adapter = adapter(source.provider).ok_or_else(|| {
            invalid_route(
                source.provider,
                "provider is not a member of the direct native-JSONL adapter family",
            )
        })?;
        let root = source.path.clone();
        let capture_root = root.clone();
        let hydration_root = root;
        let provider = source.provider;
        let certified_source_format = adapter.source_format();
        let driver = captured_route_driver(
            move |sink| capture(adapter, &capture_root, sink),
            provider_format_scope(provider, certified_source_format),
            move |request| hydrate(adapter, &hydration_root, request),
        );
        registry.register(executable_route(
            source,
            selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }

    fn adapter(provider: CaptureProvider) -> Option<DirectJsonlSourceAdapter> {
        match provider {
            CaptureProvider::Antigravity => Some(super::super::antigravity_source_backed_adapter()),
            CaptureProvider::CopilotCli => Some(super::super::copilot_source_backed_adapter()),
            CaptureProvider::FactoryAiDroid => {
                Some(super::super::factory_droid_source_backed_adapter())
            }
            CaptureProvider::Qoder => Some(super::super::qoder_source_backed_adapter()),
            CaptureProvider::QwenCode => Some(super::super::qwen_code_source_backed_adapter()),
            CaptureProvider::Tabnine => Some(super::super::tabnine_source_backed_adapter()),
            CaptureProvider::Windsurf => Some(super::super::windsurf_source_backed_adapter()),
            _ => None,
        }
    }

    fn capture(
        adapter: DirectJsonlSourceAdapter,
        root: &Path,
        sink: &mut dyn ProviderCaptureSink,
    ) -> SourceBackedRouteResult<()> {
        let inventory = adapter.discover(root).map_err(route_error)?;
        if inventory.root_missing() {
            return Ok(());
        }
        if !inventory.failures().is_empty() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "direct JSONL inventory contains inaccessible sources",
            ));
        }
        for leaf in inventory.leaves() {
            let mut reader = adapter
                .open_leaf(leaf, DateTime::<Utc>::UNIX_EPOCH)
                .map_err(route_error)?;
            let mut began = false;
            while let Some(page) = reader.next_page().map_err(route_error)? {
                if !began {
                    sink.begin(page.source.clone())?;
                    began = true;
                }
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            let certified = reader.finish().map_err(route_error)?;
            if !began {
                sink.begin(certified.source().clone())?;
            }
            sink.certify(certified.certificate().clone())?;
        }
        Ok(())
    }

    fn hydrate(
        adapter: DirectJsonlSourceAdapter,
        root: &Path,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let inventory = adapter.discover(root).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        for leaf in inventory.leaves() {
            let mut reader = adapter
                .open_leaf(leaf, DateTime::<Utc>::UNIX_EPOCH)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?;
            while reader
                .next_page()
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .is_some()
            {}
            let certified: DirectJsonlCertifiedLeaf = reader.finish().map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            if certified
                .source()
                .exact_descriptor_eq(request.locator().source())
            {
                let provider_bytes =
                    adapter
                        .hydrate(&certified, request.locator())
                        .map_err(|error| {
                            hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                        })?;
                return Ok(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes,
                });
            }
        }
        Err(hydration_failure(
            HydrationFailureKind::ConfirmedDeleted,
            "the exact direct JSONL source is absent from the complete inventory",
        ))
    }
}

#[derive(Debug, Error)]
pub(crate) enum DirectJsonlSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("direct JSONL inventory is incomplete and cannot certify deletion")]
    IncompleteInventory,
    #[error("direct JSONL leaf {0:?} has no provider-native session identity")]
    MissingNativeSession(PathBuf),
    #[error("direct JSONL leaf changed provider-native session identity while scanning")]
    NativeSessionChanged,
    #[error("direct JSONL leaf {path:?} rejected {rejected} records")]
    RejectedSource { path: PathBuf, rejected: usize },
    #[error("direct JSONL leaf scan did not reach a certified frontier")]
    IncompleteScan,
    #[error("direct JSONL scan counters do not reconcile")]
    CountMismatch,
    #[error("direct JSONL event has no exact source-record evidence")]
    MissingRecordEvidence,
    #[error("direct JSONL locator does not belong to this adapter and certified leaf")]
    InvalidLocator,
    #[error("direct JSONL locator range exceeds the bounded provider record size")]
    LocatorRangeTooLarge,
    #[error("direct JSONL locator range no longer exists")]
    LocatorRangeMissing,
    #[error("direct JSONL locator digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("direct JSONL locator no longer selects a retained lexical event")]
    LocatorRecordNotRetained,
}

pub(crate) type DirectJsonlSourceBackedResult<T> = Result<T, DirectJsonlSourceBackedError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectJsonlSourceAdapter {
    provider: CaptureProvider,
    source_format: &'static str,
    schema_variant: &'static str,
}

impl DirectJsonlSourceAdapter {
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

    pub(crate) fn provider(self) -> CaptureProvider {
        self.provider
    }

    pub(crate) fn source_format(self) -> &'static str {
        self.source_format
    }

    pub(crate) fn missing_reason(self) -> &'static str {
        super::super::dialect::native_jsonl_missing_reason(self.provider)
    }

    pub(crate) fn discover(
        self,
        root: impl AsRef<Path>,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceInventory> {
        let root = root.as_ref();
        let mut paths = Vec::new();
        let mut failures = Vec::new();
        let mut aggregate_path_bytes = 0_usize;
        let mut visit =
            |source_file: super::super::traversal::NativeJsonlSourceFile| -> crate::Result<()> {
                let path = source_file.path();
                if paths.len() == PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS {
                    return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                        limit: ProviderJsonlInventoryLimit::EligiblePaths,
                        maximum: PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
                        observed: paths.len().saturating_add(1),
                    });
                }
                aggregate_path_bytes = aggregate_path_bytes.saturating_add(path_key(path).len());
                if aggregate_path_bytes > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
                    return Err(CaptureError::InvalidPayload(
                        "direct JSONL inventory paths exceed the aggregate byte limit".to_owned(),
                    ));
                }
                let observation = super::reader::observe_opened_file(source_file.opened())?;
                paths.push((
                    path.to_path_buf(),
                    source_file.opened().clone(),
                    observation,
                ));
                Ok(())
            };
        let traversal = if self.provider == CaptureProvider::Tabnine {
            super::super::traversal::visit_jsonl_tree_files_isolating_selected(
                self.provider,
                root,
                &mut visit,
                &mut |path, error| {
                    failures.push(DirectJsonlInventoryFailure {
                        path: path.to_path_buf(),
                        detail: error.to_string(),
                    });
                    Ok(())
                },
            )
        } else {
            super::super::traversal::visit_jsonl_tree_files(self.provider, root, &mut visit)
        };
        match traversal {
            Ok(_) => {}
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return self.missing_inventory(root);
            }
            Err(error) => return Err(error.into()),
        }

        let canonical_root = root.to_path_buf();
        paths.sort_by(|left, right| path_key(&left.0).cmp(&path_key(&right.0)));
        let leaves = paths
            .into_iter()
            .map(
                |(path, source_file, observation)| DirectJsonlInventoryLeaf {
                    provider: self.provider,
                    source_format: self.source_format,
                    source_root: canonical_root.clone(),
                    route_key: relative_route_key(&canonical_root, &path),
                    path,
                    source_file,
                    observation,
                },
            )
            .collect::<Vec<_>>();
        let observation = inventory_observation(self, &canonical_root, false, &leaves, &failures)?;
        Ok(DirectJsonlSourceInventory {
            adapter: self,
            observation,
            root_missing: false,
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
            leaves: Vec::new(),
            failures: Vec::new(),
        })
    }

    pub(crate) fn open_leaf(
        self,
        leaf: &DirectJsonlInventoryLeaf,
        imported_at: DateTime<Utc>,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlSourceReader> {
        if leaf.provider != self.provider || leaf.source_format != self.source_format {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let reader = super::reader::open_direct_jsonl_pages_from_opened(
            self.provider,
            self.source_format,
            &leaf.path,
            Some(leaf.source_root.clone()),
            imported_at,
            None,
            leaf.source_file.clone(),
        )?;
        if reader.observation() != &leaf.observation
            || reader.source_change() != DirectJsonlSourceChange::Fresh
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(DirectJsonlSourceReader {
            adapter: self,
            leaf: leaf.clone(),
            reader,
            source: None,
            session_id: None,
            native_session_id: None,
            retained_records: 0,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: 0,
            last_checkpoint: None,
            exhausted: false,
        })
    }

    pub(crate) fn hydrate(
        self,
        leaf: &DirectJsonlCertifiedLeaf,
        locator: &SourceRecordLocator,
    ) -> DirectJsonlSourceBackedResult<Vec<u8>> {
        locator.validate_contract()?;
        if leaf.adapter != self
            || !leaf.source.exact_descriptor_eq(locator.source())
            || locator.source().provider() != self.provider.as_str()
            || locator.source().source_format() != self.source_format
            || locator.source().schema_variant() != self.schema_variant
        {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let NativeRecordCoordinate::Jsonl {
            byte_offset,
            byte_length,
            native_session_key,
            native_event_key,
            ..
        } = locator.coordinate()
        else {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        };
        if native_session_key.as_ref() != Some(&TypedKey::Utf8(leaf.native_session_id.clone())) {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let Some(TypedKey::Composite(event_key)) = native_event_key else {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        };
        let Some(TypedKey::U64(sub_ordinal)) = event_key.get(1) else {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        };
        let sub_ordinal = u32::try_from(*sub_ordinal)
            .map_err(|_| DirectJsonlSourceBackedError::InvalidLocator)?;
        if *byte_length == 0
            || *byte_length > (MAX_PROVIDER_JSONL_LINE_BYTES as u64).saturating_add(2)
        {
            return Err(DirectJsonlSourceBackedError::LocatorRangeTooLarge);
        }
        let range_end = byte_offset
            .checked_add(*byte_length)
            .ok_or(DirectJsonlSourceBackedError::LocatorRangeTooLarge)?;
        if leaf.leaf.source_file.len() < range_end {
            return Err(DirectJsonlSourceBackedError::LocatorRangeMissing);
        }
        let length = usize::try_from(*byte_length)
            .map_err(|_| DirectJsonlSourceBackedError::LocatorRangeTooLarge)?;
        let bytes = leaf
            .leaf
            .source_file
            .read_exact_range(
                *byte_offset,
                length,
                MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
            )
            .map_err(|error| match error {
                CaptureError::InvalidPayload(_) => {
                    DirectJsonlSourceBackedError::LocatorRangeMissing
                }
                other => DirectJsonlSourceBackedError::Capture(other),
            })?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if &digest != locator.record_digest() {
            return Err(DirectJsonlSourceBackedError::LocatorDigestMismatch);
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let display_text = hydrated_direct_jsonl_lexical_text(self.provider, &value, sub_ordinal)?
            .ok_or(DirectJsonlSourceBackedError::LocatorRecordNotRetained)?;
        Ok(display_text.into_bytes())
    }

    fn source_key(self, native_session_id: &str) -> DirectJsonlSourceBackedResult<SourceKey> {
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
    adapter: DirectJsonlSourceAdapter,
    observation: SourceInventoryObservation,
    root_missing: bool,
    leaves: Vec<DirectJsonlInventoryLeaf>,
    failures: Vec<DirectJsonlInventoryFailure>,
}

impl DirectJsonlSourceInventory {
    pub(crate) fn observation(&self) -> &SourceInventoryObservation {
        &self.observation
    }

    pub(crate) fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub(crate) fn leaves(&self) -> &[DirectJsonlInventoryLeaf] {
        &self.leaves
    }

    pub(crate) fn failures(&self) -> &[DirectJsonlInventoryFailure] {
        &self.failures
    }

    pub(crate) fn certify_against(
        &self,
        closing: &Self,
        sources: Vec<SourceKey>,
    ) -> DirectJsonlSourceBackedResult<CertifiedSourceInventory> {
        if self.adapter != closing.adapter
            || self.root_missing
            || closing.root_missing
            || !self.failures.is_empty()
            || !closing.failures.is_empty()
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
pub(crate) struct DirectJsonlInventoryFailure {
    pub(crate) path: PathBuf,
    pub(crate) detail: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DirectJsonlInventoryLeaf {
    provider: CaptureProvider,
    source_format: &'static str,
    source_root: PathBuf,
    route_key: Vec<u8>,
    path: PathBuf,
    source_file: Arc<OpenedProviderSourceFile>,
    observation: DirectJsonlFileObservation,
}

impl DirectJsonlInventoryLeaf {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn observation(&self) -> &DirectJsonlFileObservation {
        &self.observation
    }
}

#[derive(Debug)]
pub(crate) struct DirectJsonlSourcePage {
    pub(crate) source: SourceKey,
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) terminal: bool,
}

pub(crate) struct DirectJsonlSourceReader {
    adapter: DirectJsonlSourceAdapter,
    leaf: DirectJsonlInventoryLeaf,
    reader: super::reader::DirectJsonlPageReader,
    source: Option<SourceKey>,
    session_id: Option<StableEntityId>,
    native_session_id: Option<String>,
    retained_records: u64,
    rejected_records: u64,
    ignored_records: u64,
    indexed_documents: u64,
    last_checkpoint: Option<DirectJsonlCheckpoint>,
    exhausted: bool,
}

impl DirectJsonlSourceReader {
    pub(crate) fn next_page(
        &mut self,
    ) -> DirectJsonlSourceBackedResult<Option<DirectJsonlSourcePage>> {
        if self.exhausted {
            return Ok(None);
        }
        loop {
            let Some(page) = self.reader.next_page()? else {
                self.exhausted = true;
                if let Some(outcome) = self.reader.outcome() {
                    self.last_checkpoint = Some(outcome.checkpoint.clone());
                }
                return Ok(None);
            };
            if !page.rejections.is_empty() {
                return Err(DirectJsonlSourceBackedError::RejectedSource {
                    path: self.leaf.path.clone(),
                    rejected: page.rejections.len(),
                });
            }
            self.bind_session(&page.next_checkpoint)?;
            let represented_records = page
                .events
                .iter()
                .map(|event| event.raw_ordinal)
                .collect::<BTreeSet<_>>();
            let raw_records = page
                .next_checkpoint
                .next_raw_ordinal
                .checked_sub(page.expected_checkpoint.next_raw_ordinal)
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
            let represented = u64::try_from(represented_records.len())
                .map_err(|_| DirectJsonlSourceBackedError::CountMismatch)?;
            self.ignored_records = self
                .ignored_records
                .checked_add(
                    raw_records
                        .checked_sub(represented)
                        .ok_or(DirectJsonlSourceBackedError::CountMismatch)?,
                )
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
            if self.source.is_none() && page.events.is_empty() {
                self.last_checkpoint = Some(page.next_checkpoint);
                continue;
            }
            let source = self
                .source
                .as_ref()
                .ok_or_else(|| {
                    DirectJsonlSourceBackedError::MissingNativeSession(self.leaf.path.clone())
                })?
                .clone();
            let session_id = self
                .session_id
                .ok_or(DirectJsonlSourceBackedError::NativeSessionChanged)?;
            let native_session_id = self
                .native_session_id
                .as_deref()
                .ok_or(DirectJsonlSourceBackedError::NativeSessionChanged)?;
            let session = page
                .next_checkpoint
                .session
                .as_ref()
                .ok_or(DirectJsonlSourceBackedError::NativeSessionChanged)?;
            let source_path = self.leaf.path.to_str();
            let mut documents = Vec::with_capacity(page.events.len());
            for event in page.events {
                documents.push(project_event(
                    self.adapter,
                    &source,
                    session_id,
                    native_session_id,
                    session,
                    source_path,
                    event,
                )?);
            }
            self.retained_records = self
                .retained_records
                .checked_add(documents.len() as u64)
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
            self.indexed_documents = self
                .indexed_documents
                .checked_add(documents.len() as u64)
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
            self.last_checkpoint = Some(page.next_checkpoint);
            return Ok(Some(DirectJsonlSourcePage {
                source,
                documents,
                terminal: page.terminal,
            }));
        }
    }

    pub(crate) fn finish(self) -> DirectJsonlSourceBackedResult<DirectJsonlCertifiedLeaf> {
        if !self.exhausted || self.reader.outcome().is_none() {
            return Err(DirectJsonlSourceBackedError::IncompleteScan);
        }
        let outcome = self
            .reader
            .outcome()
            .ok_or(DirectJsonlSourceBackedError::IncompleteScan)?;
        let checkpoint = self
            .last_checkpoint
            .as_ref()
            .ok_or(DirectJsonlSourceBackedError::IncompleteScan)?;
        let source = self.source.ok_or_else(|| {
            DirectJsonlSourceBackedError::MissingNativeSession(self.leaf.path.clone())
        })?;
        let native_session_id = self.native_session_id.ok_or_else(|| {
            DirectJsonlSourceBackedError::MissingNativeSession(self.leaf.path.clone())
        })?;
        if checkpoint.accepted_events != self.retained_records
            || checkpoint.rejected_records != self.rejected_records
        {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        let complete_records = self
            .retained_records
            .checked_add(self.rejected_records)
            .and_then(|value| value.checked_add(self.ignored_records))
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        let closing = super::reader::observe_opened_file(&self.leaf.source_file)?;
        self.leaf.source_file.revalidate()?;
        if closing != self.leaf.observation {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let opening = source_observation(&source, &self.leaf.observation)?;
        let closing = source_observation(&source, &closing)?;
        let frontier = SourceFrontier::new(
            DIRECT_JSONL_SOURCE_FRONTIER_KIND,
            TypedKey::bytes(serde_json::to_vec(checkpoint)?)?,
            checkpoint.complete_prefix_end,
            checkpoint.complete_prefix_sha256,
        )?;
        let certificate = CertifiedSource::certify_with_frontier(
            opening,
            closing,
            DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION,
            checkpoint.complete_prefix_sha256,
            ScannedSourceCounts {
                complete_records,
                retained_records: self.retained_records,
                rejected_records: self.rejected_records,
                ignored_records: self.ignored_records,
                indexed_documents: self.indexed_documents,
                certified_bytes: checkpoint.complete_prefix_end,
            },
            Some(frontier),
        )?;
        if outcome.source_sha256 != checkpoint.complete_prefix_sha256 {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        Ok(DirectJsonlCertifiedLeaf {
            adapter: self.adapter,
            leaf: self.leaf,
            source,
            native_session_id,
            certificate,
            terminal: checkpoint.terminal,
        })
    }

    fn bind_session(
        &mut self,
        checkpoint: &DirectJsonlCheckpoint,
    ) -> DirectJsonlSourceBackedResult<()> {
        let Some(session) = checkpoint.session.as_ref() else {
            return Ok(());
        };
        if let Some(existing) = self.native_session_id.as_deref() {
            if existing != session.native_session_id {
                return Err(DirectJsonlSourceBackedError::NativeSessionChanged);
            }
            return Ok(());
        }
        let (source, session_id) =
            direct_jsonl_session_identity(self.adapter, &session.native_session_id)?;
        self.native_session_id = Some(session.native_session_id.clone());
        self.session_id = Some(session_id);
        self.source = Some(source);
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct DirectJsonlCertifiedLeaf {
    adapter: DirectJsonlSourceAdapter,
    leaf: DirectJsonlInventoryLeaf,
    source: SourceKey,
    native_session_id: String,
    certificate: CertifiedSource,
    terminal: bool,
}

impl DirectJsonlCertifiedLeaf {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }

    pub(crate) fn terminal(&self) -> bool {
        self.terminal
    }
}

fn project_event(
    adapter: DirectJsonlSourceAdapter,
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    session: &DirectJsonlSession,
    source_path: Option<&str>,
    event: DirectJsonlEvent,
) -> DirectJsonlSourceBackedResult<LexicalDocument> {
    let evidence = &event.source_record;
    let byte_length = evidence
        .byte_end_exclusive
        .checked_sub(evidence.byte_start)
        .ok_or(DirectJsonlSourceBackedError::MissingRecordEvidence)?;
    if byte_length == 0 {
        return Err(DirectJsonlSourceBackedError::MissingRecordEvidence);
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
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: evidence.byte_start,
            byte_length,
            physical_ordinal: event.raw_ordinal,
            native_session_key: Some(TypedKey::utf8(native_session_id)?),
            native_event_key: Some(native_event_key),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        evidence.record_digest,
    )?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| direct_jsonl_session_identity(adapter, parent).map(|(_, id)| id))
        .transpose()?;
    let root_session_id = match session.root_provider_session_id.as_deref() {
        Some(root) if root == session.native_session_id || root == session.provider_session_id => {
            session_id
        }
        Some(root) => direct_jsonl_session_identity(adapter, root)?.1,
        None => session_id,
    };
    let body = if event.lexical_text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        event.lexical_text.clone()
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
        source_path: source_path.map(str::to_owned),
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

fn direct_jsonl_session_identity(
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

fn source_observation(
    source: &SourceKey,
    observation: &DirectJsonlFileObservation,
) -> DirectJsonlSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        "direct-native-jsonl-file-observation-v1",
        serde_json::to_vec(observation)?,
    )?)
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

#[cfg(test)]
pub(super) fn assert_source_backed_fixture(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    expected_native_session_id: &str,
    expected_body: &str,
    expected_record: &[u8],
    expected_parent_provider_session_id: Option<&str>,
    expected_root_provider_session_id: &str,
    expected_agent_type: &str,
    expected_is_primary: bool,
) {
    use ctx_history_core::NativeRecordCoordinate;

    let opening = adapter.discover(root).unwrap();
    assert!(!opening.root_missing());
    assert!(opening.failures().is_empty());
    assert_eq!(opening.leaves().len(), 1);
    let leaf = opening.leaves()[0].clone();
    let (documents, certified) = collect_test_leaf(adapter, &leaf);
    assert!(certified.terminal());
    assert_eq!(
        certified.certificate().counts().indexed_documents,
        documents.len() as u64
    );
    assert_eq!(
        certified.certificate().counts().certified_bytes,
        leaf.source_file.len()
    );
    assert!(certified.certificate().frontier().is_some());
    let document = documents
        .iter()
        .find(|document| document.body.contains(expected_body))
        .unwrap();
    assert_eq!(
        document.provider_session_id.as_deref(),
        Some(expected_native_session_id)
    );
    let expected_parent_session_id = expected_parent_provider_session_id
        .map(|parent| direct_jsonl_session_identity(adapter, parent).unwrap().1);
    let expected_root_session_id =
        direct_jsonl_session_identity(adapter, expected_root_provider_session_id)
            .unwrap()
            .1;
    assert_eq!(document.parent_session_id, expected_parent_session_id);
    assert_eq!(document.root_session_id, expected_root_session_id);
    assert_eq!(document.agent_type, expected_agent_type);
    assert_eq!(document.is_primary, expected_is_primary);
    assert_eq!(document.branch, None);
    assert_eq!(document.source_path.as_deref(), leaf.path().to_str());
    let NativeRecordCoordinate::Jsonl {
        byte_length,
        native_session_key,
        native_event_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("source-backed fixture did not emit a typed JSONL locator");
    };
    assert_eq!(*byte_length as usize, expected_record.len());
    assert_eq!(
        native_session_key.as_ref(),
        Some(&TypedKey::Utf8(expected_native_session_id.to_owned()))
    );
    assert!(native_event_key.is_some());
    assert_eq!(
        adapter.hydrate(&certified, &document.locator).unwrap(),
        document.body.as_bytes()
    );

    let closing = adapter.discover(root).unwrap();
    let inventory = opening
        .certify_against(&closing, vec![certified.source().clone()])
        .unwrap();
    assert!(inventory.contains(certified.source()));

    let (replayed_documents, replayed) = collect_test_leaf(adapter, &closing.leaves()[0]);
    assert_eq!(
        documents
            .iter()
            .map(|document| (document.event_id, document.session_id))
            .collect::<Vec<_>>(),
        replayed_documents
            .iter()
            .map(|document| (document.event_id, document.session_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(certified.source(), replayed.source());
}

#[cfg(test)]
fn collect_test_leaf(
    adapter: DirectJsonlSourceAdapter,
    leaf: &DirectJsonlInventoryLeaf,
) -> (Vec<LexicalDocument>, DirectJsonlCertifiedLeaf) {
    let mut reader = adapter
        .open_leaf(leaf, "2026-07-28T12:00:00Z".parse().unwrap())
        .unwrap();
    let mut documents = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.documents.len() <= 64);
        assert_eq!(page.source.provider(), adapter.provider().as_str());
        documents.extend(page.documents);
    }
    (documents, reader.finish().unwrap())
}

#[cfg(test)]
mod architecture_tests {
    #[test]
    fn owned_source_backed_constructors_have_no_preview_body_or_store_fallback() {
        let sources = [
            (
                "mistral_vibe",
                include_str!("../../mistral_vibe/native_path/source_backed.rs"),
            ),
            (
                "nanoclaw",
                include_str!("../../nanoclaw/native_path/source_backed.rs"),
            ),
            ("native_jsonl", include_str!("source_backed.rs")),
            (
                "openclaw",
                include_str!("../../openclaw/native_path/source_backed.rs"),
            ),
            (
                "opencode",
                include_str!("../../opencode/native_path/source_backed.rs"),
            ),
            (
                "openhands",
                include_str!("../../openhands/nativepath/source_backed.rs"),
            ),
            ("pi", include_str!("../../pi/nativepath/source_backed.rs")),
            (
                "rovodev",
                include_str!("../../rovodev/native_path/source_backed.rs"),
            ),
            (
                "shelley",
                include_str!("../../shelley/native_path/source_backed.rs"),
            ),
            (
                "task_json",
                include_str!("../../task_json/cline_nativepath/source_backed.rs"),
            ),
            (
                "trae",
                include_str!("../../trae/nativepath/source_backed.rs"),
            ),
            ("warp", include_str!("../../warp/source_backed.rs")),
            (
                "zed",
                include_str!("../../zed/native_path/source_backed.rs"),
            ),
        ];
        let forbidden = [
            concat!("MAX_BODY_", "PREVIEW_CHARS"),
            concat!("DIRECT_JSONL_LEXICAL_", "PREVIEW_CHARS"),
            concat!("MAX_LEXICAL_", "PREVIEW_CHARS"),
            concat!("bounded_", "lexical_body"),
            concat!("bounded_", "body"),
            concat!("lexical_", "preview"),
            concat!("body: event.", "preview"),
            concat!("ctx_history_", "store"),
            concat!("Store", "::"),
        ];

        for (provider, source) in sources {
            let has_body_assignment = source.lines().any(|line| {
                let line = line.trim();
                line == "body," || line.starts_with("body:")
            });
            assert!(
                source.contains("LexicalDocument {") && has_body_assignment,
                "{provider} no longer exposes an auditable LexicalDocument body assignment"
            );
            for token in forbidden {
                assert!(
                    !source.contains(token),
                    "{provider} source-backed path contains forbidden architecture token {token}"
                );
            }
        }

        assert!(sources
            .iter()
            .find(|(provider, _)| *provider == "warp")
            .unwrap()
            .1
            .contains("event.lexical_body"));
        assert!(sources
            .iter()
            .find(|(provider, _)| *provider == "zed")
            .unwrap()
            .1
            .contains("body: event.lexical_body"));
    }
}

#[cfg(all(test, unix))]
mod authority_swap_tests {
    use std::fs;

    use super::*;

    fn discovered_leaf() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        DirectJsonlInventoryLeaf,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let ancestor = temp.path().join("authority");
        let root = ancestor.join("transcripts");
        let leaf = root.join("session.jsonl");
        fs::create_dir_all(&root).unwrap();
        fs::write(&leaf, b"{\"type\":\"message\"}\n").unwrap();
        let adapter = DirectJsonlSourceAdapter::new(
            CaptureProvider::Windsurf,
            "windsurf_hook_transcript_jsonl",
            "windsurf-hook-jsonl-v1",
        );
        let inventory = adapter.discover(&root).unwrap();
        assert_eq!(inventory.leaves.len(), 1);
        let retained = inventory.leaves[0].clone();
        (temp, ancestor, root, retained)
    }

    #[test]
    fn shared_native_jsonl_rejects_root_swap_after_discovery() {
        let (_temp, _ancestor, root, retained) = discovered_leaf();
        let displaced = root.with_file_name("transcripts-displaced");
        fs::rename(&root, &displaced).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("session.jsonl"), b"{\"replacement\":true}\n").unwrap();

        assert!(retained.source_file.revalidate().is_err());
    }

    #[test]
    fn shared_native_jsonl_rejects_ancestor_swap_after_discovery() {
        let (temp, ancestor, root, retained) = discovered_leaf();
        let displaced = temp.path().join("authority-displaced");
        fs::rename(&ancestor, &displaced).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("session.jsonl"), b"{\"replacement\":true}\n").unwrap();

        assert!(retained.source_file.revalidate().is_err());
    }

    #[test]
    fn shared_native_jsonl_rejects_leaf_swap_after_discovery() {
        let (_temp, _ancestor, root, retained) = discovered_leaf();
        let leaf = root.join("session.jsonl");
        fs::rename(&leaf, root.join("session-displaced.jsonl")).unwrap();
        fs::write(&leaf, b"{\"replacement\":true}\n").unwrap();

        assert!(retained.source_file.revalidate().is_err());
    }
}

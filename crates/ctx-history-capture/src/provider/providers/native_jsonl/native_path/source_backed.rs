use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
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
    open_direct_jsonl_pages, DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlFileObservation,
    DirectJsonlSourceChange,
};
use crate::{
    provider::normalization::provider_local_preview, CaptureError, ProviderJsonlInventoryLimit,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

const DIRECT_JSONL_SOURCE_IDENTITY_VERSION: u32 = 1;
const DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION: &str = "direct-native-jsonl-source-backed-v1";
const DIRECT_JSONL_SOURCE_FRONTIER_KIND: &str = "direct-native-jsonl-checkpoint-v1";
const DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE: &str = "direct-native-jsonl-provider-root-v1";
const DIRECT_JSONL_INVENTORY_REVISION_KIND: &str = "direct-native-jsonl-inventory-sha256-v1";
const DIRECT_JSONL_DISCOVERY_REVISION: &str = "direct-native-jsonl-discovery-v1";
const DIRECT_JSONL_LEXICAL_PREVIEW_CHARS: usize = 2_048;
const DIRECT_JSONL_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;
const DIRECT_JSONL_MAX_TOUCHED_FILES: usize = 256;

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
        let root_metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if root_metadata.is_none() {
            return self.missing_inventory(root);
        }

        let mut paths = Vec::new();
        let mut failures = Vec::new();
        let mut aggregate_path_bytes = 0_usize;
        let mut visit = |path: &Path| -> crate::Result<()> {
            if paths.len() == PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS {
                return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                    limit: ProviderJsonlInventoryLimit::EligiblePaths,
                    maximum: PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
                    observed: paths.len().saturating_add(1),
                });
            }
            let canonical_path = fs::canonicalize(path)?;
            aggregate_path_bytes =
                aggregate_path_bytes.saturating_add(path_key(&canonical_path).len());
            if aggregate_path_bytes > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
                return Err(CaptureError::InvalidPayload(
                    "direct JSONL inventory paths exceed the aggregate byte limit".to_owned(),
                ));
            }
            let observation = super::reader::observe_file(&canonical_path)?;
            paths.push((canonical_path, observation));
            Ok(())
        };
        let selected =
            |path: &Path| super::super::dialect::native_jsonl_file_is_selected(self.provider, path);
        if self.provider == CaptureProvider::Tabnine {
            super::super::traversal::visit_jsonl_tree_files_isolating_selected(
                root,
                &selected,
                &mut visit,
                &mut |path, error| {
                    failures.push(DirectJsonlInventoryFailure {
                        path: path.to_path_buf(),
                        detail: error.to_string(),
                    });
                    Ok(())
                },
            )?;
        } else {
            super::super::traversal::visit_jsonl_tree_files(root, &selected, &mut visit)?;
        }

        let canonical_root = fs::canonicalize(root)?;
        paths.sort_by(|left, right| path_key(&left.0).cmp(&path_key(&right.0)));
        let leaves = paths
            .into_iter()
            .map(|(path, observation)| DirectJsonlInventoryLeaf {
                provider: self.provider,
                source_format: self.source_format,
                source_root: canonical_root.clone(),
                route_key: relative_route_key(&canonical_root, &path),
                path,
                observation,
            })
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
        let reader = open_direct_jsonl_pages(
            self.provider,
            self.source_format,
            &leaf.path,
            Some(leaf.source_root.clone()),
            imported_at,
            false,
            None,
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
            ..
        } = locator.coordinate()
        else {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        };
        if native_session_key.as_ref() != Some(&TypedKey::Utf8(leaf.native_session_id.clone())) {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        if *byte_length == 0
            || *byte_length > (MAX_PROVIDER_JSONL_LINE_BYTES as u64).saturating_add(2)
        {
            return Err(DirectJsonlSourceBackedError::LocatorRangeTooLarge);
        }
        let range_end = byte_offset
            .checked_add(*byte_length)
            .ok_or(DirectJsonlSourceBackedError::LocatorRangeTooLarge)?;
        crate::common::io::ensure_regular_provider_transcript_file(&leaf.leaf.path)?;
        let mut file = File::open(&leaf.leaf.path)?;
        if file.metadata()?.len() < range_end {
            return Err(DirectJsonlSourceBackedError::LocatorRangeMissing);
        }
        file.seek(SeekFrom::Start(*byte_offset))?;
        let length = usize::try_from(*byte_length)
            .map_err(|_| DirectJsonlSourceBackedError::LocatorRangeTooLarge)?;
        let mut bytes = vec![0_u8; length];
        file.read_exact(&mut bytes)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if &digest != locator.record_digest() {
            return Err(DirectJsonlSourceBackedError::LocatorDigestMismatch);
        }
        Ok(bytes)
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
    reader: super::DirectJsonlPageReader,
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
            let cwd = page
                .next_checkpoint
                .session
                .as_ref()
                .and_then(|session| session.cwd.as_deref());
            let mut documents = Vec::with_capacity(page.events.len());
            for event in page.events {
                documents.push(project_event(
                    self.adapter,
                    &source,
                    session_id,
                    native_session_id,
                    cwd,
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
        let closing = super::reader::observe_file(&self.leaf.path)?;
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
        let source = self.adapter.source_key(&session.native_session_id)?;
        let native_session_key = NativeSessionKey::native_id(
            format!("{}.direct-jsonl-session", self.adapter.provider.as_str()),
            TypedKey::utf8(&session.native_session_id)?,
        )?;
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "direct-jsonl-session",
            native_session_key: &native_session_key,
        })?;
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
    cwd: Option<&str>,
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
    let body = lexical_body(&event);
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
        source: source.clone(),
        locator,
        provider_session_id: Some(native_session_id.to_owned()),
        event_sequence: event.provider_event_sequence_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: Some(event.role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: cwd.map(str::to_owned),
        touched_files,
    })
}

fn lexical_body(event: &DirectJsonlEvent) -> String {
    let candidate = event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            event.payload.get("body").and_then(|body| {
                (!body.is_null())
                    .then(|| serde_json::to_string(body).ok())
                    .flatten()
            })
        })
        .unwrap_or_else(|| {
            let tool_name = event
                .payload
                .get("tool_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(event.event_type.as_str());
            let outcome = event
                .payload
                .get("result_outcome")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("event");
            format!("{tool_name} {outcome}")
        });
    let (body, _) = provider_local_preview(&candidate, DIRECT_JSONL_LEXICAL_PREVIEW_CHARS);
    if body.is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        body
    }
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
        fs::metadata(leaf.path()).unwrap().len()
    );
    assert!(certified.certificate().frontier().is_some());
    let document = documents
        .iter()
        .find(|document| document.body.contains(expected_body))
        .unwrap();
    assert!(document.body.chars().count() <= DIRECT_JSONL_LEXICAL_PREVIEW_CHARS);
    assert_eq!(
        document.provider_session_id.as_deref(),
        Some(expected_native_session_id)
    );
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
        expected_record
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

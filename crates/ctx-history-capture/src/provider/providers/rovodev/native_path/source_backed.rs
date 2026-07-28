//! Thin source-backed projection adapter for Rovo Dev session trees.
//!
//! The existing provider discovery, bounded document parser, and verified
//! structured-content route remain authoritative. Shared code owns lifecycle
//! admission, projection publication, and deletion.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    discover_rovodev_session_sources, failure, prepare_document, prepare_page, RovoDevDiscovery,
    RovoDevFailure, RovoDevSessionObservation, RovoDevSessionSource,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::normalization::{provider_block_text, provider_message_id, provider_string_field},
    CaptureError, ProviderAdapterContext, MAX_PROVIDER_JSONL_LINE_BYTES, ROVODEV_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "rovodev.session";
const SESSION_KEY_NAMESPACE: &str = "rovodev.session";
const EVENT_KEY_NAMESPACE: &str = "rovodev.message";
const EVENT_POSITION_KIND: &str = "rovodev.message-object";
const LOGICAL_SESSION_KIND: &str = "rovodev-session";
const LOGICAL_EVENT_KIND: &str = "rovodev-event";
const SOURCE_SCHEMA_VARIANT: &str = "rovodev-session-json-tree-v1";
const SOURCE_REVISION_KIND: &str = "rovodev-session-tree-revision-v1";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "rovodev.sessions-root";
const INVENTORY_REVISION_KIND: &str = "rovodev-sessions-inventory-v1";
const INVENTORY_DISCOVERY_REVISION: &str = "rovodev-sessions-discovery-v1";
const FRONTIER_KIND: &str = "rovodev-document-frontier-v1";
const PARSER_REVISION: &str = "rovodev-source-backed-v1";
const RELATIVE_CONTEXT_FILE: &str = "session_context.json";
const MESSAGE_OBJECT_KIND: &str = "message_history";
const FILE_HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub(crate) enum RovoDevSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Rovo Dev source-backed discovery requires an authoritative sessions directory")]
    NonAuthoritativeRoot,
    #[error("Rovo Dev authoritative inventory contains duplicate session identity {0:?}")]
    DuplicateSession(String),
    #[error("Rovo Dev session lineage contains a cycle at provider thread {0:?}")]
    LineageCycle(String),
    #[error("Rovo Dev source-backed scan was not drained to a terminal frontier")]
    IncompleteScan,
    #[error("Rovo Dev source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("Rovo Dev source-backed event coordinate exceeds its supported range")]
    CoordinateOverflow,
    #[error("locator is not a Rovo Dev session-tree record")]
    InvalidLocator,
    #[error("Rovo Dev locator source is absent from the authoritative sessions inventory")]
    LocatorSourceMissing,
    #[error("Rovo Dev locator source revision no longer matches provider bytes")]
    LocatorSourceChanged,
    #[error("Rovo Dev locator object coordinate no longer matches the provider document")]
    LocatorObjectChanged,
}

pub(crate) type RovoDevSourceBackedResult<T> = Result<T, RovoDevSourceBackedError>;

#[derive(Debug)]
struct FileSnapshot {
    bytes: Option<Vec<u8>>,
    byte_len: u64,
    sha256: [u8; 32],
}

impl FileSnapshot {
    fn read(
        source: &OpenedProviderSourceFile,
        byte_len: u64,
        retain_bytes: bool,
    ) -> RovoDevSourceBackedResult<Self> {
        if retain_bytes {
            let bytes = source.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
            if u64::try_from(bytes.len()).ok() != Some(byte_len) {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            let sha256 = Sha256::digest(&bytes).into();
            return Ok(Self {
                bytes: Some(bytes),
                byte_len,
                sha256,
            });
        }

        let mut file = source.file().try_clone()?;
        let mut digest = Sha256::new();
        let mut observed = 0_u64;
        let mut buffer = [0_u8; FILE_HASH_BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            observed = observed
                .checked_add(
                    u64::try_from(read).map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
                )
                .ok_or(RovoDevSourceBackedError::CountMismatch)?;
            digest.update(&buffer[..read]);
        }
        if observed != byte_len {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(Self {
            bytes: None,
            byte_len,
            sha256: digest.finalize().into(),
        })
    }
}

#[derive(Debug)]
struct RovoDevSnapshot {
    frozen: RovoDevSessionObservation,
    context_bytes: Option<Vec<u8>>,
    context_sha256: [u8; 32],
    source_sha256: [u8; 32],
    certified_bytes: u64,
    document: std::result::Result<super::PreparedDocument, RovoDevFailure>,
    context_file: OpenedProviderSourceFile,
    metadata_file: Option<OpenedProviderSourceFile>,
}

impl RovoDevSnapshot {
    fn read(
        source: &RovoDevSessionSource,
        context: &ProviderAdapterContext,
        authority: &ProviderSourceRoot,
        session_relative_path: &Path,
        context_relative_path: &Path,
        expected_metadata: Option<&Path>,
    ) -> RovoDevSourceBackedResult<Self> {
        let context_handle = authority.open_file(context_relative_path)?;
        let metadata_relative_path = session_relative_path.join("metadata.json");
        let metadata_handle = match authority.open_file(&metadata_relative_path) {
            Ok(file) => Some(file),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if metadata_handle.is_some() != expected_metadata.is_some() {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let frozen = RovoDevSessionObservation::from_admitted(
            authority.named_path().join(context_relative_path),
            source.context_path.clone(),
            context_handle.metadata(),
            metadata_handle
                .as_ref()
                .map(|metadata| (source.session_dir.join("metadata.json"), metadata.metadata())),
        )?;
        let context_oversized = frozen.context_length() > MAX_PROVIDER_JSONL_LINE_BYTES as u64;
        let context_file = FileSnapshot::read(
            &context_handle,
            frozen.context_length(),
            !context_oversized,
        )?;
        let metadata_oversized = frozen
            .metadata_length()
            .is_some_and(|length| length > MAX_PROVIDER_JSONL_LINE_BYTES as u64);
        let metadata_file = match (metadata_handle.as_ref(), frozen.metadata_length()) {
            (Some(file), Some(length)) => {
                Some(FileSnapshot::read(file, length, !metadata_oversized)?)
            }
            (None, None) => None,
            _ => return Err(CaptureError::SourceChangedDuringCapture.into()),
        };
        let certified_bytes = metadata_file
            .as_ref()
            .map_or(Some(context_file.byte_len), |metadata| {
                context_file.byte_len.checked_add(metadata.byte_len)
            })
            .ok_or(RovoDevSourceBackedError::CountMismatch)?;
        let source_sha256 = compound_source_digest(&context_file, metadata_file.as_ref());
        let document = if context_oversized {
            Err(failure(
                1,
                format!(
                    "Rovo Dev session_context.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                ),
            ))
        } else {
            prepare_document(
                source,
                context,
                context_file.bytes.as_deref().unwrap_or_default(),
                metadata_file.as_ref().and_then(|file| file.bytes.as_deref()),
                metadata_oversized.then(|| {
                    failure(
                        1,
                        format!(
                            "Rovo Dev metadata.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                        ),
                    )
                }),
            )
        };
        Ok(Self {
            frozen,
            context_bytes: context_file.bytes,
            context_sha256: context_file.sha256,
            source_sha256,
            certified_bytes,
            document,
            context_file: context_handle,
            metadata_file: metadata_handle,
        })
    }

    fn revalidate(&self, authority: &ProviderSourceRoot) -> RovoDevSourceBackedResult<()> {
        self.context_file.revalidate()?;
        if let Some(metadata) = &self.metadata_file {
            metadata.revalidate()?;
        }
        authority.revalidate()?;
        Ok(())
    }

    fn observation(&self, source_key: SourceKey) -> RovoDevSourceBackedResult<SourceObservation> {
        let mut revision = Vec::with_capacity(64);
        revision.extend_from_slice(&self.frozen.revision_authority());
        revision.extend_from_slice(&self.source_sha256);
        Ok(SourceObservation::new(
            source_key,
            SOURCE_REVISION_KIND,
            revision,
        )?)
    }

    fn message_count(&self) -> usize {
        self.document
            .as_ref()
            .map_or(0, |document| document.messages.len())
    }
}

fn compound_source_digest(context: &FileSnapshot, metadata: Option<&FileSnapshot>) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.rovodev.source-backed.compound-v1\0");
    digest.update(context.byte_len.to_be_bytes());
    digest.update(context.sha256);
    match metadata {
        Some(metadata) => {
            digest.update([1]);
            digest.update(metadata.byte_len.to_be_bytes());
            digest.update(metadata.sha256);
        }
        None => digest.update([0]),
    }
    digest.finalize().into()
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedLeaf {
    authority: ProviderSourceRoot,
    source: RovoDevSessionSource,
    session_relative_path: PathBuf,
    context_relative_path: PathBuf,
    metadata_relative_path: Option<PathBuf>,
    source_key: SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    unique_message_ids: HashSet<String>,
    snapshot: RovoDevSnapshot,
}

impl RovoDevSourceBackedLeaf {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }

    pub(crate) fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub(crate) fn provider_session_id(&self) -> &str {
        self.snapshot
            .document
            .as_ref()
            .map_or(self.source.provider_session_id.as_str(), |document| {
                document.provider_session_id.as_str()
            })
    }
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedInventory {
    authority: ProviderSourceRoot,
    context: ProviderAdapterContext,
    opening: SourceInventoryObservation,
    leaves: Vec<RovoDevSourceBackedLeaf>,
}

impl RovoDevSourceBackedInventory {
    pub(crate) fn leaves(&self) -> &[RovoDevSourceBackedLeaf] {
        &self.leaves
    }

    pub(crate) fn certify(&self) -> RovoDevSourceBackedResult<CertifiedSourceInventory> {
        for leaf in &self.leaves {
            leaf.snapshot.revalidate(&self.authority)?;
        }
        self.authority.revalidate()?;
        let closing = bind_inventory(&self.authority, &self.context)?;
        for leaf in &closing.leaves {
            leaf.snapshot.revalidate(&self.authority)?;
        }
        self.authority.revalidate()?;
        Ok(CertifiedSourceInventory::certify(
            self.opening.clone(),
            closing.observation,
            INVENTORY_DISCOVERY_REVISION,
            closing
                .leaves
                .into_iter()
                .map(|leaf| leaf.source_key)
                .collect(),
        )?)
    }
}

pub(crate) fn discover_rovodev_source_backed(
    sessions_root: &Path,
    context: ProviderAdapterContext,
) -> RovoDevSourceBackedResult<RovoDevSourceBackedInventory> {
    authoritative_discovery(sessions_root)?;
    let canonical_root = fs::canonicalize(sessions_root)?;
    let authority = ProviderSourceRoot::open(&canonical_root)?;
    let bound = bind_inventory(&authority, &context)?;
    Ok(RovoDevSourceBackedInventory {
        authority,
        context,
        opening: bound.observation,
        leaves: bound.leaves,
    })
}

struct BoundInventory {
    canonical_root: PathBuf,
    observation: SourceInventoryObservation,
    leaves: Vec<RovoDevSourceBackedLeaf>,
}

fn bind_inventory(
    authority: &ProviderSourceRoot,
    context: &ProviderAdapterContext,
) -> RovoDevSourceBackedResult<BoundInventory> {
    let discovery = authoritative_discovery(authority.named_path())?;
    let canonical_root = authority.named_path().to_path_buf();
    let mut source_ids = HashSet::with_capacity(discovery.sources().len());
    let mut leaves = Vec::with_capacity(discovery.sources().len());
    for source in discovery.sources() {
        let session_relative_path = relative_to_rovodev_authority(authority, &source.session_dir)?;
        let context_relative_path = relative_to_rovodev_authority(authority, &source.context_path)?;
        let metadata_relative_path = source
            .metadata_path
            .as_deref()
            .map(|path| relative_to_rovodev_authority(authority, path))
            .transpose()?;
        let snapshot = RovoDevSnapshot::read(
            source,
            context,
            authority,
            &session_relative_path,
            &context_relative_path,
            metadata_relative_path.as_deref(),
        )?;
        let provider_session_id = snapshot
            .document
            .as_ref()
            .map_or(source.provider_session_id.as_str(), |document| {
                document.provider_session_id.as_str()
            });
        let source_key = rovodev_source_key(provider_session_id)?;
        if !source_ids.insert(source_key.identity().digest()) {
            return Err(RovoDevSourceBackedError::DuplicateSession(
                provider_session_id.to_owned(),
            ));
        }
        let session_id = rovodev_session_identity(&source_key, provider_session_id)?;
        let unique_message_ids = unique_message_ids(&snapshot);
        leaves.push(RovoDevSourceBackedLeaf {
            authority: authority.clone(),
            source: source.clone(),
            session_relative_path,
            context_relative_path,
            metadata_relative_path,
            source_key,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            unique_message_ids,
            snapshot,
        });
    }
    bind_session_lineage(&mut leaves)?;
    let observation = inventory_observation(&canonical_root, &leaves)?;
    Ok(BoundInventory {
        canonical_root,
        observation,
        leaves,
    })
}

fn authoritative_discovery(root: &Path) -> RovoDevSourceBackedResult<RovoDevDiscovery> {
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RovoDevSourceBackedError::NonAuthoritativeRoot
        } else {
            RovoDevSourceBackedError::Io(error)
        }
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RovoDevSourceBackedError::NonAuthoritativeRoot);
    }
    if fs::symlink_metadata(root.join(RELATIVE_CONTEXT_FILE)).is_ok() {
        return Err(RovoDevSourceBackedError::NonAuthoritativeRoot);
    }
    let discovery = discover_rovodev_session_sources(root)?;
    if !discovery.root_exists() {
        return Err(RovoDevSourceBackedError::NonAuthoritativeRoot);
    }
    Ok(discovery)
}

fn relative_to_rovodev_authority(
    authority: &ProviderSourceRoot,
    path: &Path,
) -> RovoDevSourceBackedResult<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    canonical
        .strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| RovoDevSourceBackedError::NonAuthoritativeRoot)
}

fn rovodev_source_key(provider_session_id: &str) -> RovoDevSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::RovoDev.as_str(),
        ROVODEV_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn rovodev_session_identity(
    source_key: &SourceKey,
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<StableEntityId> {
    let session_key =
        NativeSessionKey::native_id(SESSION_KEY_NAMESPACE, TypedKey::utf8(provider_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source: source_key,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?)
}

fn provider_thread_session_identity(
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<StableEntityId> {
    let source_key = rovodev_source_key(provider_session_id)?;
    rovodev_session_identity(&source_key, provider_session_id)
}

fn bind_session_lineage(leaves: &mut [RovoDevSourceBackedLeaf]) -> RovoDevSourceBackedResult<()> {
    let parents = leaves
        .iter()
        .map(|leaf| {
            (
                leaf.provider_session_id().to_owned(),
                leaf.snapshot
                    .document
                    .as_ref()
                    .ok()
                    .and_then(|document| document.parent_provider_session_id.clone()),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut bound = Vec::with_capacity(leaves.len());
    for leaf in leaves.iter() {
        let provider_session_id = leaf.provider_session_id();
        let parent_provider_session_id =
            parents.get(provider_session_id).and_then(Option::as_deref);
        let parent_session_id = parent_provider_session_id
            .map(provider_thread_session_identity)
            .transpose()?;
        let mut root_session_id = leaf.session_id;
        let mut cursor = parent_provider_session_id;
        let mut visited = HashSet::new();
        visited.insert(provider_session_id.to_owned());
        while let Some(ancestor_provider_session_id) = cursor {
            if !visited.insert(ancestor_provider_session_id.to_owned()) {
                return Err(RovoDevSourceBackedError::LineageCycle(
                    ancestor_provider_session_id.to_owned(),
                ));
            }
            root_session_id = provider_thread_session_identity(ancestor_provider_session_id)?;
            cursor = parents
                .get(ancestor_provider_session_id)
                .and_then(Option::as_deref);
        }
        bound.push((parent_session_id, root_session_id));
    }
    for (leaf, (parent_session_id, root_session_id)) in leaves.iter_mut().zip(bound) {
        leaf.parent_session_id = parent_session_id;
        leaf.root_session_id = root_session_id;
    }
    Ok(())
}

fn inventory_observation(
    canonical_root: &Path,
    leaves: &[RovoDevSourceBackedLeaf],
) -> RovoDevSourceBackedResult<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.rovodev.source-backed.inventory-v1\0");
    digest.update(
        u64::try_from(leaves.len())
            .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
            .to_be_bytes(),
    );
    for leaf in leaves {
        let relative = leaf
            .snapshot
            .frozen
            .canonical_path()
            .strip_prefix(canonical_root)
            .map_err(|_| RovoDevSourceBackedError::NonAuthoritativeRoot)?;
        let relative = relative.as_os_str().as_encoded_bytes();
        digest.update(
            u64::try_from(relative.len())
                .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
                .to_be_bytes(),
        );
        digest.update(relative);
        digest.update(leaf.source_key.identity().digest());
        digest.update(leaf.snapshot.frozen.revision_authority());
        digest.update(leaf.snapshot.source_sha256);
    }
    let mut revision = Vec::with_capacity(40);
    revision.extend_from_slice(
        &u64::try_from(leaves.len())
            .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
            .to_be_bytes(),
    );
    revision.extend_from_slice(&digest.finalize());
    Ok(SourceInventoryObservation::new(
        CaptureProvider::RovoDev.as_str(),
        INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(canonical_root.as_os_str().as_encoded_bytes().to_vec())?,
        INVENTORY_REVISION_KIND,
        revision,
    )?)
}

fn unique_message_ids(snapshot: &RovoDevSnapshot) -> HashSet<String> {
    let mut counts = HashMap::<String, usize>::new();
    if let Ok(document) = snapshot.document.as_ref() {
        for message in &document.messages {
            if let Some(native_id) = explicit_message_id(message) {
                let count = counts.entry(native_id.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
        }
    }
    counts
        .into_iter()
        .filter_map(|(native_id, count)| (count == 1).then_some(native_id))
        .collect()
}

fn explicit_message_id(message: &serde_json::Value) -> Option<&str> {
    ["id", "message_id", "messageId", "request_id", "requestId"]
        .into_iter()
        .find_map(|field| message.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RovoDevSourceBackedDisposition {
    Cold,
    Replacement,
    Unchanged,
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) terminal: bool,
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedScan {
    pub(crate) disposition: RovoDevSourceBackedDisposition,
    pub(crate) source: CertifiedSource,
}

pub(crate) struct RovoDevSourceBackedReader<'a> {
    leaf: &'a RovoDevSourceBackedLeaf,
    context: ProviderAdapterContext,
    disposition: RovoDevSourceBackedDisposition,
    previous: Option<CertifiedSource>,
    next_message: usize,
    emitted_failure_document: bool,
    terminal: bool,
    counts: ScannedSourceCounts,
}

impl<'a> RovoDevSourceBackedReader<'a> {
    pub(crate) fn new(
        leaf: &'a RovoDevSourceBackedLeaf,
        context: ProviderAdapterContext,
        previous: Option<&CertifiedSource>,
    ) -> RovoDevSourceBackedResult<Self> {
        let disposition = match previous {
            None => RovoDevSourceBackedDisposition::Cold,
            Some(previous) => {
                previous.validate_contract()?;
                leaf.source_key
                    .validate_exact_descriptor(previous.observation().source())?;
                if previous.parser_revision() == PARSER_REVISION
                    && previous.observation()
                        == &leaf.snapshot.observation(leaf.source_key.clone())?
                    && previous.content_digest() == &leaf.snapshot.source_sha256
                {
                    RovoDevSourceBackedDisposition::Unchanged
                } else {
                    RovoDevSourceBackedDisposition::Replacement
                }
            }
        };
        Ok(Self {
            leaf,
            context,
            disposition,
            previous: previous.cloned(),
            next_message: 0,
            emitted_failure_document: false,
            terminal: disposition == RovoDevSourceBackedDisposition::Unchanged,
            counts: ScannedSourceCounts::default(),
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> RovoDevSourceBackedResult<Option<RovoDevSourceBackedPage>> {
        if self.terminal {
            return Ok(None);
        }
        let document = match self.leaf.snapshot.document.as_ref() {
            Ok(document) => document,
            Err(_) => {
                if self.emitted_failure_document {
                    self.terminal = true;
                    return Ok(None);
                }
                self.emitted_failure_document = true;
                self.terminal = true;
                let page = RovoDevSourceBackedPage {
                    documents: Vec::new(),
                    complete_records: 1,
                    retained_records: 0,
                    rejected_records: 1,
                    ignored_records: 0,
                    terminal: true,
                };
                self.add_page_counts(&page)?;
                return Ok(Some(page));
            }
        };
        let prepared = prepare_page(
            &self.leaf.source,
            &self.context,
            document,
            self.next_message,
        )?;
        let start = usize::try_from(prepared.expected_frontier.next_message_index)
            .map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
        let mut documents = Vec::new();
        let mut rejected_records = if start == 0 {
            u64::try_from(document.initial_failures.len())
                .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
        } else {
            0
        };
        let mut ignored_records = 0_u64;
        for (offset, message) in prepared.messages.into_iter().enumerate() {
            let index = start
                .checked_add(offset)
                .ok_or(RovoDevSourceBackedError::CoordinateOverflow)?;
            if message.rejection.is_some() {
                rejected_records = rejected_records
                    .checked_add(1)
                    .ok_or(RovoDevSourceBackedError::CountMismatch)?;
            }
            if let Some(event) = message.event {
                let raw = document
                    .messages
                    .get(index)
                    .ok_or(RovoDevSourceBackedError::CoordinateOverflow)?;
                documents.push(lexical_document(
                    self.leaf,
                    document,
                    raw,
                    index,
                    event,
                    message.touches,
                )?);
            } else if message.rejection.is_none() {
                ignored_records = ignored_records
                    .checked_add(1)
                    .ok_or(RovoDevSourceBackedError::CountMismatch)?;
            }
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| RovoDevSourceBackedError::CountMismatch)?;
        let complete_records = retained_records
            .checked_add(rejected_records)
            .and_then(|count| count.checked_add(ignored_records))
            .ok_or(RovoDevSourceBackedError::CountMismatch)?;
        self.next_message = usize::try_from(prepared.next_frontier.next_message_index)
            .map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
        self.terminal = prepared.terminal;
        let page = RovoDevSourceBackedPage {
            documents,
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
            terminal: prepared.terminal,
        };
        self.add_page_counts(&page)?;
        Ok(Some(page))
    }

    pub(crate) fn finish(mut self) -> RovoDevSourceBackedResult<RovoDevSourceBackedScan> {
        if !self.terminal {
            if self.disposition == RovoDevSourceBackedDisposition::Unchanged {
                self.terminal = true;
            } else {
                return Err(RovoDevSourceBackedError::IncompleteScan);
            }
        }
        self.leaf.snapshot.revalidate(&self.leaf.authority)?;
        let closing = RovoDevSnapshot::read(
            &self.leaf.source,
            &self.context,
            &self.leaf.authority,
            &self.leaf.session_relative_path,
            &self.leaf.context_relative_path,
            self.leaf.metadata_relative_path.as_deref(),
        )?;
        closing.revalidate(&self.leaf.authority)?;
        let opening_observation = self
            .leaf
            .snapshot
            .observation(self.leaf.source_key.clone())?;
        let closing_observation = closing.observation(self.leaf.source_key.clone())?;
        let counts = if self.disposition == RovoDevSourceBackedDisposition::Unchanged {
            self.previous
                .as_ref()
                .ok_or(RovoDevSourceBackedError::CountMismatch)?
                .counts()
        } else {
            self.counts.certified_bytes = self.leaf.snapshot.certified_bytes;
            self.counts
        };
        let frontier = final_frontier(&self.leaf.snapshot)?;
        let source = CertifiedSource::certify_with_frontier(
            opening_observation,
            closing_observation,
            PARSER_REVISION,
            self.leaf.snapshot.source_sha256,
            counts,
            Some(frontier),
        )?;
        Ok(RovoDevSourceBackedScan {
            disposition: self.disposition,
            source,
        })
    }

    fn add_page_counts(&mut self, page: &RovoDevSourceBackedPage) -> RovoDevSourceBackedResult<()> {
        self.counts.complete_records =
            checked_add(self.counts.complete_records, page.complete_records)?;
        self.counts.retained_records =
            checked_add(self.counts.retained_records, page.retained_records)?;
        self.counts.rejected_records =
            checked_add(self.counts.rejected_records, page.rejected_records)?;
        self.counts.ignored_records =
            checked_add(self.counts.ignored_records, page.ignored_records)?;
        self.counts.indexed_documents = checked_add(
            self.counts.indexed_documents,
            u64::try_from(page.documents.len())
                .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
        )?;
        Ok(())
    }
}

fn checked_add(left: u64, right: u64) -> RovoDevSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(RovoDevSourceBackedError::CountMismatch)
}

fn final_frontier(snapshot: &RovoDevSnapshot) -> RovoDevSourceBackedResult<SourceFrontier> {
    Ok(SourceFrontier::new(
        FRONTIER_KIND,
        TypedKey::composite(vec![
            TypedKey::U64(
                u64::try_from(snapshot.message_count())
                    .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
            ),
            TypedKey::bytes(snapshot.source_sha256.to_vec())?,
        ])?,
        snapshot.certified_bytes,
        snapshot.source_sha256,
    )?)
}

fn lexical_document(
    leaf: &RovoDevSourceBackedLeaf,
    document: &super::PreparedDocument,
    raw_message: &serde_json::Value,
    index: usize,
    event: super::RovoDevCoreEvent,
    touches: Vec<super::RovoDevFileTouch>,
) -> RovoDevSourceBackedResult<LexicalDocument> {
    let native_item_key = native_item_key(leaf, raw_message, index)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &leaf.source_key,
        session_id: leaf.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let message_index =
        u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
    let native_record_id = provider_message_id(raw_message, message_index);
    let locator = SourceRecordLocator::new(
        leaf.source_key.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_CONTEXT_FILE)?,
            record_coordinate: TypedKey::composite(vec![
                TypedKey::utf8(MESSAGE_OBJECT_KIND)?,
                TypedKey::U64(message_index),
                TypedKey::utf8(&native_record_id)?,
            ])?,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(leaf.snapshot.source_sha256),
        leaf.snapshot.context_sha256,
    )?;
    let body = lexical_preview(raw_message, &event);
    Ok(LexicalDocument {
        event_id,
        session_id: leaf.session_id,
        parent_session_id: leaf.parent_session_id,
        root_session_id: leaf.root_session_id,
        source: leaf.source_key.clone(),
        locator,
        provider_session_id: Some(document.provider_session_id.clone()),
        branch: provider_string_field(
            &document.metadata,
            &[
                "branch",
                "git_branch",
                "gitBranch",
                "vcs_branch",
                "vcsBranch",
            ],
        )
        .or_else(|| {
            provider_string_field(
                &document.context_metadata,
                &[
                    "branch",
                    "git_branch",
                    "gitBranch",
                    "vcs_branch",
                    "vcsBranch",
                ],
            )
        }),
        source_path: Some(leaf.source.context_path.display().to_string()),
        agent_type: if document.parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        }
        .as_str()
        .to_owned(),
        is_primary: document.parent_provider_session_id.is_none(),
        event_sequence: message_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: document.cwd.clone(),
        cwd: document.cwd.clone(),
        touched_files: touches.into_iter().map(|touch| touch.path).collect(),
    })
}

fn native_item_key(
    leaf: &RovoDevSourceBackedLeaf,
    message: &serde_json::Value,
    index: usize,
) -> RovoDevSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = explicit_message_id(message)
        .filter(|native_id| leaf.unique_message_ids.contains(*native_id))
    {
        return Ok(NativeItemKey::native_id(
            EVENT_KEY_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    let coordinate = TypedKey::composite(vec![
        explicit_message_id(message)
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(
            u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        ),
    ])?;
    Ok(NativeItemKey::revision_scoped_position(
        EVENT_POSITION_KIND,
        coordinate,
        TypedKey::bytes(leaf.snapshot.source_sha256.to_vec())?,
    )?)
}

fn lexical_preview(raw_message: &serde_json::Value, event: &super::RovoDevCoreEvent) -> String {
    let mut text = provider_block_text(raw_message)
        .or_else(|| {
            event
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            event
                .payload
                .get("output_preview")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            ["tool", "command", "call_id"]
                .into_iter()
                .filter_map(|field| event.payload.get(field).and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        });
    text = text.chars().take(MAX_BODY_PREVIEW_CHARS).collect();
    if text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RovoDevHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

pub(crate) fn hydrate_rovodev_source_record(
    inventory: &RovoDevSourceBackedInventory,
    _event_id: StableEntityId,
    locator: &SourceRecordLocator,
) -> RovoDevSourceBackedResult<RovoDevHydratedSourceRecord> {
    locator.validate_contract()?;
    let leaf = inventory
        .leaves
        .iter()
        .find(|leaf| leaf.source_key.exact_descriptor_eq(locator.source()))
        .ok_or(RovoDevSourceBackedError::LocatorSourceMissing)?;
    if locator.source().provider() != CaptureProvider::RovoDev.as_str()
        || locator.source().source_format() != ROVODEV_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    let current = RovoDevSnapshot::read(
        &leaf.source,
        &inventory.context,
        &leaf.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    if locator.certified_source_revision_digest() != Some(&current.source_sha256)
        || locator.record_digest() != &current.context_sha256
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    let (message_index, expected_native_id) = decode_tree_coordinate(locator.coordinate())?;
    let document = current
        .document
        .as_ref()
        .map_err(|_| RovoDevSourceBackedError::LocatorObjectChanged)?;
    let message = document
        .messages
        .get(message_index)
        .ok_or(RovoDevSourceBackedError::LocatorObjectChanged)?;
    let observed_native_id = provider_message_id(
        message,
        u64::try_from(message_index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
    );
    if observed_native_id != expected_native_id {
        return Err(RovoDevSourceBackedError::LocatorObjectChanged);
    }
    let provider_bytes = current
        .context_bytes
        .clone()
        .ok_or(RovoDevSourceBackedError::LocatorObjectChanged)?;
    let decoded_display_text = provider_block_text(message)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_owned());
    current.revalidate(&leaf.authority)?;
    let closing = RovoDevSnapshot::read(
        &leaf.source,
        &inventory.context,
        &leaf.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    closing.revalidate(&leaf.authority)?;
    if closing.source_sha256 != current.source_sha256
        || closing.context_sha256 != current.context_sha256
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    Ok(RovoDevHydratedSourceRecord {
        provider_bytes,
        decoded_display_text,
    })
}

fn decode_tree_coordinate(
    coordinate: &NativeRecordCoordinate,
) -> RovoDevSourceBackedResult<(usize, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(relative_file) = relative_file_key else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(object_kind), TypedKey::U64(message_index), TypedKey::Utf8(native_id)] =
        parts.as_slice()
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    if relative_file != RELATIVE_CONTEXT_FILE
        || object_kind != MESSAGE_OBJECT_KIND
        || native_id.is_empty()
    {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    }
    Ok((
        usize::try_from(*message_index)
            .map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        native_id.clone(),
    ))
}

#[cfg(test)]
#[path = "source_backed/tests.rs"]
mod tests;

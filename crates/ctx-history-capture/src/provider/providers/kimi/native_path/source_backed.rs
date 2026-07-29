//! Source-backed Kimi Code CLI projection and exact hydration.
//!
//! Kimi's authority is compound: one `wire.jsonl` leaf is interpreted together
//! with its session `state.json` and the root `session_index.jsonl`. This module
//! certifies that compound observation while leaving generation lifecycle and
//! publication to the shared coordinator.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    CertifiedSourceInventory, ContentSourceResolver, EventHydrationRequest, EventIdentityInput,
    EventType, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{IndexError, LexicalDocument};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        },
        normalization::{
            provider_local_preview, provider_output_event_is_failure,
            provider_result_outcome_evidence,
        },
        tool_input,
    },
    CaptureError, OutputObservationKind, OutputOutcome, KIMI_CODE_CLI_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_TEXT_CHARS,
};

use super::super::{
    event::{
        kimi_event_role, kimi_event_text, kimi_event_type, kimi_legacy_provider_event_hash,
        kimi_output_content, kimi_record_timestamp,
    },
    layout::{
        canonical_source_root_for_wire, complete_content_auxiliary_paths, KimiFrozenFileMetadata,
        KimiWireRoute, KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES,
    },
    source::KimiWireObservation,
};

const KIMI_SOURCE_SCHEMA_VARIANT: &str = "compound-wire-tree-v1";
const KIMI_SOURCE_ANCHOR_NAMESPACE: &str = "kimi-code-cli-wire-lineage-v1";
const KIMI_NATIVE_SESSION_NAMESPACE: &str = "kimi-code-cli-session-v1";
const KIMI_NATIVE_EVENT_POSITION_KIND: &str = "kimi-code-cli-wire-ordinal-v1";
const KIMI_LOGICAL_SESSION_KIND: &str = "agent-session";
const KIMI_LOGICAL_EVENT_KIND: &str = "wire-event";
const KIMI_INVENTORY_AUTHORITY_NAMESPACE: &str = "kimi-code-cli-root-v1";
const KIMI_INVENTORY_REVISION_KIND: &str = "kimi-code-cli-compound-tree-sha256-v1";
const KIMI_INVENTORY_DISCOVERY_REVISION: &str = "kimi-code-cli-discovery-v1";
const KIMI_SOURCE_REVISION_KIND: &str = "kimi-code-cli-compound-leaf-sha256-v1";
const KIMI_SOURCE_PARSER_REVISION: &str = "kimi-code-cli-source-backed-v1";
const KIMI_INVENTORY_DOMAIN: &[u8] = b"ctx.kimi.source-backed.inventory.v1\0";
const KIMI_REVISION_DOMAIN: &[u8] = b"ctx.kimi.source-backed.revision.v1\0";
const KIMI_CONTENT_DOMAIN: &[u8] = b"ctx.kimi.source-backed.content.v1\0";
const KIMI_ABSENT_AUXILIARY_DIGEST: [u8; 32] = [0; 32];
const MAX_KIMI_HYDRATED_RECORD_BYTES: u64 = MAX_PROVIDER_JSONL_LINE_BYTES as u64 + 2;
const KIMI_DISCOVERY_MAX_DEPTH: usize = 16;
const KIMI_DISCOVERY_MAX_ENTRIES: usize = 65_536;

struct RawLine {
    bytes: Vec<u8>,
    observed_bytes: u64,
    terminated: bool,
    oversized: bool,
}

struct KimiInventory {
    paths: BTreeSet<PathBuf>,
    source_root: PathBuf,
    root_missing: bool,
}

struct KimiOutputClassification {
    kind: OutputObservationKind,
    outcome: OutputOutcome,
}

#[derive(Debug, Error)]
pub(crate) enum KimiSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("the Kimi source inventory changed while it was being certified")]
    InventoryChanged,
    #[error("the Kimi source changed while it was being scanned")]
    SourceChanged,
    #[error("the Kimi source inventory is unavailable")]
    InventoryUnavailable,
    #[error("duplicate Kimi provider session lineage {0}")]
    DuplicateLineage(String),
    #[error("the Kimi source is not present in this certified inventory")]
    UnknownSource,
    #[error("the Kimi source-backed locator is invalid")]
    InvalidLocator,
    #[error("the Kimi source-backed locator range is too large")]
    LocatorRangeTooLarge,
    #[error("the Kimi source-backed locator range is missing")]
    LocatorRangeMissing,
    #[error("the Kimi source-backed record evidence is stale")]
    StaleRecordEvidence,
    #[error("Kimi source-backed accounting overflowed")]
    CountOverflow,
}

pub(crate) type KimiSourceBackedResult<T> = std::result::Result<T, KimiSourceBackedError>;

#[derive(Clone, Debug)]
struct AuxiliarySnapshot {
    length: u64,
    digest: [u8; 32],
    revision: Option<String>,
}

impl AuxiliarySnapshot {
    fn absent() -> Self {
        Self {
            length: 0,
            digest: KIMI_ABSENT_AUXILIARY_DIGEST,
            revision: None,
        }
    }

    fn feed_revision(&self, digest: &mut Sha256, label: &[u8]) {
        digest.update((label.len() as u64).to_be_bytes());
        digest.update(label);
        digest.update(self.length.to_be_bytes());
        digest.update(self.digest);
        match &self.revision {
            Some(revision) => {
                digest.update(1_u8.to_be_bytes());
                digest.update((revision.len() as u64).to_be_bytes());
                digest.update(revision.as_bytes());
            }
            None => digest.update(0_u8.to_be_bytes()),
        }
    }
}

#[derive(Clone, Debug)]
struct KimiCompoundObservation {
    native: KimiWireObservation,
    source: SourceKey,
    observation: SourceObservation,
    relative_file_key: Vec<u8>,
    state: AuxiliarySnapshot,
    index: AuxiliarySnapshot,
}

impl KimiCompoundObservation {
    fn certified_bytes(&self) -> KimiSourceBackedResult<u64> {
        self.native
            .wire()
            .length
            .checked_add(self.state.length)
            .and_then(|value| value.checked_add(self.index.length))
            .ok_or(KimiSourceBackedError::CountOverflow)
    }

    fn source_revision_digest(&self) -> KimiSourceBackedResult<[u8; 32]> {
        self.observation
            .revision()
            .try_into()
            .map_err(|_| KimiSourceBackedError::SourceChanged)
    }

    fn content_hasher(&self) -> Sha256 {
        let mut digest = Sha256::new();
        digest.update(KIMI_CONTENT_DOMAIN);
        digest.update(self.source.exact_descriptor_digest());
        digest.update((self.relative_file_key.len() as u64).to_be_bytes());
        digest.update(&self.relative_file_key);
        self.state.feed_revision(&mut digest, b"state.json");
        self.index
            .feed_revision(&mut digest, b"session_index.jsonl");
        digest
    }
}

#[derive(Clone, Debug)]
struct KimiSourceLeaf {
    relative_path: PathBuf,
    source: SourceKey,
    provider_session_id: String,
    relative_file_key: Vec<u8>,
}

#[derive(Clone, Debug)]
struct CatalogSnapshot {
    observation: SourceInventoryObservation,
    leaves: BTreeMap<SourceKey, KimiSourceLeaf>,
}

#[derive(Debug)]
struct AdmittedKimiCompound {
    compound: KimiCompoundObservation,
    wire: OpenedProviderSourceFile,
    state: Option<OpenedProviderSourceFile>,
    index: Option<OpenedProviderSourceFile>,
}

impl AdmittedKimiCompound {
    fn revalidate(&self, authority: &ProviderSourceRoot) -> KimiSourceBackedResult<()> {
        self.wire.revalidate()?;
        if let Some(state) = &self.state {
            state.revalidate()?;
        }
        if let Some(index) = &self.index {
            index.revalidate()?;
        }
        authority.revalidate()?;
        Ok(())
    }
}

/// One certified, complete Kimi root inventory.
///
/// The catalog retains paths only as process-local resolver capabilities.
/// Paths do not participate in public source, session, or event identities.
#[derive(Clone, Debug)]
pub(crate) struct KimiSourceBackedCatalog {
    source_root: PathBuf,
    selected_path: PathBuf,
    authority: ProviderSourceRoot,
    inventory: CertifiedSourceInventory,
    leaves: BTreeMap<SourceKey, KimiSourceLeaf>,
}

impl KimiSourceBackedCatalog {
    pub(crate) fn discover(path: impl AsRef<Path>) -> KimiSourceBackedResult<Self> {
        let selected_path = path.as_ref().to_path_buf();
        let opening_inventory = discover_kimi_wire_files(&selected_path)?;
        if opening_inventory.root_missing {
            return Err(KimiSourceBackedError::InventoryUnavailable);
        }
        let source_root = opening_inventory.source_root.clone();
        let authority = ProviderSourceRoot::open(&source_root)?;
        let opening = bind_snapshot(opening_inventory, &authority)?;
        authority.revalidate()?;
        let closing_inventory = discover_kimi_wire_files(&selected_path)?;
        if closing_inventory.root_missing || closing_inventory.source_root != source_root {
            return Err(KimiSourceBackedError::InventoryChanged);
        }
        let closing = bind_snapshot(closing_inventory, &authority)?;
        authority.revalidate()?;
        if opening.leaves.keys().ne(closing.leaves.keys()) {
            return Err(KimiSourceBackedError::InventoryChanged);
        }
        let sources = opening.leaves.keys().cloned().collect::<Vec<_>>();
        let inventory = CertifiedSourceInventory::certify(
            opening.observation,
            closing.observation,
            KIMI_INVENTORY_DISCOVERY_REVISION,
            sources,
        )?;
        Ok(Self {
            source_root,
            selected_path,
            authority,
            inventory,
            leaves: closing.leaves,
        })
    }

    pub(crate) fn inventory(&self) -> &CertifiedSourceInventory {
        &self.inventory
    }

    pub(crate) fn source_keys(&self) -> impl ExactSizeIterator<Item = &SourceKey> {
        self.leaves.keys()
    }

    /// Streams bounded lexical records and returns the exact compound
    /// certificate. The callback is the coordinator-owned publication seam.
    pub(crate) fn scan_source<F>(
        &self,
        source: &SourceKey,
        mut emit: F,
    ) -> KimiSourceBackedResult<CertifiedSource>
    where
        F: FnMut(LexicalDocument) -> KimiSourceBackedResult<()>,
    {
        let leaf = self
            .leaves
            .get(source)
            .ok_or(KimiSourceBackedError::UnknownSource)?;
        if !leaf.source.exact_descriptor_eq(source) {
            return Err(KimiSourceBackedError::UnknownSource);
        }
        scan_leaf(&self.authority, leaf, &mut emit)
    }

    /// Final precommit source witness for the shared generation coordinator.
    pub(crate) fn revalidate_source(
        &self,
        certificate: &CertifiedSource,
    ) -> KimiSourceBackedResult<bool> {
        let Some(leaf) = self.leaves.get(certificate.observation().source()) else {
            return Ok(false);
        };
        let current = admit_compound_leaf(&self.authority, &leaf.relative_path)?;
        current.revalidate(&self.authority)?;
        Ok(current.compound.observation == *certificate.observation())
    }

    /// Final precommit inventory witness for coordinator-owned deletion.
    pub(crate) fn revalidate_inventory(&self) -> KimiSourceBackedResult<bool> {
        let inventory = discover_kimi_wire_files(&self.selected_path)?;
        if inventory.root_missing || inventory.source_root != self.source_root {
            return Ok(false);
        }
        let current = bind_snapshot(inventory, &self.authority)?;
        self.authority.revalidate()?;
        Ok(current.observation == *self.inventory.observation()
            && current.leaves.keys().eq(self.leaves.keys()))
    }
}

/// Exact Kimi content resolver over one certified catalog discovery.
#[derive(Clone, Debug)]
pub(crate) struct KimiSourceBackedResolver {
    catalog: KimiSourceBackedCatalog,
}

impl KimiSourceBackedResolver {
    pub(crate) fn new(catalog: KimiSourceBackedCatalog) -> Self {
        Self { catalog }
    }

    fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let source = first.locator().source();
        let leaf = self.catalog.leaves.get(source).ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "Kimi source is absent from the certified inventory",
            )
        })?;
        if !leaf.source.exact_descriptor_eq(source)
            || requests
                .iter()
                .any(|request| request.locator().source() != source)
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Kimi hydration batch crosses source lineage",
            ));
        }

        let opening = admit_compound_leaf(&self.catalog.authority, &leaf.relative_path)
            .map_err(map_hydration_error)?;
        let expected_revision = opening
            .compound
            .source_revision_digest()
            .map_err(map_hydration_error)?;
        let mut decoded = Vec::with_capacity(requests.len());
        for request in requests {
            let coordinate =
                decode_locator(leaf, request.locator()).map_err(map_hydration_error)?;
            if request
                .locator()
                .certified_source_revision_digest()
                .copied()
                != Some(expected_revision)
            {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "Kimi compound source revision changed",
                ));
            }
            decoded.push(coordinate);
        }

        let mut file = opening
            .wire
            .file()
            .try_clone()
            .map_err(|error| map_hydration_error(KimiSourceBackedError::Io(error)))?;
        let mut hydrated = Vec::with_capacity(requests.len());
        for (request, coordinate) in requests.iter().zip(decoded) {
            let provider_record = read_exact_record(&mut file, request.locator(), &coordinate)
                .map_err(map_hydration_error)?;
            let value = serde_json::from_slice::<Value>(json_record_bytes(&provider_record))
                .map_err(|_| {
                    hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Kimi exact record no longer decodes as JSON",
                    )
                })?;
            let (_, body) = kimi_lexical_body(
                &value,
                coordinate.physical_ordinal,
                opening.compound.native.session.cwd.as_deref(),
            )
            .map_err(map_hydration_error)?
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::UnsupportedParserRevision,
                    "Kimi exact record has no policy-selected display text",
                )
            })?;
            hydrated.push(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: body.into_bytes(),
            });
        }
        opening
            .revalidate(&self.catalog.authority)
            .map_err(map_hydration_error)?;
        let closing = admit_compound_leaf(&self.catalog.authority, &leaf.relative_path)
            .map_err(map_hydration_error)?;
        closing
            .revalidate(&self.catalog.authority)
            .map_err(map_hydration_error)?;
        if opening.compound.observation != closing.compound.observation {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "Kimi compound source changed during hydration",
            ));
        }
        Ok(hydrated)
    }
}

impl ContentSourceResolver for KimiSourceBackedResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_requests(std::slice::from_ref(request))?
            .pop()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "Kimi hydration returned no record",
                )
            })
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

#[derive(Debug)]
struct DecodedKimiLocator {
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
    native_event_id: String,
}

fn discover_kimi_wire_files(root: &Path) -> KimiSourceBackedResult<KimiInventory> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let source_root = canonical_source_root_for_wire(root)
                .or_else(|_| std::path::absolute(root).map_err(CaptureError::from))?;
            return Ok(KimiInventory {
                paths: BTreeSet::new(),
                source_root,
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript roots must not be symbolic links",
        }
        .into());
    }
    if metadata.is_file() {
        KimiWireRoute::parse(root)?;
        let canonical = fs::canonicalize(root)?;
        return Ok(KimiInventory {
            paths: BTreeSet::from([canonical.clone()]),
            source_root: canonical_source_root_for_wire(&canonical)?,
            root_missing: false,
        });
    }
    if !metadata.is_dir() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript root is neither a file nor directory",
        }
        .into());
    }
    let mut paths = BTreeSet::new();
    let mut source_roots = BTreeSet::new();
    let mut entries = 0_usize;
    discover_kimi_directory(root, 0, &mut entries, &mut paths, &mut source_roots)?;
    if source_roots.len() > 1 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript selection spans multiple canonical layout roots",
        }
        .into());
    }
    Ok(KimiInventory {
        paths,
        source_root: source_roots
            .into_iter()
            .next()
            .unwrap_or(fs::canonicalize(root)?),
        root_missing: false,
    })
}

fn discover_kimi_directory(
    directory: &Path,
    depth: usize,
    entries: &mut usize,
    paths: &mut BTreeSet<PathBuf>,
    source_roots: &mut BTreeSet<PathBuf>,
) -> KimiSourceBackedResult<()> {
    if depth > KIMI_DISCOVERY_MAX_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: directory.to_path_buf(),
            reason: "Kimi transcript tree exceeds the discovery depth bound",
        }
        .into());
    }
    let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        *entries = entries.saturating_add(1);
        if *entries > KIMI_DISCOVERY_MAX_ENTRIES {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: directory.to_path_buf(),
                reason: "Kimi transcript tree exceeds the discovery entry bound",
            }
            .into());
        }
        let path = child.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            discover_kimi_directory(&path, depth.saturating_add(1), entries, paths, source_roots)?;
        } else if metadata.is_file() && KimiWireRoute::parse(&path).is_ok() {
            let canonical = fs::canonicalize(path)?;
            KimiWireRoute::parse(&canonical)?;
            source_roots.insert(canonical_source_root_for_wire(&canonical)?);
            paths.insert(canonical);
        }
    }
    Ok(())
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    max_bytes: usize,
) -> KimiSourceBackedResult<RawLine> {
    let mut bytes = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed_bytes = observed_bytes
            .checked_add(chunk.len() as u64)
            .ok_or(KimiSourceBackedError::CountOverflow)?;
        if bytes.len() < max_bytes.saturating_add(2) {
            let remaining = max_bytes.saturating_add(2).saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        oversized |= observed_bytes > max_bytes as u64;
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if terminated {
            break;
        }
    }
    Ok(RawLine {
        bytes,
        observed_bytes,
        terminated,
        oversized,
    })
}

fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn bind_snapshot(
    inventory: KimiInventory,
    authority: &ProviderSourceRoot,
) -> KimiSourceBackedResult<CatalogSnapshot> {
    let source_root = inventory.source_root;
    let mut leaves = BTreeMap::new();
    let mut native_lineages = BTreeSet::new();
    let mut observations = Vec::with_capacity(inventory.paths.len());
    for wire_path in inventory.paths {
        let relative_path = wire_path
            .strip_prefix(&source_root)
            .map_err(|_| KimiSourceBackedError::SourceChanged)?
            .to_path_buf();
        let admitted = admit_compound_leaf(authority, &relative_path)?;
        admitted.revalidate(authority)?;
        let compound = admitted.compound;
        let provider_session_id = compound.native.session.provider_session_id.clone();
        if !native_lineages.insert(provider_session_id.clone()) {
            return Err(KimiSourceBackedError::DuplicateLineage(provider_session_id));
        }
        observations.push((
            compound.relative_file_key.clone(),
            compound.source.exact_descriptor_digest(),
            compound.observation.revision().to_vec(),
        ));
        let leaf = KimiSourceLeaf {
            relative_path,
            source: compound.source.clone(),
            provider_session_id,
            relative_file_key: compound.relative_file_key,
        };
        if leaves.insert(compound.source, leaf).is_some() {
            return Err(KimiSourceBackedError::InventoryChanged);
        }
    }
    observations.sort_by(|left, right| left.0.cmp(&right.0));
    let mut revision = Sha256::new();
    revision.update(KIMI_INVENTORY_DOMAIN);
    revision.update((observations.len() as u64).to_be_bytes());
    for (relative_file_key, descriptor, source_revision) in observations {
        revision.update((relative_file_key.len() as u64).to_be_bytes());
        revision.update(relative_file_key);
        revision.update(descriptor);
        revision.update((source_revision.len() as u64).to_be_bytes());
        revision.update(source_revision);
    }
    let root_digest: [u8; 32] = Sha256::digest(source_root.as_os_str().as_encoded_bytes()).into();
    let observation = SourceInventoryObservation::new(
        CaptureProvider::KimiCodeCli.as_str(),
        KIMI_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(root_digest.to_vec())?,
        KIMI_INVENTORY_REVISION_KIND,
        revision.finalize().to_vec(),
    )?;
    Ok(CatalogSnapshot {
        observation,
        leaves,
    })
}

fn admit_compound_leaf(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
) -> KimiSourceBackedResult<AdmittedKimiCompound> {
    let wire = authority.open_file(relative_path)?;
    let display_path = authority.named_path().join(relative_path);
    let (state_path, index_path) = complete_content_auxiliary_paths(&display_path)?;
    let state_relative = state_path
        .strip_prefix(authority.named_path())
        .map_err(|_| KimiSourceBackedError::SourceChanged)?;
    let index_relative = index_path
        .strip_prefix(authority.named_path())
        .map_err(|_| KimiSourceBackedError::SourceChanged)?;
    let state = open_optional_file(authority, state_relative)?;
    let index = open_optional_file(authority, index_relative)?;
    let state_bytes = read_auxiliary_bytes(state.as_ref())?;
    let index_bytes = read_auxiliary_bytes(index.as_ref())?;
    let native = KimiWireObservation::read_from_admitted(
        &display_path,
        display_path.clone(),
        wire.metadata(),
        state
            .as_ref()
            .zip(state_bytes.as_deref())
            .map(|(file, bytes)| (file.metadata(), bytes)),
        index
            .as_ref()
            .zip(index_bytes.as_deref())
            .map(|(file, bytes)| (file.metadata(), bytes)),
    )?;
    let relative_file_key = relative_path.as_os_str().as_encoded_bytes().to_vec();
    if relative_file_key.is_empty() {
        return Err(KimiSourceBackedError::SourceChanged);
    }
    let source = source_key(&native.session.provider_session_id)?;
    let state_snapshot = auxiliary_snapshot(state.as_ref(), state_bytes.as_deref())?;
    let index_snapshot = auxiliary_snapshot(index.as_ref(), index_bytes.as_deref())?;
    let mut revision = Sha256::new();
    revision.update(KIMI_REVISION_DOMAIN);
    revision.update(source.exact_descriptor_digest());
    revision.update((relative_file_key.len() as u64).to_be_bytes());
    revision.update(&relative_file_key);
    let wire_revision = native.wire().revision_component();
    revision.update((wire_revision.len() as u64).to_be_bytes());
    revision.update(wire_revision.as_bytes());
    state_snapshot.feed_revision(&mut revision, b"state.json");
    index_snapshot.feed_revision(&mut revision, b"session_index.jsonl");
    let observation = SourceObservation::new(
        source.clone(),
        KIMI_SOURCE_REVISION_KIND,
        revision.finalize().to_vec(),
    )?;
    Ok(AdmittedKimiCompound {
        compound: KimiCompoundObservation {
            native,
            source,
            observation,
            relative_file_key,
            state: state_snapshot,
            index: index_snapshot,
        },
        wire,
        state,
        index,
    })
}

fn open_optional_file(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
) -> KimiSourceBackedResult<Option<OpenedProviderSourceFile>> {
    match authority.open_file(relative_path) {
        Ok(file) => Ok(Some(file)),
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_auxiliary_bytes(
    file: Option<&OpenedProviderSourceFile>,
) -> KimiSourceBackedResult<Option<Vec<u8>>> {
    let Some(file) = file else {
        return Ok(None);
    };
    if file.len() > KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES as u64 {
        return Err(CaptureError::InvalidPayload(
            "Kimi source-backed auxiliary file exceeds its bounded layout limit".to_owned(),
        )
        .into());
    }
    Ok(Some(
        file.read_all_bounded(KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES)?,
    ))
}

fn auxiliary_snapshot(
    file: Option<&OpenedProviderSourceFile>,
    bytes: Option<&[u8]>,
) -> KimiSourceBackedResult<AuxiliarySnapshot> {
    let (Some(file), Some(bytes)) = (file, bytes) else {
        return Ok(AuxiliarySnapshot::absent());
    };
    Ok(AuxiliarySnapshot {
        length: file.len(),
        digest: Sha256::digest(&bytes).into(),
        revision: Some(
            KimiFrozenFileMetadata::from_metadata(file.metadata())?.revision_component(),
        ),
    })
}

fn source_key(provider_session_id: &str) -> KimiSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        KIMI_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::KimiCodeCli.as_str(),
        KIMI_CODE_CLI_SOURCE_FORMAT,
        KIMI_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn session_identity(
    source: &SourceKey,
    provider_session_id: &str,
) -> KimiSourceBackedResult<StableEntityId> {
    let key = NativeSessionKey::native_id(
        KIMI_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: KIMI_LOGICAL_SESSION_KIND,
        native_session_key: &key,
    })?)
}

fn lineage_session_identity(provider_session_id: &str) -> KimiSourceBackedResult<StableEntityId> {
    let source = source_key(provider_session_id)?;
    session_identity(&source, provider_session_id)
}

fn scan_leaf<F>(
    authority: &ProviderSourceRoot,
    leaf: &KimiSourceLeaf,
    emit: &mut F,
) -> KimiSourceBackedResult<CertifiedSource>
where
    F: FnMut(LexicalDocument) -> KimiSourceBackedResult<()>,
{
    let opening = admit_compound_leaf(authority, &leaf.relative_path)?;
    if !opening.compound.source.exact_descriptor_eq(&leaf.source)
        || opening.compound.relative_file_key != leaf.relative_file_key
        || opening.compound.native.session.provider_session_id != leaf.provider_session_id
    {
        return Err(KimiSourceBackedError::SourceChanged);
    }
    let source_revision_digest = opening.compound.source_revision_digest()?;
    let session_id = session_identity(&leaf.source, &leaf.provider_session_id)?;
    let fallback_timestamp = opening
        .compound
        .native
        .session
        .started_at
        .or_else(|| DateTime::<Utc>::from_timestamp(0, 0))
        .ok_or(KimiSourceBackedError::SourceChanged)?;
    let file = opening.wire.file().try_clone()?;
    let mut reader = std::io::BufReader::new(file);
    let mut content_hasher = opening.compound.content_hasher();
    let mut counts = ScannedSourceCounts::default();
    let mut offset = 0_u64;
    let mut ordinal = 0_u64;

    loop {
        let raw = read_bounded_line(
            &mut reader,
            &mut content_hasher,
            MAX_PROVIDER_JSONL_LINE_BYTES,
        )?;
        if raw.observed_bytes == 0 {
            break;
        }
        let byte_start = offset;
        offset = offset
            .checked_add(raw.observed_bytes)
            .ok_or(KimiSourceBackedError::CountOverflow)?;
        if !raw.terminated {
            break;
        }
        counts.complete_records = counts
            .complete_records
            .checked_add(1)
            .ok_or(KimiSourceBackedError::CountOverflow)?;
        let current_ordinal = ordinal;
        ordinal = ordinal
            .checked_add(1)
            .ok_or(KimiSourceBackedError::CountOverflow)?;
        if raw.oversized {
            counts.rejected_records = counts
                .rejected_records
                .checked_add(1)
                .ok_or(KimiSourceBackedError::CountOverflow)?;
            continue;
        }
        let record_bytes = json_record_bytes(&raw.bytes);
        if record_bytes.iter().all(u8::is_ascii_whitespace) {
            counts.ignored_records = counts
                .ignored_records
                .checked_add(1)
                .ok_or(KimiSourceBackedError::CountOverflow)?;
            continue;
        }
        let value = match serde_json::from_slice::<Value>(record_bytes) {
            Ok(value) => value,
            Err(_) => {
                counts.rejected_records = counts
                    .rejected_records
                    .checked_add(1)
                    .ok_or(KimiSourceBackedError::CountOverflow)?;
                continue;
            }
        };
        let record_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if record_type == "metadata" {
            counts.ignored_records = counts
                .ignored_records
                .checked_add(1)
                .ok_or(KimiSourceBackedError::CountOverflow)?;
            continue;
        }
        counts.retained_records = counts
            .retained_records
            .checked_add(1)
            .ok_or(KimiSourceBackedError::CountOverflow)?;
        let Some(document) = lexical_document(
            &opening.compound,
            session_id,
            current_ordinal,
            byte_start,
            raw.observed_bytes,
            record_bytes,
            &value,
            fallback_timestamp,
            source_revision_digest,
        )?
        else {
            continue;
        };
        emit(document)?;
        counts.indexed_documents = counts
            .indexed_documents
            .checked_add(1)
            .ok_or(KimiSourceBackedError::CountOverflow)?;
    }

    if offset != opening.compound.native.wire().length {
        return Err(KimiSourceBackedError::SourceChanged);
    }
    counts.certified_bytes = opening.compound.certified_bytes()?;
    opening.revalidate(authority)?;
    let closing = admit_compound_leaf(authority, &leaf.relative_path)?;
    closing.revalidate(authority)?;
    let content_digest = content_hasher.finalize().into();
    Ok(CertifiedSource::certify(
        opening.compound.observation,
        closing.compound.observation,
        KIMI_SOURCE_PARSER_REVISION,
        content_digest,
        counts,
    )?)
}

#[allow(clippy::too_many_arguments)]
fn lexical_document(
    compound: &KimiCompoundObservation,
    session_id: StableEntityId,
    ordinal: u64,
    byte_offset: u64,
    byte_length: u64,
    record_bytes: &[u8],
    value: &Value,
    fallback_timestamp: DateTime<Utc>,
    source_revision_digest: [u8; 32],
) -> KimiSourceBackedResult<Option<LexicalDocument>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((event_type, body)) =
        kimi_lexical_body(value, ordinal, compound.native.session.cwd.as_deref())?
    else {
        return Ok(None);
    };
    let role = kimi_event_role(record_type, value, event_type);
    let occurred_at =
        kimi_record_timestamp(value, fallback_timestamp).unwrap_or(fallback_timestamp);
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(KimiSourceBackedError::CountOverflow)?;
    let native_event_id = kimi_legacy_provider_event_hash(record_type, value, line_number);
    let event_key = NativeItemKey::certified_position(
        KIMI_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &compound.source,
        session_id,
        logical_item_kind: KIMI_LOGICAL_EVENT_KIND,
        native_item_key: &event_key,
        subrecord_selector: None,
    })?;
    let coordinate = TypedKey::composite(vec![
        TypedKey::U64(byte_offset),
        TypedKey::U64(byte_length),
        TypedKey::U64(ordinal),
        TypedKey::utf8(&compound.native.session.provider_session_id)?,
        TypedKey::utf8(native_event_id)?,
    ])?;
    let locator = SourceRecordLocator::new(
        compound.source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::bytes(compound.relative_file_key.clone())?,
            record_coordinate: coordinate,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        Sha256::digest(record_bytes).into(),
    )?;
    let touched_files = kimi_touched_paths(
        value,
        event_type,
        event_type_supports_structured_file_touches(event_type),
    )?;
    let parent_session_id = compound
        .native
        .session
        .parent_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?;
    let root_session_id = compound
        .native
        .session
        .root_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?
        .unwrap_or(session_id);
    let workspace = compound.native.session.cwd.clone();
    Ok(Some(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: compound.source.clone(),
        locator,
        provider_session_id: Some(compound.native.session.provider_session_id.clone()),
        branch: None,
        source_path: Some(compound.native.canonical_path().display().to_string()),
        agent_type: if compound.native.session.is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        }
        .as_str()
        .to_owned(),
        is_primary: compound.native.session.is_primary,
        event_sequence: ordinal,
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        event_type: event_type.as_str().to_owned(),
        role: Some(role.as_str().to_owned()),
        body,
        workspace,
        cwd: compound.native.session.cwd.clone(),
        touched_files,
    }))
}

fn kimi_lexical_body(
    value: &Value,
    _ordinal: u64,
    _cwd: Option<&str>,
) -> KimiSourceBackedResult<Option<(EventType, String)>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut event_type = kimi_event_type(record_type, value);
    let body = if event_type == EventType::ToolOutput {
        let output = kimi_output_classification(value);
        if !matches!(
            output.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        ) {
            return Ok(None);
        }
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        kimi_output_content(value).unwrap_or_default()
    } else {
        kimi_event_text(record_type, value, event_type)
    };
    let body = provider_local_preview(&body, PROVIDER_MAX_TEXT_CHARS).0;
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some((event_type, body)))
}

fn kimi_touched_paths(
    value: &Value,
    event_type: EventType,
    include_structured_touches: bool,
) -> KimiSourceBackedResult<Vec<String>> {
    if !matches!(
        event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    visit_provider_file_touch_drafts_with_limit(
        value,
        include_structured_touches,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, draft)| {
            paths.push(draft.path);
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(paths)
}

fn kimi_output_classification(value: &Value) -> KimiOutputClassification {
    let event = value.get("event").unwrap_or(value);
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .or_else(|| event.get("name"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool");
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let outcome = if kimi_value_timed_out(event) {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(event) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, event).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    KimiOutputClassification { kind, outcome }
}

fn kimi_value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(kimi_value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(kimi_value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn decode_locator(
    leaf: &KimiSourceLeaf,
    locator: &SourceRecordLocator,
) -> KimiSourceBackedResult<DecodedKimiLocator> {
    locator.validate_contract()?;
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || !leaf.source.exact_descriptor_eq(locator.source())
    {
        return Err(KimiSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = locator.coordinate()
    else {
        return Err(KimiSourceBackedError::InvalidLocator);
    };
    if relative_file_key != &TypedKey::Bytes(leaf.relative_file_key.clone()) {
        return Err(KimiSourceBackedError::InvalidLocator);
    }
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(KimiSourceBackedError::InvalidLocator);
    };
    let [TypedKey::U64(byte_offset), TypedKey::U64(byte_length), TypedKey::U64(physical_ordinal), TypedKey::Utf8(provider_session_id), TypedKey::Utf8(native_event_id)] =
        parts.as_slice()
    else {
        return Err(KimiSourceBackedError::InvalidLocator);
    };
    if provider_session_id != &leaf.provider_session_id || *byte_length == 0 {
        return Err(KimiSourceBackedError::InvalidLocator);
    }
    if *byte_length > MAX_KIMI_HYDRATED_RECORD_BYTES {
        return Err(KimiSourceBackedError::LocatorRangeTooLarge);
    }
    Ok(DecodedKimiLocator {
        byte_offset: *byte_offset,
        byte_length: *byte_length,
        physical_ordinal: *physical_ordinal,
        native_event_id: native_event_id.clone(),
    })
}

fn read_exact_record(
    file: &mut File,
    locator: &SourceRecordLocator,
    coordinate: &DecodedKimiLocator,
) -> KimiSourceBackedResult<Vec<u8>> {
    let range_end = coordinate
        .byte_offset
        .checked_add(coordinate.byte_length)
        .ok_or(KimiSourceBackedError::LocatorRangeTooLarge)?;
    if file.metadata()?.len() < range_end {
        return Err(KimiSourceBackedError::LocatorRangeMissing);
    }
    file.seek(SeekFrom::Start(coordinate.byte_offset))?;
    let length = usize::try_from(coordinate.byte_length)
        .map_err(|_| KimiSourceBackedError::LocatorRangeTooLarge)?;
    let mut provider_bytes = vec![0; length];
    file.read_exact(&mut provider_bytes)?;
    if provider_bytes[..provider_bytes.len().saturating_sub(1)].contains(&b'\n')
        || (provider_bytes.last() != Some(&b'\n') && range_end != file.metadata()?.len())
    {
        return Err(KimiSourceBackedError::StaleRecordEvidence);
    }
    let record_bytes = json_record_bytes(&provider_bytes);
    if &Sha256::digest(record_bytes)[..] != locator.record_digest() {
        return Err(KimiSourceBackedError::StaleRecordEvidence);
    }
    let value = serde_json::from_slice::<Value>(record_bytes)
        .map_err(|_| KimiSourceBackedError::StaleRecordEvidence)?;
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let line_number = usize::try_from(coordinate.physical_ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(KimiSourceBackedError::InvalidLocator)?;
    if kimi_legacy_provider_event_hash(record_type, &value, line_number)
        != coordinate.native_event_id
    {
        return Err(KimiSourceBackedError::StaleRecordEvidence);
    }
    Ok(provider_bytes)
}

fn hydration_failure(kind: HydrationFailureKind, detail: &str) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_owned(),
    }
}

fn map_hydration_error(error: KimiSourceBackedError) -> HydrationFailure {
    let kind = match error {
        KimiSourceBackedError::UnknownSource => HydrationFailureKind::ConfirmedDeleted,
        KimiSourceBackedError::InvalidLocator
        | KimiSourceBackedError::LocatorRangeTooLarge
        | KimiSourceBackedError::Projection(_)
        | KimiSourceBackedError::Resolver(_) => HydrationFailureKind::InvalidLocator,
        KimiSourceBackedError::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
        KimiSourceBackedError::StaleRecordEvidence => HydrationFailureKind::StaleRecordEvidence,
        KimiSourceBackedError::SourceChanged | KimiSourceBackedError::InventoryChanged => {
            HydrationFailureKind::StaleSourceEvidence
        }
        KimiSourceBackedError::InventoryUnavailable
        | KimiSourceBackedError::Capture(_)
        | KimiSourceBackedError::Io(_)
        | KimiSourceBackedError::Index(_)
        | KimiSourceBackedError::DuplicateLineage(_)
        | KimiSourceBackedError::CountOverflow => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(
        kind,
        "Kimi provider source could not satisfy exact hydration",
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use ctx_history_core::{ContentSourceResolver, EventHydrationRequest};
    use serde_json::json;

    use crate::test_support_paths::tempdir;

    use super::*;

    fn fixture(prompt: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".kimi-code");
        let session = root.join("sessions/work/session-1");
        let agent = session.join("agents/main");
        fs::create_dir_all(&agent).unwrap();
        fs::write(
            root.join("session_index.jsonl"),
            format!(
                "{}\n",
                json!({
                    "sessionId": "session-1",
                    "sessionDir": session,
                    "workDir": "/workspace/kimi"
                })
            ),
        )
        .unwrap();
        fs::write(
            session.join("state.json"),
            json!({
                "createdAt": "2026-07-17T12:00:00Z",
                "title": "initial",
                "agents": {"main": {"type": "main"}}
            })
            .to_string(),
        )
        .unwrap();
        let wire = agent.join("wire.jsonl");
        let mut file = File::create(&wire).unwrap();
        for record in [
            json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
            json!({
                "type": "turn.prompt",
                "time": 1_784_289_600_001_i64,
                "input": prompt
            }),
            json!({
                "type": "context.append_loop_event",
                "time": 1_784_289_600_002_i64,
                "event": {
                    "type": "tool.result",
                    "toolName": "bash",
                    "exit_code": 0,
                    "output": "SUCCESS_BODY_MUST_NOT_BE_STORED"
                }
            }),
            json!({
                "type": "context.append_loop_event",
                "time": 1_784_289_600_003_i64,
                "event": {
                    "type": "tool.result",
                    "toolName": "bash",
                    "exit_code": 7,
                    "output": "bounded failure"
                }
            }),
        ] {
            writeln!(file, "{record}").unwrap();
        }
        (temp, root, wire)
    }

    #[test]
    fn kimi_source_backed_compound_cold_scan_and_exact_hydration() {
        let (_temp, root, _wire) = fixture("cold exact message");
        let catalog = KimiSourceBackedCatalog::discover(&root).unwrap();
        assert_eq!(catalog.inventory().observed_sources(), 1);
        assert!(catalog.revalidate_inventory().unwrap());
        let source = catalog.source_keys().next().unwrap().clone();
        let mut documents = Vec::new();
        let certificate = catalog
            .scan_source(&source, |document| {
                documents.push(document);
                Ok(())
            })
            .unwrap();
        assert_eq!(certificate.counts().complete_records, 4);
        assert_eq!(certificate.counts().indexed_documents, 2);
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].body, "cold exact message");
        assert_eq!(documents[1].body, "bounded failure");
        assert_eq!(documents[0].root_session_id, documents[0].session_id);
        assert_eq!(documents[0].parent_session_id, None);
        assert_eq!(
            documents[0].provider_session_id.as_deref(),
            Some("session-1")
        );
        assert_eq!(documents[0].branch, None);
        assert!(documents[0].source_path.is_some());
        assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
        assert!(documents[0].is_primary);
        assert_eq!(documents[0].workspace.as_deref(), Some("/workspace/kimi"));
        assert!(documents
            .iter()
            .all(|document| !document.body.contains("SUCCESS_BODY_MUST_NOT_BE_STORED")));
        assert!(documents.iter().all(|document| matches!(
            document.locator.coordinate(),
            NativeRecordCoordinate::TreeRecord { .. }
        )));
        assert!(documents.iter().all(|document| {
            document.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
        }));
        assert!(catalog.revalidate_source(&certificate).unwrap());

        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();
        let resolver = KimiSourceBackedResolver::new(catalog);
        let hydrated = resolver.hydrate_event(&request).unwrap();
        assert_eq!(hydrated.event_id, documents[0].event_id);
        assert_eq!(hydrated.provider_bytes, b"cold exact message");
    }

    #[test]
    fn kimi_source_backed_indexes_and_hydrates_the_full_policy_body() {
        let prompt = format!("kimi-head-{}-kimi-tail", "x".repeat(3_000));
        let (_temp, root, _wire) = fixture(&prompt);
        let catalog = KimiSourceBackedCatalog::discover(&root).unwrap();
        let source = catalog.source_keys().next().unwrap().clone();
        let mut documents = Vec::new();
        catalog
            .scan_source(&source, |document| {
                documents.push(document);
                Ok(())
            })
            .unwrap();
        assert_eq!(documents[0].body, prompt);
        assert!(documents[0].body.ends_with("kimi-tail"));

        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();
        let hydrated = KimiSourceBackedResolver::new(catalog)
            .hydrate_event(&request)
            .unwrap();
        assert_eq!(hydrated.provider_bytes, prompt.as_bytes());
    }

    #[test]
    fn kimi_source_backed_auxiliary_mutation_invalidates_exact_revision_not_identity() {
        let (_temp, root, _wire) = fixture("cold exact message");
        let initial = KimiSourceBackedCatalog::discover(&root).unwrap();
        let source = initial.source_keys().next().unwrap().clone();
        let mut initial_documents = Vec::new();
        let initial_certificate = initial
            .scan_source(&source, |document| {
                initial_documents.push(document);
                Ok(())
            })
            .unwrap();
        let stale_request = EventHydrationRequest::new(
            initial_documents[0].event_id,
            initial_documents[0].locator.clone(),
        )
        .unwrap();

        let state = root.join("sessions/work/session-1/state.json");
        fs::write(
            &state,
            json!({
                "createdAt": "2026-07-17T12:00:00Z",
                "title": "mutated auxiliary authority",
                "agents": {"main": {"type": "main"}}
            })
            .to_string(),
        )
        .unwrap();
        let stale = KimiSourceBackedResolver::new(initial)
            .hydrate_event(&stale_request)
            .unwrap_err();
        assert_eq!(stale.kind, HydrationFailureKind::StaleSourceEvidence);

        let refreshed = KimiSourceBackedCatalog::discover(&root).unwrap();
        let refreshed_source = refreshed.source_keys().next().unwrap().clone();
        let mut refreshed_documents = Vec::new();
        let refreshed_certificate = refreshed
            .scan_source(&refreshed_source, |document| {
                refreshed_documents.push(document);
                Ok(())
            })
            .unwrap();
        assert_eq!(source, refreshed_source);
        assert_eq!(
            initial_documents
                .iter()
                .map(|document| (document.session_id, document.event_id))
                .collect::<Vec<_>>(),
            refreshed_documents
                .iter()
                .map(|document| (document.session_id, document.event_id))
                .collect::<Vec<_>>()
        );
        assert_ne!(
            initial_certificate.observation().revision(),
            refreshed_certificate.observation().revision()
        );
        let refreshed_request = EventHydrationRequest::new(
            refreshed_documents[0].event_id,
            refreshed_documents[0].locator.clone(),
        )
        .unwrap();
        assert!(KimiSourceBackedResolver::new(refreshed)
            .hydrate_event(&refreshed_request)
            .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn compound_authority_kimi_rejects_missing_auxiliary_sibling_and_ancestor_swaps() {
        let (_temp, root, _wire) = fixture("cold exact message");
        let state = root.join("sessions/work/session-1/state.json");
        fs::remove_file(&state).unwrap();
        let missing = KimiSourceBackedCatalog::discover(&root).unwrap();
        fs::write(&state, r#"{"title":"appeared"}"#).unwrap();
        assert!(!missing.revalidate_inventory().unwrap());

        let catalog = KimiSourceBackedCatalog::discover(&root).unwrap();
        let source = catalog.source_keys().next().unwrap().clone();
        let mut documents = Vec::new();
        catalog
            .scan_source(&source, |document| {
                documents.push(document);
                Ok(())
            })
            .unwrap();
        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();

        let state_bytes = fs::read(&state).unwrap();
        fs::rename(&state, state.with_extension("retired")).unwrap();
        fs::write(&state, state_bytes).unwrap();
        assert!(KimiSourceBackedResolver::new(catalog.clone())
            .hydrate_event(&request)
            .is_err());

        let retired_root = root.with_extension("retired");
        fs::rename(&root, &retired_root).unwrap();
        fs::create_dir_all(root.join("sessions/work/session-1/agents/main")).unwrap();
        fs::copy(
            retired_root.join("session_index.jsonl"),
            root.join("session_index.jsonl"),
        )
        .unwrap();
        fs::copy(
            retired_root.join("sessions/work/session-1/state.json"),
            root.join("sessions/work/session-1/state.json"),
        )
        .unwrap();
        fs::copy(
            retired_root.join("sessions/work/session-1/agents/main/wire.jsonl"),
            root.join("sessions/work/session-1/agents/main/wire.jsonl"),
        )
        .unwrap();
        assert!(KimiSourceBackedResolver::new(catalog)
            .hydrate_event(&request)
            .is_err());
    }
}

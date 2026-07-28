//! Source-backed Kimi Code CLI projection and exact hydration.
//!
//! Kimi's authority is compound: one `wire.jsonl` leaf is interpreted together
//! with its session `state.json` and the root `session_index.jsonl`. This module
//! certifies that compound observation while leaving generation lifecycle and
//! publication to the shared coordinator.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceInventory,
    ContentSourceResolver, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, PositionStability, ProjectionContractError,
    ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput, SourceAnchor,
    SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{IndexError, LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::normalization::provider_local_preview, CaptureError, KIMI_CODE_CLI_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::super::event::kimi_event_role;
use super::*;

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
            let provider_bytes = read_exact_record(&mut file, request.locator(), &coordinate)
                .map_err(map_hydration_error)?;
            hydrated.push(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
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
    let (state_path, index_path) =
        super::super::layout::complete_content_auxiliary_paths(&display_path)?;
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
    if file.len() > super::super::layout::KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES as u64 {
        return Err(CaptureError::InvalidPayload(
            "Kimi source-backed auxiliary file exceeds its bounded layout limit".to_owned(),
        )
        .into());
    }
    Ok(Some(file.read_all_bounded(
        super::super::layout::KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES,
    )?))
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
        revision: Some(KimiFrozenFileMetadata::from_metadata(file.metadata())?.revision_component()),
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
    let mut event_type = kimi_event_type(record_type, value);
    let role = kimi_event_role(record_type, value, event_type);
    let occurred_at =
        kimi_record_timestamp(value, fallback_timestamp).unwrap_or(fallback_timestamp);
    let body = if event_type == EventType::ToolOutput {
        let output = kimi_output_metadata(
            value,
            usize::try_from(ordinal)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(KimiSourceBackedError::CountOverflow)?,
            compound.native.session.cwd.as_deref(),
        );
        if !matches!(
            output.outcome.outcome,
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
    let body = provider_local_preview(&body, MAX_BODY_PREVIEW_CHARS).0;
    if body.is_empty() {
        return Ok(None);
    }

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
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(KimiSourceBackedError::CountOverflow)?;
    let native_event_id = kimi_legacy_provider_event_hash(record_type, value, line_number);
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
    let touches = kimi_file_touches(
        value,
        event_type,
        occurred_at,
        Some(ordinal),
        ordinal << 16,
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
        touched_files: touches
            .touches
            .into_iter()
            .map(|touch| touch.path)
            .collect(),
    }))
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

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
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
                "input": "cold exact message"
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
        let (_temp, root, _wire) = fixture();
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
        let value: Value =
            serde_json::from_slice(json_record_bytes(&hydrated.provider_bytes)).unwrap();
        assert_eq!(value["input"], "cold exact message");
    }

    #[test]
    fn kimi_source_backed_auxiliary_mutation_invalidates_exact_revision_not_identity() {
        let (_temp, root, _wire) = fixture();
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
        let (_temp, root, _wire) = fixture();
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

//! Source-backed Kimi Code CLI projection.
//!
//! Kimi's authority is compound: one `wire.jsonl` leaf is interpreted together
//! with its session `state.json` and the root `session_index.jsonl`. This module
//! validates that compound input while leaving generation lifecycle and
//! publication to the shared coordinator.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, EventType, NativeItemKey, NativeSessionKey, PositionStability,
    ProjectionContractError, SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId,
    TypedKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::family::jsonl::{
        JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlRecordRef,
    },
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        },
        tool_input,
    },
    CaptureError, OutputObservationKind, KIMI_CODE_CLI_SOURCE_FORMAT,
};

use super::super::{
    event::{
        kimi_event_role, kimi_event_text, kimi_event_type, kimi_legacy_provider_event_hash,
        kimi_output_content, kimi_record_timestamp,
    },
    layout::{
        auxiliary_paths, canonical_source_root_for_wire, KimiWireRoute,
        KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES,
    },
    source::KimiWireObservation,
};

const KIMI_SOURCE_SCHEMA_VARIANT: &str = "compound-wire-tree-v1";
const KIMI_SOURCE_ANCHOR_NAMESPACE: &str = "kimi-code-cli-wire-lineage-v1";
const KIMI_NATIVE_SESSION_NAMESPACE: &str = "kimi-code-cli-session-v1";
const KIMI_NATIVE_EVENT_POSITION_KIND: &str = "kimi-code-cli-wire-ordinal-v1";
const KIMI_LOGICAL_SESSION_KIND: &str = "agent-session";
const KIMI_LOGICAL_EVENT_KIND: &str = "wire-event";
const KIMI_SOURCE_PARSER_REVISION: &str = "kimi-code-cli-source-backed-v2";
const KIMI_DISCOVERY_MAX_DEPTH: usize = 16;
const KIMI_DISCOVERY_MAX_ENTRIES: usize = 65_536;

struct KimiInventory {
    paths: BTreeSet<PathBuf>,
    source_root: PathBuf,
    root_missing: bool,
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
    Core(#[from] CoreRecordError),
    #[error("the Kimi source inventory changed while it was being certified")]
    InventoryChanged,
    #[error("the Kimi source changed while it was being scanned")]
    SourceChanged,
    #[error("duplicate Kimi provider session lineage {0}")]
    DuplicateLineage(String),
    #[error("Kimi source-backed accounting overflowed")]
    CountOverflow,
}

pub(crate) type KimiSourceBackedResult<T> = std::result::Result<T, KimiSourceBackedError>;

mod records;

use records::core_record;

#[derive(Clone, Debug)]
struct KimiCompoundObservation {
    native: KimiWireObservation,
    source: SourceKey,
    relative_file_key: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KimiSourceLeaf {
    relative_path: PathBuf,
    source: SourceKey,
    provider_session_id: String,
    relative_file_key: Vec<u8>,
}

#[derive(Clone, Debug)]
struct CatalogSnapshot {
    leaves: BTreeMap<SourceKey, KimiSourceLeaf>,
}

#[derive(Debug)]
struct AdmittedKimiCompound {
    compound: KimiCompoundObservation,
    state: Option<OpenedProviderSourceFile>,
    index: Option<OpenedProviderSourceFile>,
}

impl AdmittedKimiCompound {
    fn revalidate(&self, authority: &ProviderSourceRoot) -> KimiSourceBackedResult<()> {
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

#[derive(Debug, Clone, Copy)]
struct KimiJsonlAdapter;

fn kimi_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(KimiJsonlAdapter)
}

impl JsonlFamilyAdapter for KimiJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::KimiCodeCli
    }

    fn source_format(&self) -> &'static str {
        KIMI_CODE_CLI_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        KIMI_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        KIMI_SOURCE_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        let inventory = discover_kimi_wire_files(root).map_err(capture_error)?;
        if inventory.root_missing {
            return JsonlFamilyInventory::missing(self.provider(), root);
        }
        let authority =
            Arc::new(ProviderSourceRoot::open(&inventory.source_root).map_err(capture_error)?);
        let snapshot = bind_snapshot(inventory, &authority).map_err(capture_error)?;
        let mut leaves = Vec::with_capacity(snapshot.leaves.len());
        for leaf in snapshot.leaves.into_values() {
            leaves.push(JsonlFamilyLeaf::observe(
                leaf.source.clone(),
                authority.named_path().join(&leaf.relative_path),
                Arc::clone(&authority),
                leaf.relative_path.clone(),
                TypedKey::bytes(serde_json::to_vec(&leaf)?).map_err(capture_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_family_leaf(leaf)?;
        let admitted = admit_compound_leaf_from_opened(
            leaf.authority(),
            &binding.relative_path,
            source_file.as_ref(),
        )
        .map_err(capture_error)?;
        if !admitted.compound.source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let AdmittedKimiCompound {
            compound,
            state,
            index,
        } = admitted;
        let session_id =
            session_identity(leaf.source(), &binding.provider_session_id).map_err(capture_error)?;
        let fallback_timestamp = compound
            .native
            .session
            .started_at
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        Ok(Box::new(KimiProjector {
            compound,
            session_id,
            fallback_timestamp,
            authority: Arc::clone(leaf.authority()),
            state,
            index,
        }))
    }
}

struct KimiProjector {
    compound: KimiCompoundObservation,
    session_id: StableEntityId,
    fallback_timestamp: DateTime<Utc>,
    authority: Arc<ProviderSourceRoot>,
    state: Option<OpenedProviderSourceFile>,
    index: Option<OpenedProviderSourceFile>,
}

impl JsonlFamilyProjector for KimiProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if value.get("type").and_then(Value::as_str) == Some("metadata") {
            return Ok(());
        }
        let evidence = record.evidence();
        if let Some(document) = core_record(
            &self.compound,
            self.session_id,
            evidence.physical_ordinal(),
            &value,
            self.fallback_timestamp,
        )
        .map_err(capture_error)?
        {
            emit(document)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> crate::Result<()> {
        if let Some(state) = &self.state {
            state.revalidate()?;
        }
        if let Some(index) = &self.index {
            index.revalidate()?;
        }
        self.authority.revalidate()
    }
}

fn decode_family_leaf(leaf: &JsonlFamilyLeaf) -> crate::Result<KimiSourceLeaf> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Kimi family leaf binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn capture_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(crate) struct KimiSourceBackedCatalog;
pub(crate) struct KimiSourceBackedResolver(Arc<dyn JsonlFamilyAdapter>);

impl KimiSourceBackedCatalog {
    pub(crate) fn shared() -> KimiSourceBackedResolver {
        KimiSourceBackedResolver(kimi_jsonl_adapter())
    }
}

impl KimiSourceBackedResolver {
    pub(crate) fn into_shared(self) -> Arc<dyn JsonlFamilyAdapter> {
        self.0
    }
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

fn bind_snapshot(
    inventory: KimiInventory,
    authority: &ProviderSourceRoot,
) -> KimiSourceBackedResult<CatalogSnapshot> {
    let source_root = inventory.source_root;
    let mut leaves = BTreeMap::new();
    let mut native_lineages = BTreeSet::new();
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
    Ok(CatalogSnapshot { leaves })
}

fn admit_compound_leaf(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
) -> KimiSourceBackedResult<AdmittedKimiCompound> {
    let wire = authority.open_file(relative_path)?;
    let admitted = admit_compound_leaf_from_opened(authority, relative_path, &wire)?;
    wire.revalidate()?;
    Ok(admitted)
}

fn admit_compound_leaf_from_opened(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
    wire: &OpenedProviderSourceFile,
) -> KimiSourceBackedResult<AdmittedKimiCompound> {
    let display_path = authority.named_path().join(relative_path);
    let (state_path, index_path) = auxiliary_paths(&display_path)?;
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
    Ok(AdmittedKimiCompound {
        compound: KimiCompoundObservation {
            native,
            source,
            relative_file_key,
        },
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

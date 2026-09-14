use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use ctx_history_core::{CORE_RECORD_VERSION, IDENTITY_VERSION};
use ctx_history_index_generation::{
    load_manifest_bytes, load_manifest_metadata, manifest_path, sha256_hex, write_manifest_bytes,
};
use serde::{Deserialize, Serialize};
use tantivy::{IndexMeta, Searcher};

use crate::{
    expected_source_generation_policy_hash, is_generation_id, validate_core_contract_fingerprint,
    CommitPayload, GenerationManifest, IndexError, Result, SourceCoreRecordAggregate,
    COMMIT_PAYLOAD_VERSION, GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION,
    LEXICAL_SCHEMA_VERSION,
};

use ctx_history_core::CertifiedSource;

const MAX_COMMIT_PAYLOAD_BYTES: usize = 256;
// Payload v2 carried up to 48 KiB of opaque metadata encoded as base64.
const MAX_RELEASED_V2_METADATA_ENCODED_BYTES: usize = 64 * 1024;
const MAX_RELEASED_V2_COMMIT_PAYLOAD_BYTES: usize = MAX_RELEASED_V2_METADATA_ENCODED_BYTES + 256;
const MANIFEST_FLAT_DELTA_STORAGE: &str = "ctx-manifest-flat-delta-v1";
const MANIFEST_FLAT_DELTA_PREFIX: &[u8] = br#"{"storage_format":"ctx-manifest-flat-delta-v1","#;
const MAX_MANIFEST_DELTA_CHANGES: usize = 64;
const MAX_MANIFEST_DELTA_BYTES: usize = 1024 * 1024;

type ManifestCacheKey = (PathBuf, String);
static MANIFEST_CACHE: OnceLock<Mutex<BTreeMap<ManifestCacheKey, ManifestCacheEntry>>> =
    OnceLock::new();

/// Drops process-local manifest identity snapshots after a synchronized,
/// metadata-only permission repair. Live publications retain their immutable
/// materialization; subsequent opens authenticate the repaired files anew.
#[doc(hidden)]
pub fn clear_manifest_cache_for_root(root: &Path) -> Result<()> {
    let Some(cache) = MANIFEST_CACHE.get() else {
        return Ok(());
    };
    cache
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?
        .retain(|(cached_root, _), _| cached_root != root);
    Ok(())
}

#[derive(Clone)]
struct ManifestCacheEntry {
    manifest: Weak<GenerationManifest>,
    identity: ManifestFileIdentity,
}

#[derive(Clone, PartialEq, Eq)]
struct ManifestFileIdentity {
    length: u64,
    readonly: bool,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(windows)]
    last_write_time: u64,
    #[cfg(windows)]
    attributes: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredManifestFlatDeltaV1 {
    storage_format: String,
    base_generation_id: String,
    indexed_documents: u64,
    certified_source_bytes: u64,
    source_count: usize,
    changes: Vec<StoredManifestSourceChangeV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredManifestSourceChangeV1 {
    source_identity: [u8; 32],
    source: CertifiedSource,
    aggregate: SourceCoreRecordAggregate,
}

#[derive(Deserialize)]
struct StoredManifestVersion {
    manifest_version: u32,
}

pub struct PreparedManifest {
    generation_id: String,
    bytes: Vec<u8>,
    materialized: Arc<GenerationManifest>,
    base_fence: Option<(String, ManifestFileIdentity)>,
}

impl PreparedManifest {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn verify_persisted(&self, root: &Path) -> Result<()> {
        if load_manifest_bytes(root, &self.generation_id)? != self.bytes {
            return Err(IndexError::NonCanonicalManifest);
        }
        if let Some((generation_id, expected)) = &self.base_fence {
            if capture_manifest_identity(root, generation_id)? != *expected {
                return Err(IndexError::NonCanonicalManifest);
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct LoadedPublication {
    generation_id: String,
    manifest: Arc<GenerationManifest>,
}

impl LoadedPublication {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    #[doc(hidden)]
    pub fn into_parts(self) -> (String, Arc<GenerationManifest>) {
        (self.generation_id, self.manifest)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BorrowedCommitPayload<'a> {
    version: u32,
    #[serde(borrow)]
    generation_id: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BorrowedReleasedV2CommitPayload<'a> {
    version: u32,
    #[serde(borrow)]
    generation_id: &'a str,
    #[serde(borrow)]
    publication_metadata: Option<&'a str>,
}

#[derive(Debug)]
struct DecodedCommitPayload {
    generation_id: String,
}

pub fn load_publication_for_metas(root: &Path, metas: &IndexMeta) -> Result<LoadedPublication> {
    let payload = decode_commit_payload(
        metas
            .payload
            .as_deref()
            .ok_or(IndexError::MissingCommitPayload)?,
    )?;
    let loaded = load_materialized_manifest(root, &payload.generation_id, 0)?;
    Ok(LoadedPublication {
        generation_id: payload.generation_id,
        manifest: loaded,
    })
}

fn load_materialized_manifest(
    root: &Path,
    generation_id: &str,
    depth: usize,
) -> Result<Arc<GenerationManifest>> {
    if depth > 128 {
        return Err(IndexError::NonCanonicalManifest);
    }
    let key = (root.to_path_buf(), generation_id.to_owned());
    // Recursive bases are content-addressed immutable snapshots already
    // authenticated in this process. Top-level opens still authenticate their
    // named descriptor before reusing an in-memory materialization.
    if depth != 0 {
        if let Some(manifest) = cached_manifest(&key)? {
            return Ok(manifest);
        }
    }
    let bytes = load_manifest_bytes(root, generation_id)?;
    if let Some(manifest) = cached_manifest(&key)? {
        return Ok(manifest);
    }
    let manifest = if bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        let delta: StoredManifestFlatDeltaV1 = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&delta)? != bytes
            || delta.storage_format != MANIFEST_FLAT_DELTA_STORAGE
            || !is_generation_id(&delta.base_generation_id)
            || delta.changes.is_empty()
            || delta.changes.len() > MAX_MANIFEST_DELTA_CHANGES
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        let base = load_materialized_manifest(root, &delta.base_generation_id, depth + 1)?;
        let manifest = materialize_delta(
            base.as_ref(),
            delta.indexed_documents,
            delta.certified_source_bytes,
            delta.source_count,
            delta.changes,
        )?;
        manifest
    } else {
        let stored_version: StoredManifestVersion = serde_json::from_slice(&bytes)?;
        if stored_version.manifest_version != GENERATION_MANIFEST_VERSION {
            return Err(IndexError::UnsupportedManifest(
                stored_version.manifest_version,
            ));
        }
        let manifest: GenerationManifest = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&manifest)? != bytes {
            return Err(IndexError::NonCanonicalManifest);
        }
        validate_manifest_contract(&manifest)?;
        manifest
    };
    let manifest = Arc::new(manifest);
    let mut cache = MANIFEST_CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?;
    cache.retain(|_, entry| entry.manifest.strong_count() != 0);
    cache.insert(
        key,
        ManifestCacheEntry {
            manifest: Arc::downgrade(&manifest),
            identity: capture_manifest_identity(root, generation_id)?,
        },
    );
    Ok(manifest)
}

fn cached_manifest(key: &ManifestCacheKey) -> Result<Option<Arc<GenerationManifest>>> {
    let entry = MANIFEST_CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?
        .get(key)
        .cloned();
    let Some(entry) = entry else {
        return Ok(None);
    };
    let Some(manifest) = entry.manifest.upgrade() else {
        return Ok(None);
    };
    if capture_manifest_identity(&key.0, &key.1)? != entry.identity {
        return Err(IndexError::NonCanonicalManifest);
    }
    Ok(Some(manifest))
}

fn capture_manifest_identity(root: &Path, generation_id: &str) -> Result<ManifestFileIdentity> {
    let metadata = load_manifest_metadata(root, generation_id)?;
    if !metadata.file_type().is_file() {
        return Err(IndexError::NonCanonicalManifest);
    }
    manifest_identity_from_metadata(&metadata)
}

fn manifest_identity_from_metadata(metadata: &Metadata) -> Result<ManifestFileIdentity> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt as _;

    Ok(ManifestFileIdentity {
        length: metadata.len(),
        readonly: metadata.permissions().readonly(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        changed_seconds: metadata.ctime(),
        #[cfg(unix)]
        changed_nanoseconds: metadata.ctime_nsec(),
        #[cfg(windows)]
        creation_time: metadata.creation_time(),
        #[cfg(windows)]
        last_write_time: metadata.last_write_time(),
        #[cfg(windows)]
        attributes: metadata.file_attributes(),
    })
}

fn materialize_delta(
    base: &GenerationManifest,
    indexed_documents: u64,
    certified_source_bytes: u64,
    source_count: usize,
    changes: Vec<StoredManifestSourceChangeV1>,
) -> Result<GenerationManifest> {
    if source_count != base.sources.len() {
        return Err(IndexError::NonCanonicalManifest);
    }
    let mut replacements = Vec::with_capacity(changes.len());
    let mut previous = None;
    for change in changes {
        if previous.is_some_and(|digest| digest >= change.source_identity) {
            return Err(IndexError::NonCanonicalManifest);
        }
        previous = Some(change.source_identity);
        let source_index = base
            .sources
            .binary_search_by_key(&change.source_identity, |source| {
                source.observation().source().identity().digest()
            })
            .map_err(|_| IndexError::NonCanonicalManifest)?;
        if change.source.observation().source().identity().digest() != change.source_identity {
            return Err(IndexError::NonCanonicalManifest);
        }
        let source_identity_hex = hex_digest(change.source_identity);
        let aggregate_index = base
            .core_record_aggregates
            .binary_search_by(|aggregate| {
                aggregate.source_identity_digest().cmp(&source_identity_hex)
            })
            .map_err(|_| IndexError::NonCanonicalManifest)?;
        if base.core_record_aggregates[aggregate_index].source_identity_digest()
            != change.aggregate.source_identity_digest()
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        if source_index != aggregate_index {
            return Err(IndexError::NonCanonicalManifest);
        }
        replacements.push((change.source, change.aggregate));
    }
    let materialized = base.apply_validated_source_replacements(replacements)?;
    if materialized.indexed_documents != indexed_documents
        || materialized.certified_source_bytes != certified_source_bytes
        || materialized.sources.len() != source_count
    {
        return Err(IndexError::NonCanonicalManifest);
    }
    Ok(materialized)
}

fn validate_manifest_contract(manifest: &GenerationManifest) -> Result<()> {
    if manifest.manifest_version != GENERATION_MANIFEST_VERSION {
        return Err(IndexError::UnsupportedManifest(manifest.manifest_version));
    }
    if manifest.identity_version != IDENTITY_VERSION
        || manifest.lexical_schema_version != LEXICAL_SCHEMA_VERSION
        || manifest.lexical_analyzer_version != LEXICAL_ANALYZER_VERSION
        || manifest.core_record_version != CORE_RECORD_VERSION
    {
        return Err(IndexError::GenerationContractMismatch {
            identity: manifest.identity_version,
            schema: manifest.lexical_schema_version,
            analyzer: manifest.lexical_analyzer_version,
            core_record: manifest.core_record_version,
        });
    }
    validate_core_contract_fingerprint(&manifest.core_record_contract_fingerprint)?;
    let expected_policy_hash = expected_source_generation_policy_hash()?;
    if manifest.policy_schema_hash != expected_policy_hash {
        return Err(IndexError::GenerationPolicyMismatch {
            expected: expected_policy_hash,
            actual: manifest.policy_schema_hash.clone(),
        });
    }
    manifest.validate_contract()
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn canonical_commit_payload(generation_id: &str) -> Result<String> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    Ok(serde_json::to_string(&CommitPayload {
        version: COMMIT_PAYLOAD_VERSION,
        generation_id: generation_id.to_owned(),
    })?)
}

fn decode_commit_payload(encoded: &str) -> Result<DecodedCommitPayload> {
    if is_released_v2_commit_payload(encoded) {
        return Err(IndexError::UnsupportedCommitPayload(2));
    }
    if encoded.len() > MAX_COMMIT_PAYLOAD_BYTES {
        return Err(IndexError::CommitPayloadTooLarge {
            actual: encoded.len(),
            maximum: MAX_COMMIT_PAYLOAD_BYTES,
        });
    }
    let payload: BorrowedCommitPayload<'_> = serde_json::from_str(encoded)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    if serde_json::to_string(&payload)? != encoded {
        return Err(IndexError::NonCanonicalCommitPayload);
    }
    Ok(DecodedCommitPayload {
        generation_id: payload.generation_id.to_owned(),
    })
}

fn is_released_v2_commit_payload(encoded: &str) -> bool {
    // Only recognize the released v2 envelope for source rebuild. Its
    // removed metadata stays opaque, and current payload validation is unchanged.
    if encoded.len() > MAX_RELEASED_V2_COMMIT_PAYLOAD_BYTES
        || !encoded.starts_with(r#"{"version":2,"#)
    {
        return false;
    }
    let Ok(payload) = serde_json::from_str::<BorrowedReleasedV2CommitPayload<'_>>(encoded) else {
        return false;
    };
    payload.version == 2
        && is_generation_id(payload.generation_id)
        && payload
            .publication_metadata
            .is_none_or(|metadata| metadata.len() <= MAX_RELEASED_V2_METADATA_ENCODED_BYTES)
}

pub fn reconcile_commit_error(
    index: &tantivy::Index,
    expected_generation_id: &str,
    previous_generation_id: Option<&str>,
    commit_error: tantivy::TantivyError,
) -> Result<u64> {
    let metas = index.load_metas().map_err(|reconcile_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; reopening meta.json failed: {reconcile_error}"),
        }
    })?;
    let visible_generation = payload_generation_id(&metas).map_err(|payload_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; candidate payload is invalid: {payload_error}"),
        }
    })?;
    if visible_generation.as_deref() == Some(expected_generation_id) {
        return Ok(metas.opstamp);
    }
    if visible_generation.as_deref() == previous_generation_id
        || (previous_generation_id.is_none()
            && visible_generation.is_none()
            && metas.segments.is_empty())
    {
        return Err(IndexError::Tantivy(commit_error));
    }
    Err(IndexError::CommittedGenerationNeedsRecovery {
        generation_id: expected_generation_id.to_owned(),
        stage: "candidate commit reconciliation",
        detail: format!(
            "{commit_error}; expected old generation {:?} or candidate generation, found {:?}",
            previous_generation_id, visible_generation
        ),
    })
}

pub fn payload_generation_id(metas: &IndexMeta) -> Result<Option<String>> {
    let Some(payload) = metas.payload.as_deref() else {
        return Ok(None);
    };
    Ok(Some(decode_commit_payload(payload)?.generation_id))
}

pub fn write_manifest(
    root: &Path,
    generation_id: &str,
    manifest: &GenerationManifest,
) -> Result<()> {
    if manifest_path(root, generation_id).is_file() {
        let retained = load_materialized_manifest(root, generation_id, 0)?;
        if serde_json::to_vec(retained.as_ref())? == serde_json::to_vec(manifest)? {
            return Ok(());
        }
        return Err(IndexError::NonCanonicalManifest);
    }
    let bytes = serde_json::to_vec(manifest)?;
    Ok(write_manifest_bytes(root, generation_id, &bytes)?)
}

pub fn prepare_successor_manifest(
    root: &Path,
    manifest: Arc<GenerationManifest>,
    base: Option<(&str, &GenerationManifest)>,
) -> Result<PreparedManifest> {
    let full = || -> Result<PreparedManifest> {
        let bytes = serde_json::to_vec(&manifest)?;
        Ok(PreparedManifest {
            generation_id: sha256_hex(&bytes),
            bytes,
            materialized: Arc::clone(&manifest),
            base_fence: None,
        })
    };
    let Some((base_generation_id, base)) = base else {
        return full();
    };
    if manifest.exact_snapshot_eq(base) {
        return Ok(PreparedManifest {
            generation_id: base_generation_id.to_owned(),
            bytes: load_manifest_bytes(root, base_generation_id)?,
            materialized: manifest,
            base_fence: None,
        });
    }
    if !is_generation_id(base_generation_id)
        || base.sources.len() != manifest.sources.len()
        || base.core_record_aggregates.len() != manifest.core_record_aggregates.len()
        || base.source_routes().len() != manifest.source_routes().len()
        || base
            .source_routes()
            .iter()
            .zip(manifest.source_routes())
            .any(|(base, current)| !base.exact_snapshot_eq(current))
        || base.manifest_version != manifest.manifest_version
        || base.identity_version != manifest.identity_version
        || base.core_record_version != manifest.core_record_version
        || base.core_record_contract_fingerprint != manifest.core_record_contract_fingerprint
        || base.lexical_schema_version != manifest.lexical_schema_version
        || base.lexical_analyzer_version != manifest.lexical_analyzer_version
        || base.policy_schema_hash != manifest.policy_schema_hash
        || base.generation_state() != manifest.generation_state()
        || base.automatic_provider_discovery() != manifest.automatic_provider_discovery()
        || base.provider_root_config_digest() != manifest.provider_root_config_digest()
        || base.provider_roots() != manifest.provider_roots()
        || base.detached_released_provider_roots() != manifest.detached_released_provider_roots()
    {
        return full();
    }
    let mut changes = Vec::new();
    for ((base_source, source), (base_aggregate, aggregate)) in
        base.sources.iter().zip(&manifest.sources).zip(
            base.core_record_aggregates
                .iter()
                .zip(&manifest.core_record_aggregates),
        )
    {
        let source_identity = source.observation().source().identity().digest();
        if base_source.observation().source().identity().digest() != source_identity
            || base_aggregate.source_identity_digest() != aggregate.source_identity_digest()
            || aggregate.source_identity_digest() != hex_digest(source_identity)
        {
            return full();
        }
        let source_changed =
            !base_source.shares_immutable_parts_with(source) && base_source != source;
        if source_changed || base_aggregate != aggregate {
            changes.push(StoredManifestSourceChangeV1 {
                source_identity,
                source: source.clone(),
                aggregate: aggregate.clone(),
            });
        }
    }
    if changes.is_empty() || changes.len() > MAX_MANIFEST_DELTA_CHANGES {
        return full();
    }
    let (base_generation_id, mut accumulated) =
        accumulated_manifest_changes(root, base_generation_id)?;
    for change in changes {
        accumulated.insert(change.source_identity, change);
    }
    if accumulated.len() > MAX_MANIFEST_DELTA_CHANGES {
        return full();
    }
    let base_fence = Some((
        base_generation_id.clone(),
        capture_manifest_identity(root, &base_generation_id)?,
    ));
    let delta = StoredManifestFlatDeltaV1 {
        storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
        base_generation_id,
        indexed_documents: manifest.indexed_documents,
        certified_source_bytes: manifest.certified_source_bytes,
        source_count: manifest.sources.len(),
        changes: accumulated.into_values().collect(),
    };
    let bytes = serde_json::to_vec(&delta)?;
    if bytes.len() > MAX_MANIFEST_DELTA_BYTES {
        return full();
    }
    Ok(PreparedManifest {
        generation_id: sha256_hex(&bytes),
        bytes,
        materialized: manifest,
        base_fence,
    })
}

fn accumulated_manifest_changes(
    root: &Path,
    generation_id: &str,
) -> Result<(String, BTreeMap<[u8; 32], StoredManifestSourceChangeV1>)> {
    let mut prefix = [0_u8; 64];
    let mut file = File::open(manifest_path(root, generation_id)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            IndexError::MissingManifest(generation_id.to_owned())
        } else {
            IndexError::Io(error)
        }
    })?;
    let prefix_len = file.read(&mut prefix)?;
    let prefix = &prefix[..prefix_len];
    if !prefix.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        // `base` was materialized through the authenticated loader before this
        // helper was called, so reaching its non-delta anchor does not require
        // reading and hashing the potentially corpus-sized full manifest again.
        return Ok((generation_id.to_owned(), BTreeMap::new()));
    }

    let bytes = load_manifest_bytes(root, generation_id)?;
    if bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        let delta: StoredManifestFlatDeltaV1 = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&delta)? != bytes
            || delta.storage_format != MANIFEST_FLAT_DELTA_STORAGE
            || !is_generation_id(&delta.base_generation_id)
            || delta.changes.is_empty()
            || delta.changes.len() > MAX_MANIFEST_DELTA_CHANGES
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        return Ok((
            delta.base_generation_id,
            delta
                .changes
                .into_iter()
                .map(|change| (change.source_identity, change))
                .collect(),
        ));
    }
    Err(IndexError::NonCanonicalManifest)
}

pub fn write_prepared_manifest(root: &Path, manifest: &PreparedManifest) -> Result<()> {
    write_manifest_bytes(root, &manifest.generation_id, &manifest.bytes)?;
    let key = (root.to_path_buf(), manifest.generation_id.clone());
    let mut cache = MANIFEST_CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?;
    cache.retain(|_, entry| entry.manifest.strong_count() != 0);
    cache.insert(
        key,
        ManifestCacheEntry {
            manifest: Arc::downgrade(&manifest.materialized),
            identity: capture_manifest_identity(root, &manifest.generation_id)?,
        },
    );
    Ok(())
}

pub fn meta_generation(metas: &IndexMeta) -> BTreeMap<String, Option<u64>> {
    metas
        .segments
        .iter()
        .map(|segment| (segment.id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

pub fn searcher_generation(searcher: &Searcher) -> BTreeMap<String, Option<u64>> {
    searcher
        .segment_readers()
        .iter()
        .map(|segment| (segment.segment_id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

#[cfg(test)]
#[path = "manifest/tests.rs"]
mod tests;

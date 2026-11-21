use std::{
    collections::BTreeMap,
    fs::Metadata,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use ctx_history_core::{CORE_RECORD_VERSION, IDENTITY_VERSION};
use ctx_history_index_generation::{
    load_manifest_bytes, load_manifest_metadata, manifest_path, sha256_hex, write_manifest_bytes,
};
use serde::{Deserialize, Serialize};
use tantivy::{IndexMeta, Searcher};

mod delta;

use delta::{
    decode_validated_delta, materialize_delta, prepare_flat_delta, MANIFEST_FLAT_DELTA_PREFIX,
};
#[cfg(test)]
use delta::{
    StoredManifestFlatDeltaV1, StoredManifestRouteAdditionV1, StoredManifestSourceChangeV1,
    MANIFEST_FLAT_DELTA_STORAGE, MAX_MANIFEST_DELTA_BYTES, MAX_MANIFEST_DELTA_CHANGES,
};

use crate::{
    is_generation_id, validate_core_contract_fingerprint, CommitPayload, GenerationManifest,
    IndexError, Result, COMMIT_PAYLOAD_VERSION, GENERATION_MANIFEST_VERSION,
    LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
};

mod summary;
pub use summary::{
    ensure_manifest_summary, write_manifest_summary_for_generation, ManifestSummary,
    MANIFEST_SUMMARY_FORMAT,
};

const MAX_COMMIT_PAYLOAD_BYTES: usize = 256;
// Payload v2 carried up to 48 KiB of opaque metadata encoded as base64.
const MAX_RELEASED_V2_METADATA_ENCODED_BYTES: usize = 64 * 1024;
const MAX_RELEASED_V2_COMMIT_PAYLOAD_BYTES: usize = MAX_RELEASED_V2_METADATA_ENCODED_BYTES + 256;

type ManifestCacheKey = (PathBuf, String);
static MANIFEST_CACHE: OnceLock<Mutex<BTreeMap<ManifestCacheKey, ManifestCacheEntry>>> =
    OnceLock::new();

fn manifest_cache_root(root: &Path) -> PathBuf {
    // Reader leases can name this directory through /proc/self/fd/<n>. Share
    // the writer's key without using the normalized path for authenticated I/O.
    // A cache-key normalization failure must not invalidate an opened capability.
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Drops process-local manifest identity snapshots after a synchronized,
/// metadata-only permission repair. Live publications retain their immutable
/// materialization; subsequent opens authenticate the repaired files anew.
#[doc(hidden)]
pub fn clear_manifest_cache_for_root(root: &Path) -> Result<()> {
    let Some(cache) = MANIFEST_CACHE.get() else {
        return Ok(());
    };
    let root = manifest_cache_root(root);
    cache
        .lock()
        .map_err(|_| IndexError::NonCanonicalManifest)?
        .retain(|(cached_root, _), _| cached_root != &root);
    Ok(())
}

#[derive(Clone)]
struct ManifestCacheEntry {
    manifest: Weak<GenerationManifest>,
    identity: ManifestFileIdentity,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ManifestFileIdentity {
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

#[derive(Deserialize)]
struct StoredManifestVersion {
    manifest_version: u32,
}

pub struct PreparedManifest {
    pub(super) generation_id: String,
    pub(super) bytes: Vec<u8>,
    pub(super) materialized: Arc<GenerationManifest>,
    pub(super) base_fence: Option<(String, ManifestFileIdentity)>,
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

#[derive(Debug)]
pub enum LoadedPublicationSummary {
    Summary(ManifestSummary),
    Full(LoadedPublication),
}

impl LoadedPublicationSummary {
    pub fn generation_id(&self) -> &str {
        match self {
            Self::Summary(summary) => &summary.generation_id,
            Self::Full(publication) => publication.generation_id(),
        }
    }

    pub fn lexical_schema_version(&self) -> u32 {
        match self {
            Self::Summary(summary) => summary.lexical_schema_version,
            Self::Full(publication) => publication.manifest().lexical_schema_version,
        }
    }

    pub fn indexed_documents(&self) -> u64 {
        match self {
            Self::Summary(summary) => summary.indexed_documents,
            Self::Full(publication) => publication.manifest().indexed_documents,
        }
    }
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

/// Read-only upgrade scope derived from the active generation's manifest
/// without opening or verifying the physical lexical index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveGenerationUpgradeScope {
    pub estimated_total_sources: usize,
    pub estimated_total_source_bytes: u64,
    pub estimated_total_documents: u64,
}

/// Best-effort read-only manifest scope lookup for an active generation whose
/// physical index failed compatibility checks (for example,
/// `IndexSettingsMismatch`). Never decodes unsupported manifest versions or
/// mutates index state.
pub fn active_generation_upgrade_scope(root: &Path) -> Option<ActiveGenerationUpgradeScope> {
    let pointer = ctx_history_index_generation::load_active_generation_pointer(root).ok()??;
    let manifest = load_materialized_manifest(root, pointer.active().generation_id(), 0).ok()?;
    Some(ActiveGenerationUpgradeScope {
        estimated_total_sources: manifest.sources.len(),
        estimated_total_source_bytes: manifest.certified_source_bytes,
        estimated_total_documents: manifest.indexed_documents,
    })
}

pub fn load_publication_summary_for_metas(
    root: &Path,
    metas: &IndexMeta,
) -> Result<LoadedPublicationSummary> {
    let payload = decode_commit_payload(
        metas
            .payload
            .as_deref()
            .ok_or(IndexError::MissingCommitPayload)?,
    )?;
    let key = (manifest_cache_root(root), payload.generation_id.clone());
    if let Some(manifest) = cached_manifest(root, &key)? {
        return Ok(LoadedPublicationSummary::Full(LoadedPublication {
            generation_id: payload.generation_id,
            manifest,
        }));
    }
    if let Some(summary) =
        summary::try_load_validated_manifest_summary(root, &payload.generation_id)?
    {
        return Ok(LoadedPublicationSummary::Summary(summary));
    }
    let loaded = load_materialized_manifest(root, &payload.generation_id, 0)?;
    Ok(LoadedPublicationSummary::Full(LoadedPublication {
        generation_id: payload.generation_id,
        manifest: loaded,
    }))
}

pub fn load_materialized_manifest_for_generation(
    root: &Path,
    generation_id: &str,
) -> Result<Arc<GenerationManifest>> {
    load_materialized_manifest(root, generation_id, 0)
}

fn load_materialized_manifest(
    root: &Path,
    generation_id: &str,
    depth: usize,
) -> Result<Arc<GenerationManifest>> {
    if depth > 128 {
        return Err(IndexError::NonCanonicalManifest);
    }
    let key = (manifest_cache_root(root), generation_id.to_owned());
    // Recursive bases are content-addressed immutable snapshots already
    // authenticated in this process. Top-level opens still authenticate their
    // named descriptor before reusing an in-memory materialization.
    if depth != 0 {
        if let Some(manifest) = cached_manifest(root, &key)? {
            return Ok(manifest);
        }
    }
    let bytes = load_manifest_bytes(root, generation_id)?;
    if let Some(manifest) = cached_manifest(root, &key)? {
        return Ok(manifest);
    }
    let manifest = if bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        let delta = decode_validated_delta(&bytes)?;
        let base = load_materialized_manifest(root, &delta.base_generation_id, depth + 1)?;
        materialize_delta(base.as_ref(), delta)?
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

fn cached_manifest(root: &Path, key: &ManifestCacheKey) -> Result<Option<Arc<GenerationManifest>>> {
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
    if capture_manifest_identity(root, &key.1)? != entry.identity {
        return Err(IndexError::NonCanonicalManifest);
    }
    Ok(Some(manifest))
}

pub(super) fn capture_manifest_identity(
    root: &Path,
    generation_id: &str,
) -> Result<ManifestFileIdentity> {
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

fn validate_manifest_contract(manifest: &GenerationManifest) -> Result<()> {
    if manifest.manifest_version != GENERATION_MANIFEST_VERSION {
        return Err(IndexError::UnsupportedManifest(manifest.manifest_version));
    }
    if manifest.identity_version != IDENTITY_VERSION
        || !matches!(manifest.lexical_schema_version, 22 | LEXICAL_SCHEMA_VERSION)
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
    // Only the additive import projection differs in the readable predecessor.
    // All other Core, analyzer and source-policy contracts must still match.
    let mut policy = crate::policy::current_source_generation_policy();
    policy.lexical.schema_revision = manifest.lexical_schema_version;
    let expected_policy_hash = policy.canonical_sha256()?;
    if manifest.policy_schema_hash != expected_policy_hash {
        return Err(IndexError::GenerationPolicyMismatch {
            expected: expected_policy_hash,
            actual: manifest.policy_schema_hash.clone(),
        });
    }
    manifest.validate_contract()
}

pub(super) fn hex_digest(digest: [u8; 32]) -> String {
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
            ensure_manifest_summary(root, generation_id, manifest);
            return Ok(());
        }
        return Err(IndexError::NonCanonicalManifest);
    }
    let bytes = serde_json::to_vec(manifest)?;
    write_manifest_bytes(root, generation_id, &bytes)?;
    write_manifest_summary_for_generation(root, generation_id, manifest, bytes.len() as u64);
    Ok(())
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
    // Source equality tracks lineage. Exact reuse requires matching exact
    // descriptors as well as full logical snapshot equality.
    if base.sources.len() == manifest.sources.len()
        && base
            .sources
            .iter()
            .zip(&manifest.sources)
            .all(|(base, current)| {
                base.observation()
                    .source()
                    .exact_descriptor_eq(current.observation().source())
            })
        && manifest.exact_snapshot_eq(base)
    {
        return Ok(PreparedManifest {
            generation_id: base_generation_id.to_owned(),
            bytes: load_manifest_bytes(root, base_generation_id)?,
            materialized: manifest,
            base_fence: None,
        });
    }
    if let Some(delta) = prepare_flat_delta(root, &manifest, base_generation_id, base)? {
        return Ok(delta);
    }
    full()
}

pub fn write_prepared_manifest(root: &Path, manifest: &PreparedManifest) -> Result<()> {
    write_manifest_bytes(root, &manifest.generation_id, &manifest.bytes)?;
    write_manifest_summary_for_generation(
        root,
        &manifest.generation_id,
        manifest.materialized.as_ref(),
        manifest.bytes.len() as u64,
    );
    let key = (manifest_cache_root(root), manifest.generation_id.clone());
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

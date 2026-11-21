use std::path::Path;

use ctx_history_core::{CORE_RECORD_VERSION, IDENTITY_VERSION};
use ctx_history_index_generation::{
    load_manifest_metadata, load_manifest_summary_bytes, write_manifest_summary_bytes,
};
use serde::{Deserialize, Serialize};

use crate::{
    validate_core_contract_fingerprint, GenerationManifest, IndexError, Result,
    GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
};

pub const MANIFEST_SUMMARY_FORMAT: &str = "ctx-manifest-summary-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSummary {
    pub format: String,
    pub generation_id: String,
    pub manifest_version: u32,
    pub identity_version: u16,
    pub core_record_version: u32,
    pub core_record_contract_fingerprint: String,
    pub lexical_schema_version: u32,
    pub lexical_analyzer_version: u32,
    pub policy_schema_hash: String,
    pub indexed_documents: u64,
    pub certified_source_bytes: u64,
    pub source_count: u64,
    pub manifest_bytes_len: u64,
}

impl ManifestSummary {
    pub fn from_manifest(
        generation_id: &str,
        manifest: &GenerationManifest,
        manifest_bytes_len: u64,
    ) -> Self {
        Self {
            format: MANIFEST_SUMMARY_FORMAT.to_owned(),
            generation_id: generation_id.to_owned(),
            manifest_version: manifest.manifest_version,
            identity_version: manifest.identity_version,
            core_record_version: manifest.core_record_version,
            core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
            lexical_schema_version: manifest.lexical_schema_version,
            lexical_analyzer_version: manifest.lexical_analyzer_version,
            policy_schema_hash: manifest.policy_schema_hash.clone(),
            indexed_documents: manifest.indexed_documents,
            certified_source_bytes: manifest.certified_source_bytes,
            source_count: manifest.sources.len() as u64,
            manifest_bytes_len,
        }
    }
}

/// Validates and loads the advisory summary sidecar for `generation_id`.
///
/// The authoritative manifest file MUST exist and be a regular file; a missing
/// manifest always fails closed rather than trusting an orphaned sidecar. Any
/// missing, corrupt, non-canonical, size-mismatched, or contract-mismatched
/// sidecar returns `Ok(None)` so callers fall back to loading and validating
/// the full manifest.
pub(super) fn try_load_validated_manifest_summary(
    root: &Path,
    generation_id: &str,
) -> Result<Option<ManifestSummary>> {
    let manifest_metadata = load_manifest_metadata(root, generation_id)?;
    if !manifest_metadata.file_type().is_file() {
        return Err(IndexError::NonCanonicalManifest);
    }
    let Some(bytes) = load_manifest_summary_bytes(root, generation_id)? else {
        return Ok(None);
    };
    let Ok(summary) = serde_json::from_slice::<ManifestSummary>(&bytes) else {
        return Ok(None);
    };
    let Ok(canonical) = serde_json::to_vec(&summary) else {
        return Ok(None);
    };
    if canonical != bytes
        || summary.format != MANIFEST_SUMMARY_FORMAT
        || summary.generation_id != generation_id
        || summary.manifest_bytes_len != manifest_metadata.len()
    {
        return Ok(None);
    }
    if !summary_matches_current_read_contract(&summary) {
        return Ok(None);
    }
    Ok(Some(summary))
}

fn summary_matches_current_read_contract(summary: &ManifestSummary) -> bool {
    if summary.manifest_version != GENERATION_MANIFEST_VERSION
        || summary.identity_version != IDENTITY_VERSION
        || !matches!(summary.lexical_schema_version, 22 | LEXICAL_SCHEMA_VERSION)
        || summary.lexical_analyzer_version != LEXICAL_ANALYZER_VERSION
        || summary.core_record_version != CORE_RECORD_VERSION
    {
        return false;
    }
    if validate_core_contract_fingerprint(&summary.core_record_contract_fingerprint).is_err() {
        return false;
    }
    let mut policy = crate::policy::current_source_generation_policy();
    policy.lexical.schema_revision = summary.lexical_schema_version;
    let Ok(expected_policy_hash) = policy.canonical_sha256() else {
        return false;
    };
    summary.policy_schema_hash == expected_policy_hash
}

/// Best-effort advisory write of `<generation_id>.summary.json`.
pub fn write_manifest_summary_for_generation(
    root: &Path,
    generation_id: &str,
    manifest: &GenerationManifest,
    manifest_bytes_len: u64,
) {
    let summary = ManifestSummary::from_manifest(generation_id, manifest, manifest_bytes_len);
    if let Ok(bytes) = serde_json::to_vec(&summary) {
        let _ = write_manifest_summary_bytes(root, generation_id, &bytes);
    }
}

/// Backfills or repairs `<generation_id>.summary.json` for an already-published
/// generation when the writer holds write authority and the sidecar is absent,
/// corrupt, symlinked, or mismatched.
pub fn ensure_manifest_summary(root: &Path, generation_id: &str, manifest: &GenerationManifest) {
    let Ok(metadata) = load_manifest_metadata(root, generation_id) else {
        return;
    };
    if !metadata.file_type().is_file() {
        return;
    }
    let expected = ManifestSummary::from_manifest(generation_id, manifest, metadata.len());
    if try_load_validated_manifest_summary(root, generation_id)
        .ok()
        .flatten()
        .as_ref()
        == Some(&expected)
    {
        return;
    }
    write_manifest_summary_for_generation(root, generation_id, manifest, metadata.len());
}

use std::path::Path;

use ctx_history_core::{CORE_RECORD_VERSION, IDENTITY_VERSION};
use ctx_history_index_generation::{
    load_manifest_bytes, load_manifest_metadata, load_manifest_summary_bytes, sha256_hex,
    write_manifest_summary_bytes,
};
use serde::{Deserialize, Serialize};

use super::delta::{decode_validated_delta, MANIFEST_FLAT_DELTA_PREFIX};
use crate::{
    is_generation_id, validate_core_contract_fingerprint, GenerationManifest, IndexError, Result,
    GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
};

pub const MANIFEST_SUMMARY_FORMAT: &str = "ctx-manifest-summary-v1";
const MAX_SUMMARY_PRESENT_NONEMPTY_ROUTES: usize = 64;

/// Exact certified source and record totals derived from a verified generation manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManifestSourceTotals {
    pub source_count: u64,
    pub indexed_documents: u64,
    pub complete_records: u64,
    pub retained_records: u64,
    pub rejected_records: u64,
    pub ignored_records: u64,
    pub certified_source_bytes: u64,
    pub sources_with_rejections: u64,
}

impl ManifestSourceTotals {
    pub fn from_manifest(manifest: &GenerationManifest) -> Result<Self> {
        let mut totals = Self {
            source_count: manifest.sources.len() as u64,
            ..Self::default()
        };
        for source in &manifest.sources {
            let counts = source.counts();
            totals.indexed_documents = totals
                .indexed_documents
                .checked_add(counts.indexed_documents)
                .ok_or(IndexError::CountOverflow)?;
            totals.complete_records = totals
                .complete_records
                .checked_add(counts.complete_records)
                .ok_or(IndexError::CountOverflow)?;
            totals.retained_records = totals
                .retained_records
                .checked_add(counts.retained_records)
                .ok_or(IndexError::CountOverflow)?;
            totals.rejected_records = totals
                .rejected_records
                .checked_add(counts.rejected_records)
                .ok_or(IndexError::CountOverflow)?;
            totals.ignored_records = totals
                .ignored_records
                .checked_add(counts.ignored_records)
                .ok_or(IndexError::CountOverflow)?;
            totals.certified_source_bytes = totals
                .certified_source_bytes
                .checked_add(counts.certified_bytes)
                .ok_or(IndexError::CountOverflow)?;
            totals.sources_with_rejections = totals
                .sources_with_rejections
                .checked_add(u64::from(counts.rejected_records > 0))
                .ok_or(IndexError::CountOverflow)?;
        }
        Ok(totals)
    }
}

pub fn count_archive_sources(manifest: &GenerationManifest) -> u64 {
    manifest
        .sources
        .iter()
        .filter(|source| source.observation().source().source_format() == "ctx_archive_v1")
        .count() as u64
}

fn summary_present_nonempty_route_identities(manifest: &GenerationManifest) -> Option<Vec<String>> {
    let routes = manifest.source_routes();
    if routes.len() > MAX_SUMMARY_PRESENT_NONEMPTY_ROUTES {
        return None;
    }
    Some(
        routes
            .iter()
            .filter(|route| route.missing_state().is_none() && !route.sources().is_empty())
            .map(|route| route.route_identity().as_str().to_owned())
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSummary {
    pub format: String,
    pub generation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_generation_id: Option<String>,
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
    pub complete_records: u64,
    pub retained_records: u64,
    pub rejected_records: u64,
    pub ignored_records: u64,
    pub sources_with_rejections: u64,
    pub archive_source_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present_nonempty_route_identities: Option<Vec<String>>,
    pub manifest_bytes_len: u64,
}

impl ManifestSummary {
    pub fn from_manifest(
        generation_id: &str,
        base_generation_id: Option<String>,
        manifest: &GenerationManifest,
        manifest_bytes_len: u64,
    ) -> Result<Self> {
        let totals = ManifestSourceTotals::from_manifest(manifest)?;
        Ok(Self {
            format: MANIFEST_SUMMARY_FORMAT.to_owned(),
            generation_id: generation_id.to_owned(),
            base_generation_id,
            manifest_version: manifest.manifest_version,
            identity_version: manifest.identity_version,
            core_record_version: manifest.core_record_version,
            core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
            lexical_schema_version: manifest.lexical_schema_version,
            lexical_analyzer_version: manifest.lexical_analyzer_version,
            policy_schema_hash: manifest.policy_schema_hash.clone(),
            indexed_documents: manifest.indexed_documents,
            certified_source_bytes: manifest.certified_source_bytes,
            source_count: totals.source_count,
            complete_records: totals.complete_records,
            retained_records: totals.retained_records,
            rejected_records: totals.rejected_records,
            ignored_records: totals.ignored_records,
            sources_with_rejections: totals.sources_with_rejections,
            archive_source_count: count_archive_sources(manifest),
            present_nonempty_route_identities: summary_present_nonempty_route_identities(manifest),
            manifest_bytes_len,
        })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn source_totals(&self) -> ManifestSourceTotals {
        ManifestSourceTotals {
            source_count: self.source_count,
            indexed_documents: self.indexed_documents,
            complete_records: self.complete_records,
            retained_records: self.retained_records,
            rejected_records: self.rejected_records,
            ignored_records: self.ignored_records,
            certified_source_bytes: self.certified_source_bytes,
            sources_with_rejections: self.sources_with_rejections,
        }
    }
}

/// Validates and loads the cryptographically authenticated summary sidecar for
/// `generation_id`.
///
/// The authoritative manifest file MUST exist and be a regular file; a missing
/// manifest always fails closed rather than trusting an orphaned sidecar.
/// Acceptance requires the trusted generation pointer to supply
/// `expected_summary_sha256` AND the owner-private bounded sidecar bytes to
/// match that exact SHA-256 digest. Any unpinned pointer, digest mismatch,
/// missing/corrupt/non-canonical/size-mismatched/contract-mismatched sidecar
/// returns `Ok(None)` so callers fall back to loading and SHA-256-verifying
/// the authoritative manifest.
pub(super) fn try_load_validated_manifest_summary(
    root: &Path,
    generation_id: &str,
    expected_summary_sha256: Option<&str>,
) -> Result<Option<ManifestSummary>> {
    let manifest_metadata = load_manifest_metadata(root, generation_id)?;
    if !manifest_metadata.file_type().is_file() {
        return Err(IndexError::NonCanonicalManifest);
    }
    let Some(expected_summary_sha256) = expected_summary_sha256 else {
        return Ok(None);
    };
    if !is_generation_id(expected_summary_sha256) {
        return Ok(None);
    }
    let Some(bytes) = load_manifest_summary_bytes(root, generation_id)? else {
        return Ok(None);
    };
    if sha256_hex(&bytes) != expected_summary_sha256 {
        return Ok(None);
    }
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
        || summary
            .base_generation_id
            .as_deref()
            .is_some_and(|base| !is_generation_id(base) || base == generation_id)
        || summary.archive_source_count > summary.source_count
        || summary.sources_with_rejections > summary.source_count
        || !routes_are_canonical(summary.present_nonempty_route_identities.as_deref())
    {
        return Ok(None);
    }
    if !summary_matches_current_read_contract(&summary) {
        return Ok(None);
    }
    Ok(Some(summary))
}

fn routes_are_canonical(routes: Option<&[String]>) -> bool {
    let Some(routes) = routes else {
        return true;
    };
    if routes.len() > MAX_SUMMARY_PRESENT_NONEMPTY_ROUTES {
        return false;
    }
    let mut previous: Option<&str> = None;
    for route in routes {
        if !is_generation_id(route) || previous.is_some_and(|prev| prev >= route.as_str()) {
            return false;
        }
        previous = Some(route.as_str());
    }
    true
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

/// Durably publishes `<generation_id>.summary.json` and returns its SHA-256 digest.
pub fn write_manifest_summary_for_generation(
    root: &Path,
    generation_id: &str,
    base_generation_id: Option<String>,
    manifest: &GenerationManifest,
    manifest_bytes_len: u64,
) -> Result<String> {
    let summary = ManifestSummary::from_manifest(
        generation_id,
        base_generation_id,
        manifest,
        manifest_bytes_len,
    )?;
    let bytes = summary.canonical_bytes()?;
    let digest = sha256_hex(&bytes);
    write_manifest_summary_bytes(root, generation_id, &bytes)?;
    Ok(digest)
}

/// Repairs `<generation_id>.summary.json` for an already-published generation
/// whose active pointer pins `expected_summary_sha256` when the writer holds
/// write authority and the sidecar is absent, corrupt, symlinked, or tampered.
///
/// Legacy unpinned pointers (`expected_summary_sha256 == None`) are left
/// untouched because readers never trust an unpinned sidecar.
pub fn ensure_manifest_summary(
    root: &Path,
    generation_id: &str,
    manifest: &GenerationManifest,
    expected_summary_sha256: Option<&str>,
) {
    let Some(expected_summary_sha256) = expected_summary_sha256 else {
        return;
    };
    if !is_generation_id(expected_summary_sha256) {
        return;
    }
    if try_load_validated_manifest_summary(root, generation_id, Some(expected_summary_sha256))
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let Ok(manifest_bytes) = load_manifest_bytes(root, generation_id) else {
        return;
    };
    let base_generation_id = if manifest_bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        let Ok(delta) = decode_validated_delta(&manifest_bytes) else {
            return;
        };
        Some(delta.base_generation_id)
    } else {
        None
    };
    let Ok(summary) = ManifestSummary::from_manifest(
        generation_id,
        base_generation_id,
        manifest,
        manifest_bytes.len() as u64,
    ) else {
        return;
    };
    let Ok(summary_bytes) = summary.canonical_bytes() else {
        return;
    };
    if sha256_hex(&summary_bytes) != expected_summary_sha256 {
        return;
    }
    let _ = write_manifest_summary_bytes(root, generation_id, &summary_bytes);
}

use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, ProjectionContractError,
    IDENTITY_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{hex, CommittedCoreGeneration, RelationalProjectionError, Result};

pub(super) const GENERATION_MANIFEST_VERSION: u32 = 3;
pub(super) const REQUIRED_LEXICAL_SCHEMA_VERSION: u32 = 5;
pub(super) const REQUIRED_LEXICAL_ANALYZER_VERSION: u32 = 1;
/// Exact hash of the active manifest-v3/schema-v5 source generation policy.
///
/// This independent consumer intentionally pins the published compatibility
/// boundary instead of importing the lexical implementation. Any policy change
/// must update this hash and bump both relational projection versions.
pub(super) const REQUIRED_SOURCE_GENERATION_POLICY_HASH: &str =
    "255eb2b901f1dfb1c9c521c1d177dbe7a416491b5c0b8532494135bcb8b42ede";
const MAX_GENERATION_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationManifest {
    pub(super) manifest_version: u32,
    pub(super) identity_version: u16,
    pub(super) lexical_schema_version: u32,
    pub(super) lexical_analyzer_version: u32,
    pub(super) policy_schema_hash: String,
    pub(super) indexed_documents: u64,
    pub(super) certified_source_bytes: u64,
    pub(super) sources: Vec<CertifiedSource>,
    pub(super) removals: Vec<GenerationRemoval>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationRemoval {
    pub(super) deletion: CertifiedSourceDeletion,
    pub(super) inventory: CertifiedSourceInventory,
}

pub(super) struct ValidatedManifest {
    pub(super) digest: [u8; 32],
    pub(super) sources: BTreeMap<String, ManifestSource>,
    pub(super) removal_ids: BTreeSet<String>,
    pub(super) indexed_documents: u64,
    pub(super) policy_schema_hash: String,
}

pub(super) struct ManifestSource {
    pub(super) certificate: CertifiedSource,
    pub(super) certificate_json: Vec<u8>,
    pub(super) certificate_digest: [u8; 32],
}

impl ValidatedManifest {
    pub(super) fn from_commit(commit: &CommittedCoreGeneration) -> Result<Self> {
        if commit.manifest_json.len() > MAX_GENERATION_MANIFEST_BYTES {
            return invalid_generation("manifest exceeds the relational projection limit");
        }
        let manifest: GenerationManifest =
            serde_json::from_slice(&commit.manifest_json).map_err(|error| {
                RelationalProjectionError::InvalidCoreGeneration(format!(
                    "manifest is not the required canonical v3 contract: {error}; \
                     rebuild the disposable Core generation"
                ))
            })?;
        if serde_json::to_vec(&manifest)? != commit.manifest_json {
            return invalid_generation("manifest is not in canonical ctx JSON encoding");
        }
        if manifest.manifest_version != GENERATION_MANIFEST_VERSION {
            return invalid_generation(
                "Core manifest v3 is required; rebuild the disposable Core generation",
            );
        }
        if manifest.identity_version != IDENTITY_VERSION
            || manifest.lexical_schema_version != REQUIRED_LEXICAL_SCHEMA_VERSION
            || manifest.lexical_analyzer_version != REQUIRED_LEXICAL_ANALYZER_VERSION
        {
            return invalid_generation(
                "manifest identity or schema-v5 lexical lineage is unsupported; \
                 rebuild the disposable Core generation",
            );
        }
        if manifest.policy_schema_hash != REQUIRED_SOURCE_GENERATION_POLICY_HASH {
            return invalid_generation(format!(
                "manifest policy hash {} does not match active policy {}; \
                 rebuild the disposable Core generation",
                manifest.policy_schema_hash, REQUIRED_SOURCE_GENERATION_POLICY_HASH
            ));
        }
        let digest: [u8; 32] = Sha256::digest(&commit.manifest_json).into();
        if commit.generation_id != hex(&digest) {
            return invalid_generation("generation ID does not match the manifest digest");
        }
        if manifest.indexed_documents != commit.indexed_documents
            || manifest.certified_source_bytes != commit.certified_source_bytes
            || manifest.sources.len() != commit.certified_sources
        {
            return invalid_generation("commit receipt counts do not match the manifest");
        }
        let mut expected_events = 0_u64;
        let mut expected_bytes = 0_u64;
        let mut prior_digest = None;
        let mut active_source_digests = BTreeSet::new();
        let mut sources = BTreeMap::new();
        for certificate in manifest.sources {
            certificate
                .validate_contract()
                .map_err(contract_generation_error)?;
            let source = certificate.observation().source();
            let source_digest = source.identity().digest();
            if prior_digest.is_some_and(|prior| prior >= source_digest) {
                return invalid_generation("manifest sources are not strictly sorted");
            }
            prior_digest = Some(source_digest);
            active_source_digests.insert(source_digest);
            expected_events = expected_events
                .checked_add(certificate.counts().indexed_documents)
                .ok_or(RelationalProjectionError::CountOverflow(
                    "manifest indexed documents",
                ))?;
            expected_bytes = expected_bytes
                .checked_add(certificate.counts().certified_bytes)
                .ok_or(RelationalProjectionError::CountOverflow(
                    "manifest certified bytes",
                ))?;
            let certificate_json = serde_json::to_vec(&certificate)?;
            let certificate_digest = Sha256::digest(&certificate_json).into();
            let source_id = source.identity().as_uuid().to_string();
            sources.insert(
                source_id,
                ManifestSource {
                    certificate,
                    certificate_json,
                    certificate_digest,
                },
            );
        }
        let mut prior_removal_digest = None;
        let mut removal_ids = BTreeSet::new();
        for removal in manifest.removals {
            removal
                .deletion
                .validate_contract()
                .map_err(contract_generation_error)?;
            removal
                .inventory
                .validate_contract()
                .map_err(contract_generation_error)?;
            if !removal.deletion.verifies(&removal.inventory) {
                return invalid_generation(
                    "manifest deletion evidence does not match its certified inventory",
                );
            }
            let source = removal.deletion.source();
            let source_digest = source.identity().digest();
            if prior_removal_digest.is_some_and(|prior| prior >= source_digest) {
                return invalid_generation("manifest removals are not strictly sorted");
            }
            if active_source_digests.contains(&source_digest) {
                return invalid_generation(
                    "manifest source and certified removal identities overlap",
                );
            }
            prior_removal_digest = Some(source_digest);
            removal_ids.insert(source.identity().as_uuid().to_string());
        }
        if expected_events != manifest.indexed_documents
            || expected_bytes != manifest.certified_source_bytes
        {
            return invalid_generation("manifest totals do not reconcile");
        }
        Ok(Self {
            digest,
            sources,
            removal_ids,
            indexed_documents: manifest.indexed_documents,
            policy_schema_hash: manifest.policy_schema_hash,
        })
    }
}

pub(super) fn invalid_generation<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidCoreGeneration(
        detail.into(),
    ))
}

fn contract_generation_error(error: ProjectionContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidCoreGeneration(error.to_string())
}

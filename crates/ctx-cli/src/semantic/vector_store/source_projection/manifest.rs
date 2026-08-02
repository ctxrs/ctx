use anyhow::{anyhow, Result};
use ctx_history_core::{core_record_contract_fingerprint, StableEntityKind, IDENTITY_VERSION};
use ctx_history_index::{current_source_generation_policy, CoreEventRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{SourceBackedSemanticGeneration, SourceBackedSemanticPage};
use crate::semantic::{
    model_contract::semantic_model_key,
    vector_store::flat_segments::{
        FlatPublicationToken, FlatSourceStagingToken, PinnedFlatGeneration,
    },
    SemanticEventDocument,
};

pub(super) const SOURCE_FRONTIER_STATE: &str = "core_semantic_frontier_v1";
pub(super) const SOURCE_ACKNOWLEDGEMENT_STATE: &str = "core_semantic_acknowledgement_v1";
pub(super) const SOURCE_CONTRACT_VERSION: u16 = 10;
const SOURCE_CONTRACT_DOMAIN: &[u8] = b"ctx-source-backed-semantic-contract-v1\0";
const SOURCE_BUILD_DOMAIN: &[u8] = b"ctx-source-backed-semantic-build-v1\0";
pub(super) const SOURCE_INPUT_LEXICAL_SCHEMA_VERSION: u32 = 15;
const SHA256_HEX_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceProjectionFrontier {
    pub(super) contract_version: u16,
    pub(super) contract_fingerprint: String,
    pub(super) core_generation_id: String,
    pub(super) semantic_policy_fingerprint: String,
    pub(super) consumer_build_id: String,
    pub(super) semantic_documents: u64,
    pub(super) source_traversal_phase: SourceTraversalPhase,
    pub(super) source_traversal_after_identity_digest: Option<String>,
    pub(super) active_source_identity_digest: Option<String>,
    pub(super) active_source_reconciliation_id: Option<String>,
    pub(super) active_source_indexed_documents: u64,
    pub(super) active_source_semantic_documents: u64,
    pub(super) processed_source_documents: u64,
    pub(super) processed_source_semantic_documents: u64,
    pub(super) after_identity: Option<Vec<u8>>,
    pub(super) source_scan_complete: bool,
    pub(super) removing_source: bool,
    pub(super) last_failure: Option<String>,
    #[serde(default)]
    pub(super) flat_publication: FlatPublicationToken,
    #[serde(default)]
    pub(super) flat_staging: Option<FlatSourceStagingToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SourceTraversalPhase {
    RemovingStaleSources,
    ReconcilingSources,
    Finalizing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceProjectionAcknowledgement {
    pub(super) contract_version: u16,
    pub(super) contract_fingerprint: String,
    pub(super) core_generation_id: String,
    pub(super) semantic_policy_fingerprint: String,
    pub(super) consumer_build_id: String,
    pub(super) semantic_documents: u64,
    pub(super) projected_documents: u64,
    pub(super) source_receipt_count: u64,
    pub(super) source_receipts_hash: String,
    #[serde(default)]
    pub(super) flat_generation: u64,
    #[serde(default)]
    pub(super) flat_generation_hash: String,
    #[serde(default)]
    pub(super) flat_active_events: u64,
    #[serde(default)]
    pub(super) flat_active_chunks: u64,
}

pub(super) struct AcknowledgedSourceProjection {
    pub(super) flat: Option<PinnedFlatGeneration>,
    pub(super) projected_documents: u64,
}

pub(super) fn validate_generation(generation: &SourceBackedSemanticGeneration) -> Result<()> {
    validate_generation_id(&generation.core_generation_id)?;
    validate_sha256(
        &generation.semantic_policy_fingerprint,
        "semantic policy fingerprint",
    )
}

pub(super) fn validate_generation_id(generation_id: &str) -> Result<()> {
    validate_sha256(generation_id, "Core semantic generation ID")
}

fn validate_sha256(value: &str, field: &str) -> Result<()> {
    if value.len() == SHA256_HEX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(());
    }
    Err(anyhow!("{field} is not a lowercase SHA-256 digest"))
}

pub(super) fn validate_page(
    frontier: &SourceProjectionFrontier,
    page: &SourceBackedSemanticPage,
) -> Result<()> {
    if page.core_generation_id != frontier.core_generation_id {
        return Err(anyhow!(
            "source-backed semantic page generation does not match its durable frontier"
        ));
    }
    if frontier.removing_source
        || frontier.source_scan_complete
        || frontier.active_source_identity_digest.as_deref()
            != Some(page.source_identity_digest.as_str())
    {
        return Err(anyhow!(
            "source-backed semantic page does not match its active source frontier"
        ));
    }
    let requested_after = page
        .after
        .map(|identity| identity.encode_canonical().map(|value| value.to_vec()))
        .transpose()?;
    if requested_after != frontier.after_identity {
        return Err(anyhow!(
            "source-backed semantic page cursor does not match its durable frontier"
        ));
    }
    let mut previous = frontier.after_identity.clone();
    for record in &page.records {
        record.event_id.validate_contract()?;
        record.core_record.validate_contract()?;
        if record.event_id.entity_kind() != StableEntityKind::Event
            || record.event_id != record.core_record.event_id
            || record.session_id != record.core_record.session_id
        {
            return Err(anyhow!(
                "Core semantic page contains mismatched record identity"
            ));
        }
        let encoded = record.event_id.encode_canonical()?;
        if previous
            .as_deref()
            .is_some_and(|previous| previous >= encoded.as_slice())
        {
            return Err(anyhow!(
                "source-backed semantic records are not in strict stable-identity order"
            ));
        }
        previous = Some(encoded.to_vec());
    }
    Ok(())
}

pub(super) fn validate_resolved_document(
    record: &CoreEventRecord,
    document: &SemanticEventDocument,
) -> Result<()> {
    if document.event_id != record.event_id.as_uuid()
        || document.seq != record.event_sequence
        || document.text.trim().is_empty()
    {
        return Err(anyhow!(
            "Core semantic document does not match {}",
            record.event_id
        ));
    }
    Ok(())
}

pub(super) fn semantic_policy_fingerprint() -> Result<String> {
    let policy = current_source_generation_policy().semantic;
    let encoded = serde_json::to_vec(&policy)?;
    Ok(hex(&Sha256::digest(encoded)))
}

pub(super) fn source_contract_fingerprint() -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(SOURCE_CONTRACT_DOMAIN);
    digest.update(SOURCE_CONTRACT_VERSION.to_be_bytes());
    digest.update(IDENTITY_VERSION.to_be_bytes());
    digest.update(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION.to_be_bytes());
    digest.update(core_record_contract_fingerprint().as_bytes());
    digest.update(semantic_policy_fingerprint()?.as_bytes());
    digest.update(semantic_model_key().as_bytes());
    Ok(hex(&digest.finalize()))
}

pub(super) fn source_consumer_build_id(
    contract_fingerprint: &str,
    core_generation_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(SOURCE_BUILD_DOMAIN);
    digest.update(contract_fingerprint.as_bytes());
    digest.update(core_generation_id.as_bytes());
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_input_revision_exactly_mirrors_core_schema() {
        assert_eq!(
            SOURCE_INPUT_LEXICAL_SCHEMA_VERSION,
            ctx_history_index::LEXICAL_SCHEMA_VERSION
        );
    }
}

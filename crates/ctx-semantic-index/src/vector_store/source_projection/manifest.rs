use anyhow::{anyhow, Result};
use ctx_history_core::{core_record_contract_fingerprint, StableEntityKind, IDENTITY_VERSION};
use ctx_history_index::{policy::semantic_generation_policy_hash, CoreEventRecord};
#[cfg(test)]
use ctx_semantic_model::semantic_model_contract;
use ctx_semantic_model::SemanticModelContract;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{SourceBackedSemanticGeneration, SourceBackedSemanticPage};
use crate::{
    vector_store::flat_segments::{
        FlatPublicationToken, FlatSourceStagingToken, PinnedFlatGeneration,
    },
    SemanticEventDocument,
};

pub(super) const SOURCE_FRONTIER_STATE: &str = "core_semantic_frontier_v1";
pub(super) const SOURCE_ACKNOWLEDGEMENT_STATE: &str = "core_semantic_acknowledgement_v1";
pub(super) const SOURCE_CONTRACT_VERSION: u16 = 13;
const SOURCE_CONTRACT_DOMAIN: &[u8] = b"ctx-source-backed-semantic-contract-v1\0";
const SOURCE_BUILD_DOMAIN: &[u8] = b"ctx-source-backed-semantic-build-v1\0";
pub(super) const SOURCE_INPUT_LEXICAL_SCHEMA_VERSION: u32 = 22;
const SHA256_HEX_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceProjectionFrontier {
    pub(super) contract_version: u16,
    pub(super) contract_fingerprint: String,
    pub(super) core_generation_id: String,
    pub(super) semantic_policy_fingerprint: String,
    pub(super) consumer_build_id: String,
    pub(super) source_traversal_phase: SourceTraversalPhase,
    pub(super) source_traversal_after_identity_digest: Option<String>,
    pub(super) active_source_identity_digest: Option<String>,
    pub(super) active_source_reconciliation_id: Option<String>,
    pub(super) active_source_indexed_documents: u64,
    pub(super) processed_source_documents: u64,
    pub(super) processed_source_semantic_documents: u64,
    pub(super) processed_source_filtered_documents: u64,
    pub(super) after_identity: Option<Vec<u8>>,
    pub(super) source_scan_complete: bool,
    pub(super) removing_source: bool,
    #[serde(default)]
    pub(super) vector_reuse_allowed: bool,
    pub(super) last_failure: Option<String>,
    #[serde(default)]
    pub(super) flat_publication: FlatPublicationToken,
    #[serde(default)]
    pub(super) flat_staging: Option<FlatSourceStagingToken>,
    /// Monotonic only within this exact Core/model/source target. It is
    /// advanced with a durable reconciliation boundary, never with a status
    /// write or an elapsed-time observation.
    #[serde(default)]
    pub(super) semantic_progress_sequence: u64,
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
    pub(super) filtered_documents: u64,
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
    /// Carries the frontier sequence across final acknowledgement, after the
    /// frontier itself has been removed.
    #[serde(default)]
    pub(super) semantic_progress_sequence: u64,
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
    )?;
    if generation.semantic_policy.canonical_sha256()? != generation.semantic_policy_fingerprint {
        return Err(anyhow!(
            "semantic generation policy does not match its fingerprint"
        ));
    }
    validate_sha256(
        &generation.contract_fingerprint,
        "semantic contract fingerprint",
    )?;
    if source_contract_fingerprint_with_authority(
        &generation.semantic_policy_fingerprint,
        &generation.model_descriptor,
    )? != generation.contract_fingerprint
    {
        return Err(anyhow!(
            "semantic generation model descriptor does not match its contract fingerprint"
        ));
    }
    Ok(())
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
    let literal_facts = record
        .core_record
        .content
        .activity
        .as_ref()
        .map_or(&[][..], |activity| activity.facts.as_slice());
    if document.event_id != record.event_id.as_uuid()
        || document.seq != record.event_sequence
        || document.agent_scope != record.core_record.agent_scope
        || document.literal_facts.as_slice() != literal_facts
        || document.text.trim().is_empty()
    {
        return Err(anyhow!(
            "Core semantic document does not match {}",
            record.event_id
        ));
    }
    Ok(())
}

pub(super) fn semantic_policy_fingerprint(
    model_contract: &SemanticModelContract,
) -> Result<String> {
    Ok(semantic_generation_policy_hash(model_contract)?)
}

pub(super) fn source_contract_fingerprint(
    model_contract: &SemanticModelContract,
) -> Result<String> {
    source_contract_fingerprint_with_authority(
        &semantic_policy_fingerprint(model_contract)?,
        model_contract.descriptor(),
    )
}

pub(super) fn trusted_legacy_source_contract_fingerprint(
    model_contract: &SemanticModelContract,
    semantic_policy_fingerprint: &str,
) -> Result<Option<String>> {
    model_contract
        .legacy_builtin_descriptor_alias()
        .map(|descriptor| {
            source_contract_fingerprint_with_authority(semantic_policy_fingerprint, descriptor)
        })
        .transpose()
}

pub(super) fn source_contract_fingerprint_with_authority(
    semantic_policy_fingerprint: &str,
    model_descriptor: &str,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(SOURCE_CONTRACT_DOMAIN);
    digest.update(SOURCE_CONTRACT_VERSION.to_be_bytes());
    digest.update(IDENTITY_VERSION.to_be_bytes());
    digest.update(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION.to_be_bytes());
    digest.update(core_record_contract_fingerprint().as_bytes());
    digest.update(semantic_policy_fingerprint.as_bytes());
    digest.update(model_descriptor.as_bytes());
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

    fn revise_descriptor_component(descriptor: &str, revised_index: usize) -> String {
        descriptor
            .split('|')
            .enumerate()
            .map(|(index, component)| {
                if index != revised_index {
                    return component.to_owned();
                }
                component.split_once('=').map_or_else(
                    || format!("{component}-test-only-revision"),
                    |(field, _)| format!("{field}=test-only-revision"),
                )
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn source_input_revision_exactly_mirrors_core_schema() {
        assert_eq!(
            SOURCE_INPUT_LEXICAL_SCHEMA_VERSION,
            ctx_history_index::LEXICAL_SCHEMA_VERSION
        );
    }

    #[test]
    fn complete_model_descriptor_participates_in_semantic_contract_identity() {
        let model_contract = semantic_model_contract();
        let descriptor = model_contract.descriptor();
        let policy = semantic_policy_fingerprint(model_contract).unwrap();
        let baseline = source_contract_fingerprint_with_authority(&policy, descriptor).unwrap();
        for (index, component) in descriptor.split('|').enumerate() {
            let field = component
                .split_once('=')
                .map_or("contract_version", |(field, _)| field);
            let revised = revise_descriptor_component(descriptor, index);
            assert_ne!(descriptor, revised, "fixture did not revise {field}");
            assert_ne!(
                baseline,
                source_contract_fingerprint_with_authority(&policy, &revised).unwrap(),
                "{field} did not rotate source projection identity"
            );
        }
        assert_eq!(
            baseline,
            source_contract_fingerprint(model_contract).unwrap()
        );
    }
}

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use ctx_history_core::{HydrationFailureKind, StableEntityKind, IDENTITY_VERSION};
use ctx_history_index::EventRecord;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{SourceBackedSemanticGeneration, SourceBackedSemanticPage};
use crate::semantic::{
    model_contract::semantic_model_key, vector_store::flat_segments::PinnedFlatGeneration,
    vector_store_schema::SemanticVectorStoreError, SemanticEventDocument,
};

pub(super) const SOURCE_FRONTIER_STATE: &str = "source_backed_semantic_frontier_v1";
pub(super) const SOURCE_ACKNOWLEDGEMENT_STATE: &str = "source_backed_semantic_acknowledgement_v1";
pub(super) const SOURCE_CONTRACT_VERSION: u16 = 3;
const SOURCE_CONTRACT_DOMAIN: &[u8] = b"ctx-source-backed-semantic-contract-v1\0";
const SOURCE_BUILD_DOMAIN: &[u8] = b"ctx-source-backed-semantic-build-v1\0";
pub(super) const SOURCE_INPUT_LEXICAL_SCHEMA_VERSION: u32 = 5;
const SHA256_HEX_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceProjectionFrontier {
    pub(super) contract_version: u16,
    pub(super) contract_fingerprint: String,
    pub(super) core_generation_id: String,
    pub(super) consumer_build_id: String,
    pub(super) semantic_documents: u64,
    pub(super) processed_documents: u64,
    pub(super) after_identity: Option<Vec<u8>>,
    pub(super) last_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceProjectionAcknowledgement {
    pub(super) contract_version: u16,
    pub(super) contract_fingerprint: String,
    pub(super) core_generation_id: String,
    pub(super) consumer_build_id: String,
    pub(super) semantic_documents: u64,
    pub(super) projected_documents: u64,
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
}

pub(super) fn validate_flat_projection(
    frontier: &SourceProjectionFrontier,
    source_documents: &HashMap<Uuid, String>,
    pinned: Option<&PinnedFlatGeneration>,
) -> Result<u64> {
    let source_document_count = u64::try_from(source_documents.len())?;
    if source_document_count > frontier.semantic_documents {
        return Err(SemanticVectorStoreError::reset_required(format!(
            "source-backed semantic completion has {source_document_count} projected documents, but only {} metadata-eligible records",
            frontier.semantic_documents
        ))
        .into());
    }
    if source_document_count == 0 {
        if pinned.is_some_and(|pinned| {
            pinned.stats().active_events != 0 || pinned.stats().active_chunks != 0
        }) {
            return Err(SemanticVectorStoreError::reset_required(
                "empty source-backed semantic generation has active flat F32 records",
            )
            .into());
        }
        return Ok(0);
    }
    let pinned = pinned.ok_or_else(|| {
        SemanticVectorStoreError::reset_required(
            "source-backed semantic completion has no flat F32 generation",
        )
    })?;
    if pinned.stats().active_events as u64 != source_document_count
        || pinned.active_events().len() != source_documents.len()
    {
        return Err(SemanticVectorStoreError::reset_required(
            "source-backed semantic source-document count does not match flat F32 events",
        )
        .into());
    }
    for event in pinned.active_events() {
        if event.chunk_count == 0
            || source_documents
                .get(&event.event_id)
                .is_none_or(|hash| hash != &event.source_text_hash.to_hex())
        {
            return Err(SemanticVectorStoreError::reset_required(
                "source-backed semantic source documents do not match flat F32 event metadata",
            )
            .into());
        }
    }
    Ok(source_document_count)
}

pub(super) fn validate_generation(generation: &SourceBackedSemanticGeneration) -> Result<()> {
    validate_generation_id(&generation.core_generation_id)
}

pub(super) fn validate_generation_id(generation_id: &str) -> Result<()> {
    if generation_id.len() != SHA256_HEX_BYTES
        || !generation_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(anyhow!(
            "source-backed semantic generation ID is not a lowercase SHA-256 digest"
        ));
    }
    Ok(())
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
    for event in &page.records {
        event.event_id.validate_contract()?;
        event.locator.validate_contract()?;
        if event.event_id.entity_kind() != StableEntityKind::Event
            || event.event_id.source_digest() != event.locator.source().identity().digest()
            || event.event_id.source_descriptor_digest()
                != event.locator.source().exact_descriptor_digest()
        {
            return Err(anyhow!(
                "source-backed semantic page contains mismatched identity and locator evidence"
            ));
        }
        let encoded = event.event_id.encode_canonical()?;
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
    event: &EventRecord,
    document: &SemanticEventDocument,
) -> Result<()> {
    if document.event_id != event.event_id.as_uuid()
        || document.seq != event.event_sequence
        || document.text.trim().is_empty()
    {
        return Err(anyhow!(
            "source-backed semantic resolver returned a document that does not match {}",
            event.event_id
        ));
    }
    Ok(())
}

pub(super) fn hydration_failure_invalidates(kind: HydrationFailureKind) -> bool {
    matches!(
        kind,
        HydrationFailureKind::ConfirmedDeleted
            | HydrationFailureKind::StaleSourceEvidence
            | HydrationFailureKind::StaleRecordEvidence
            | HydrationFailureKind::MissingRecord
            | HydrationFailureKind::InvalidLocator
    )
}

pub(super) fn hydration_failure_name(kind: HydrationFailureKind) -> &'static str {
    match kind {
        HydrationFailureKind::TemporarilyUnavailable => "temporarily_unavailable",
        HydrationFailureKind::ConfirmedDeleted => "confirmed_deleted",
        HydrationFailureKind::StaleSourceEvidence => "stale_source_evidence",
        HydrationFailureKind::StaleRecordEvidence => "stale_record_evidence",
        HydrationFailureKind::MissingRecord => "missing_record",
        HydrationFailureKind::UnsupportedParserRevision => "unsupported_parser_revision",
        HydrationFailureKind::InvalidLocator => "invalid_locator",
    }
}

pub(super) fn source_contract_fingerprint() -> String {
    let mut digest = Sha256::new();
    digest.update(SOURCE_CONTRACT_DOMAIN);
    digest.update(SOURCE_CONTRACT_VERSION.to_be_bytes());
    digest.update(IDENTITY_VERSION.to_be_bytes());
    digest.update(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION.to_be_bytes());
    digest.update(semantic_model_key().as_bytes());
    hex(&digest.finalize())
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

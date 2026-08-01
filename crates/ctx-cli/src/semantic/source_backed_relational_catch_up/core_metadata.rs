use ctx_history_core::CertifiedSource;
use ctx_history_index::VerifiedIndex;
use ctx_history_relational::{
    CommittedCoreGeneration, RelationalSourceHealth, RelationalSourceMetadata,
};
use sha2::{Digest, Sha256};

use super::SourceBackedRelationalCatchUpError;

pub(super) fn committed_generation(
    index: &VerifiedIndex,
) -> std::result::Result<CommittedCoreGeneration, SourceBackedRelationalCatchUpError> {
    let manifest = index.manifest();
    let sources = manifest
        .sources
        .iter()
        .zip(&manifest.core_record_aggregates)
        .map(|(certificate, aggregate)| {
            // Certification alone does not change when a Core projector rewrites
            // an equal-count source. Bind the exact stored-record aggregate too.
            relational_source_metadata_for_revision(certificate, &(certificate, aggregate))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(CommittedCoreGeneration {
        generation_id: index.generation_id().to_owned(),
        manifest_version: manifest.manifest_version,
        core_record_version: manifest.core_record_version,
        core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
        lexical_schema_version: manifest.lexical_schema_version,
        policy_schema_hash: manifest.policy_schema_hash.clone(),
        indexed_documents: manifest.indexed_documents,
        sources,
    })
}

pub(super) fn relational_source_metadata(
    certificate: &CertifiedSource,
) -> std::result::Result<RelationalSourceMetadata, SourceBackedRelationalCatchUpError> {
    relational_source_metadata_for_revision(certificate, certificate)
}

fn relational_source_metadata_for_revision(
    certificate: &CertifiedSource,
    revision: &impl serde::Serialize,
) -> std::result::Result<RelationalSourceMetadata, SourceBackedRelationalCatchUpError> {
    let encoded = serde_json::to_vec(revision).map_err(|error| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
            "serialize Core source revision: {error}"
        ))
    })?;
    Ok(RelationalSourceMetadata {
        source: certificate.observation().source().clone(),
        parser_revision: certificate.parser_revision().to_owned(),
        revision_digest: Sha256::digest(encoded).into(),
        indexed_event_count: certificate.counts().indexed_documents,
        health: RelationalSourceHealth::Ready,
    })
}

use serde::{Deserialize, Serialize};

use ctx_attribution_model::EvidenceCitation;
use ctx_history_core::SourceKey;

use super::resource::{decode_stable_entity_required, encode_stable_entity};
use super::{
    EvidenceRelationship, MAX_CORE_IDENTITY_BYTES, MAX_IDENTIFIER_BYTES, ServingModelError,
    is_lower_sha256, validate_text,
};

const CORE_CITATION_SOURCE_PREFIX: &str = "_ctx.core_citation_source.v1:";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServingCitation {
    pub citation_id: String,
    pub source_id: String,
    pub source_revision_sha256: String,
    pub session_id: String,
    pub event_id: String,
    pub event_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_end_exclusive: Option<u64>,
    pub evidence_sha256: String,
    pub relationship: EvidenceRelationship,
}

impl ServingCitation {
    pub fn from_core(
        evidence_id: impl Into<String>,
        citation: &EvidenceCitation,
        source_revision_sha256: impl Into<String>,
        relationship: EvidenceRelationship,
    ) -> Result<Self, ServingModelError> {
        let evidence_sha256 = citation
            .evidence_sha256
            .clone()
            .ok_or(ServingModelError::InvalidCoreCitation)?;
        let source_id = encode_core_citation_source(&CoreCitationSource {
            core_generation_id: citation.core_generation_id.clone(),
            source: citation.source.clone(),
        })?;
        let serving = Self {
            citation_id: evidence_id.into(),
            source_id,
            source_revision_sha256: source_revision_sha256.into(),
            session_id: encode_stable_entity(&citation.session_id)?,
            event_id: encode_stable_entity(&citation.event_id)?,
            event_sequence: citation.event_sequence,
            byte_start: citation.byte_range.as_ref().map(|range| range.start),
            byte_end_exclusive: citation
                .byte_range
                .as_ref()
                .map(|range| range.end_exclusive),
            evidence_sha256,
            relationship,
        };
        serving.validate_exact_core()?;
        Ok(serving)
    }

    pub fn exact_core_citation(&self) -> Result<EvidenceCitation, ServingModelError> {
        let source = decode_core_citation_source(&self.source_id)?;
        let session_id = decode_stable_entity_required(&self.session_id)?;
        let event_id = decode_stable_entity_required(&self.event_id)?;
        let byte_range = match (self.byte_start, self.byte_end_exclusive) {
            (None, None) => None,
            (Some(start), Some(end_exclusive)) => Some(ctx_attribution_model::ByteRange {
                start,
                end_exclusive,
            }),
            _ => return Err(ServingModelError::InvalidCoreCitation),
        };
        let citation = EvidenceCitation {
            core_generation_id: source.core_generation_id,
            source: source.source,
            session_id,
            event_id,
            event_sequence: self.event_sequence,
            byte_range,
            evidence_sha256: Some(self.evidence_sha256.clone()),
        };
        citation
            .is_usable()
            .then_some(citation)
            .ok_or(ServingModelError::InvalidCoreCitation)
    }

    pub fn validate(&self) -> Result<(), ServingModelError> {
        validate_text("citation id", &self.citation_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(
            "citation source id",
            &self.source_id,
            MAX_CORE_IDENTITY_BYTES,
        )?;
        validate_text(
            "citation session id",
            &self.session_id,
            MAX_CORE_IDENTITY_BYTES,
        )?;
        validate_text("citation event id", &self.event_id, MAX_CORE_IDENTITY_BYTES)?;
        if !is_lower_sha256(&self.source_revision_sha256) || !is_lower_sha256(&self.evidence_sha256)
        {
            return Err(ServingModelError::InvalidDigest);
        }
        match (self.byte_start, self.byte_end_exclusive) {
            (None, None) => Ok(()),
            (Some(start), Some(end)) if start <= end => Ok(()),
            _ => Err(ServingModelError::InvalidLineRange),
        }
    }

    pub fn validate_exact_core(&self) -> Result<(), ServingModelError> {
        self.validate()?;
        let _ = self.exact_core_citation()?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreCitationSource {
    core_generation_id: String,
    source: SourceKey,
}

fn encode_core_citation_source(value: &CoreCitationSource) -> Result<String, ServingModelError> {
    let encoded =
        serde_json::to_string(value).map_err(|_| ServingModelError::InvalidCoreCitation)?;
    let encoded = format!("{CORE_CITATION_SOURCE_PREFIX}{encoded}");
    validate_text("Core citation source", &encoded, MAX_CORE_IDENTITY_BYTES)?;
    Ok(encoded)
}

fn decode_core_citation_source(value: &str) -> Result<CoreCitationSource, ServingModelError> {
    let encoded = value
        .strip_prefix(CORE_CITATION_SOURCE_PREFIX)
        .ok_or(ServingModelError::InvalidCoreCitation)?;
    serde_json::from_str::<CoreCitationSource>(encoded)
        .map_err(|_| ServingModelError::InvalidCoreCitation)
}

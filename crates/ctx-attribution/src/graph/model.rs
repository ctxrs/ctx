use serde::Serialize;

use super::identity::GraphRecordId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ResourceWrite {
    pub resource_id: String,
    pub namespace_id: String,
    pub resource_type: String,
    pub canonical_key: String,
    pub attributes_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ResourceAliasWrite {
    pub namespace_id: String,
    pub alias_type: String,
    pub alias_value: String,
    pub resource_id: String,
}

/// A semantic relationship fact. No duplicate edge row is produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FactWrite {
    pub fact_type: String,
    pub subject_resource_id: String,
    pub object_resource_id: Option<String>,
    pub scope_resource_id: Option<String>,
    pub actor_resource_id: Option<String>,
    pub occurred_at_unix_ms: Option<i64>,
    pub status: Option<String>,
    pub payload_json: Option<String>,
}

impl FactWrite {
    /// Derives the only durable fact identity from semantic and resource fields.
    pub fn canonical_id(&self) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct Identity<'a> {
            identity_version: u32,
            fact_type: &'a str,
            subject_resource_id: &'a str,
            object_resource_id: &'a Option<String>,
            scope_resource_id: &'a Option<String>,
            actor_resource_id: &'a Option<String>,
            occurred_at_unix_ms: Option<i64>,
            status: &'a Option<String>,
            payload: &'a Option<serde_json::Value>,
        }

        let payload = self.canonical_payload()?;
        let identity = serde_json::to_vec(&Identity {
            identity_version: 1,
            fact_type: &self.fact_type,
            subject_resource_id: &self.subject_resource_id,
            object_resource_id: &self.object_resource_id,
            scope_resource_id: &self.scope_resource_id,
            actor_resource_id: &self.actor_resource_id,
            occurred_at_unix_ms: self.occurred_at_unix_ms,
            status: &self.status,
            payload: &payload,
        })?;
        Ok(GraphRecordId::from_parts("fact", [identity.as_slice()]).to_string())
    }

    pub(crate) fn canonical_payload_json(&self) -> Result<Option<String>, serde_json::Error> {
        self.canonical_payload()?
            .map(|payload| serde_json::to_string(&payload))
            .transpose()
    }

    fn canonical_payload(&self) -> Result<Option<serde_json::Value>, serde_json::Error> {
        self.payload_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
    }
}

/// One stable Core event citation. Generation is citation provenance, not
/// event ownership, so it deliberately does not participate in evidence ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct EvidenceWrite {
    pub source_id: String,
    pub source_revision_sha256: String,
    pub origin_event_id: String,
    pub citation: crate::protocol::EvidenceCitation,
}

impl EvidenceWrite {
    pub(crate) fn canonical_id(&self) -> String {
        GraphRecordId::from_parts(
            "core_evidence",
            [self.source_id.as_bytes(), self.origin_event_id.as_bytes()],
        )
        .to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct FactEvidenceWrite {
    fact_id: String,
    evidence_id: String,
    relationship: String,
    confidence: String,
    detector_id: String,
    detector_revision: String,
}

impl FactEvidenceWrite {
    pub(crate) fn new(
        fact: &FactWrite,
        evidence: &EvidenceWrite,
        relationship: impl Into<String>,
        confidence: impl Into<String>,
        detector_id: impl Into<String>,
        detector_revision: impl Into<String>,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            fact_id: fact.canonical_id()?,
            evidence_id: evidence.canonical_id(),
            relationship: relationship.into(),
            confidence: confidence.into(),
            detector_id: detector_id.into(),
            detector_revision: detector_revision.into(),
        })
    }

    pub(crate) fn fact_id(&self) -> &str {
        &self.fact_id
    }

    pub(crate) fn evidence_id(&self) -> &str {
        &self.evidence_id
    }

    pub(crate) fn relationship(&self) -> &str {
        &self.relationship
    }

    pub(crate) fn confidence(&self) -> &str {
        &self.confidence
    }

    pub(crate) fn detector_id(&self) -> &str {
        &self.detector_id
    }

    pub(crate) fn detector_revision(&self) -> &str {
        &self.detector_revision
    }
}

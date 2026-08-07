use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::validation::{
    encode_with_bound, hex_sha256, validate_encoded_bound, validate_identity, validate_sha256,
};
use super::{CoreGenerationHead, MAX_CORE_CONTROL_WIRE_BYTES, MAX_CORE_SOURCE_STATES};
use crate::{ErrorClass, ProtocolError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationReceipt {
    pub core_generation_id: String,
    pub core_record_contract_fingerprint: String,
    pub source_snapshot_sha256: String,
    pub materializer_revision: String,
    pub source_count: u32,
    pub event_count: u64,
}

impl CoreMaterializationReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(
            &self.core_record_contract_fingerprint,
            "Core record contract fingerprint",
        )?;
        validate_sha256(&self.source_snapshot_sha256, "Core source snapshot")?;
        validate_identity(&self.materializer_revision, "Core materializer revision")?;
        if usize::try_from(self.source_count)
            .ok()
            .is_none_or(|count| count > MAX_CORE_SOURCE_STATES)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core materialization receipt exceeds its source count bound",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core materialization receipt exceeds its wire bound",
        )
    }

    pub fn validate_for_head(&self, head: &CoreGenerationHead) -> Result<(), ProtocolError> {
        self.validate()?;
        if self.core_generation_id != head.core_generation_id
            || self.core_record_contract_fingerprint != head.core_record_contract_fingerprint
            || self.source_snapshot_sha256 != head.source_snapshot_sha256
            || self.source_count != head.source_count
            || self.event_count != head.event_count
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core materialization receipt belongs to a different generation contract",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationReceiptIdentity {
    pub core_generation_id: String,
    pub materializer_revision: String,
}

impl CoreMaterializationReceiptIdentity {
    pub fn from_receipt(receipt: &CoreMaterializationReceipt) -> Result<Self, ProtocolError> {
        receipt.validate()?;
        Ok(Self {
            core_generation_id: receipt.core_generation_id.clone(),
            materializer_revision: receipt.materializer_revision.clone(),
        })
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "Core materializer revision")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginCoreMaterializationRequest {
    pub head: CoreGenerationHead,
    pub expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
}

impl BeginCoreMaterializationRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.validate_fields()?;
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "begin Core materialization request exceeds its wire bound",
        )
    }

    fn validate_fields(&self) -> Result<(), ProtocolError> {
        self.head.validate()?;
        if let Some(receipt) = &self.expected_prior_receipt {
            receipt.validate()?;
        }
        Ok(())
    }

    pub fn acknowledgement_identity(
        &self,
    ) -> Result<CoreMaterializationBeginAcknowledgementIdentity, ProtocolError> {
        self.validate_fields()?;
        let encoded_request = encode_with_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "begin Core materialization request exceeds its wire bound",
        )?;
        let mut materialization_id_prefix = Sha256::new();
        materialization_id_prefix.update(b"[");
        materialization_id_prefix.update(encoded_request);
        materialization_id_prefix.update(b",");
        Ok(CoreMaterializationBeginAcknowledgementIdentity {
            core_generation_id: self.head.core_generation_id.clone(),
            expected_prior_receipt: self.expected_prior_receipt.clone(),
            materialization_id_prefix,
        })
    }
}

/// Pre-transport CAS state for validating a Core materialization begin receipt
/// without retaining or re-encoding the complete request.
#[derive(Clone)]
pub struct CoreMaterializationBeginAcknowledgementIdentity {
    core_generation_id: String,
    expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
    materialization_id_prefix: Sha256,
}

impl CoreMaterializationBeginAcknowledgementIdentity {
    pub(super) fn materialization_id(
        &self,
        materializer_revision: &str,
    ) -> Result<String, ProtocolError> {
        validate_identity(materializer_revision, "Core materializer revision")?;
        let encoded_revision = serde_json::to_vec(materializer_revision).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core materialization ID encoding failed",
            )
        })?;
        let mut digest = self.materialization_id_prefix.clone();
        digest.update(encoded_revision);
        digest.update(b"]");
        Ok(hex_sha256(digest.finalize()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationBegan {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub materializer_revision: String,
    pub expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
    pub replayed: bool,
}

impl CoreMaterializationBegan {
    pub fn validate_for(
        &self,
        request: &BeginCoreMaterializationRequest,
    ) -> Result<(), ProtocolError> {
        self.validate_for_identity(&request.acknowledgement_identity()?)
    }

    pub fn validate_for_identity(
        &self,
        identity: &CoreMaterializationBeginAcknowledgementIdentity,
    ) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "Core materializer revision")?;
        if let Some(receipt) = &self.expected_prior_receipt {
            receipt.validate()?;
        }
        if self.core_generation_id != identity.core_generation_id
            || self.expected_prior_receipt != identity.expected_prior_receipt
            || self.materialization_id
                != identity.materialization_id(&self.materializer_revision)?
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core materialization begin response does not match its request CAS",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishCoreMaterializationRequest {
    pub materialization_id: String,
    pub head: CoreGenerationHead,
    pub expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
    pub source_delta_pages: u32,
    pub changed_sources: u32,
    pub removed_sources: u32,
    pub event_delta_pages: u32,
    pub event_mutations: u64,
}

impl FinishCoreMaterializationRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.validate_fields()?;
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "finish Core materialization request exceeds its wire bound",
        )
    }

    fn validate_fields(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        self.head.validate()?;
        if let Some(receipt) = &self.expected_prior_receipt {
            receipt.validate()?;
        }
        if self.changed_sources > self.head.source_count
            || usize::try_from(self.removed_sources)
                .ok()
                .is_none_or(|count| count > MAX_CORE_SOURCE_STATES)
            || (self.changed_sources == 0 && self.removed_sources == 0)
                != (self.event_delta_pages == 0)
            || self.event_delta_pages < self.changed_sources.saturating_add(self.removed_sources)
            || (self.changed_sources > 0 || self.removed_sources > 0)
                && self.source_delta_pages == 0
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core materialization terminal counts are inconsistent",
            ));
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, ProtocolError> {
        self.validate_fields()?;
        let encoded = encode_with_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "finish Core materialization request exceeds its wire bound",
        )?;
        Ok(hex_sha256(Sha256::digest(encoded)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreMaterializationFinalizationPhase {
    SealingInputs,
    EmitReplay,
    EmitFlat,
    EmitEventIndex,
    EmitSources,
    ValidateCandidate,
    ReadyToActivate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationFinalizationProgress {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub finish_request_digest: String,
    pub materializer_revision: String,
    pub phase: CoreMaterializationFinalizationPhase,
    pub cursor_sha256: String,
}

impl CoreMaterializationFinalizationProgress {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.finish_request_digest, "Core Finish request digest")?;
        validate_identity(&self.materializer_revision, "Core materializer revision")?;
        validate_sha256(&self.cursor_sha256, "Core finalization cursor")?;
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core materialization finalization progress exceeds its wire bound",
        )
    }

    pub fn validate_for_finish(
        &self,
        request: &FinishCoreMaterializationRequest,
        materializer_revision: &str,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        request.validate()?;
        if self.materialization_id != request.materialization_id
            || self.core_generation_id != request.head.core_generation_id
            || self.finish_request_digest != request.canonical_digest()?
            || self.materializer_revision != materializer_revision
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core finalization progress belongs to a different Finish request or materializer revision",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinueCoreMaterializationRequest {
    pub expected_progress: CoreMaterializationFinalizationProgress,
}

impl ContinueCoreMaterializationRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.expected_progress.validate()?;
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "continue Core materialization request exceeds its wire bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationFinalizationPending {
    pub progress: CoreMaterializationFinalizationProgress,
    pub replayed: bool,
}

impl CoreMaterializationFinalizationPending {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.progress.validate()?;
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core materialization finalization pending response exceeds its wire bound",
        )
    }

    pub fn validate_for_finish(
        &self,
        request: &FinishCoreMaterializationRequest,
        materializer_revision: &str,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        self.progress
            .validate_for_finish(request, materializer_revision)
    }

    pub fn validate_for_continue(
        &self,
        request: &ContinueCoreMaterializationRequest,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        request.validate()?;
        if self.progress.materialization_id != request.expected_progress.materialization_id
            || self.progress.core_generation_id != request.expected_progress.core_generation_id
            || self.progress.finish_request_digest
                != request.expected_progress.finish_request_digest
            || self.progress.materializer_revision
                != request.expected_progress.materializer_revision
            || self.progress.phase < request.expected_progress.phase
            || self.progress == request.expected_progress
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core finalization response did not advance the expected owner and cursor",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationFinished {
    pub materialization_id: String,
    pub finish_request_digest: String,
    pub receipt: CoreMaterializationReceipt,
    pub replayed: bool,
}

impl CoreMaterializationFinished {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.finish_request_digest, "Core Finish request digest")?;
        self.receipt.validate()?;
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core materialization finished response exceeds its wire bound",
        )
    }

    pub fn validate_for_finish(
        &self,
        request: &FinishCoreMaterializationRequest,
        materializer_revision: &str,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        request.validate()?;
        self.receipt.validate_for_head(&request.head)?;
        if self.materialization_id != request.materialization_id
            || self.finish_request_digest != request.canonical_digest()?
            || self.receipt.materializer_revision != materializer_revision
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core materialization finished response does not match its Finish request CAS",
            ));
        }
        Ok(())
    }

    pub fn validate_for_continue(
        &self,
        request: &ContinueCoreMaterializationRequest,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        request.validate()?;
        let expected = &request.expected_progress;
        if self.materialization_id != expected.materialization_id
            || self.finish_request_digest != expected.finish_request_digest
            || self.receipt.core_generation_id != expected.core_generation_id
            || self.receipt.materializer_revision != expected.materializer_revision
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core materialization finished response does not match its continuation CAS",
            ));
        }
        Ok(())
    }
}

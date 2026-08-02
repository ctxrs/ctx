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
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "finish Core materialization request exceeds its wire bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMaterializationFinished {
    pub receipt: CoreMaterializationReceipt,
    pub replayed: bool,
}

impl CoreMaterializationFinished {
    pub fn validate_for(
        &self,
        request: &FinishCoreMaterializationRequest,
    ) -> Result<(), ProtocolError> {
        request.validate()?;
        self.receipt.validate_for_head(&request.head)?;
        Ok(())
    }
}

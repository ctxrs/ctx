use super::validation::{validate_encoded_bound, validate_identity, validate_sha256};
use super::{CoreGenerationHead, MAX_CORE_CONTROL_WIRE_BYTES, MAX_CORE_SOURCE_STATES};
use crate::{ErrorClass, ProtocolError};
use serde::{Deserialize, Serialize};

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

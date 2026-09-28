use crate::{CORE_REPOSITORY_CONTRACT_REVISION, ErrorClass, ProtocolError};
use ctx_history_core::{
    CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION, SourceKey,
};
use serde::{Deserialize, Serialize};
mod receipt;
mod source_snapshot;
mod validation;
pub use receipt::{CoreMaterializationReceipt, CoreMaterializationReceiptIdentity};
pub use source_snapshot::{CoreSourceSnapshot, CoreSourceSnapshotBuilder};
pub use validation::{
    CoreRecordDigests, core_record_digests, core_record_digests_from_encoded,
    core_record_leaf_sha256, core_record_sha256, core_source_snapshot_sha256,
};
use validation::{invalid_contract, validate_encoded_bound, validate_sha256};
pub const CORE_MATERIALIZATION_CONTRACT_VERSION: u16 = 3;
pub const MAX_CORE_CONTROL_WIRE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CORE_MATERIALIZER_REVISION_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceState {
    pub source: SourceKey,
    pub core_record_accumulator: String,
    pub event_count: u64,
}

impl CoreSourceState {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("Core source identity", error))?;
        validate_sha256(
            &self.core_record_accumulator,
            "Core source record accumulator",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreGenerationHead {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub generation_manifest_version: u32,
    pub identity_version: u16,
    pub core_record_version: u32,
    pub core_record_contract_fingerprint: String,
    pub normalization_revision: u32,
    pub content_policy_revision: u32,
    pub repository_contract_revision: u32,
    pub lexical_schema_version: u32,
    pub lexical_analyzer_version: u32,
    pub policy_schema_hash: String,
    pub source_snapshot_sha256: String,
    pub source_count: u32,
    pub event_count: u64,
}

impl CoreGenerationHead {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core_generation_id: impl Into<String>,
        generation_manifest_version: u32,
        identity_version: u16,
        core_record_contract_fingerprint: impl Into<String>,
        lexical_schema_version: u32,
        lexical_analyzer_version: u32,
        policy_schema_hash: impl Into<String>,
        sources: &[CoreSourceState],
    ) -> Result<Self, ProtocolError> {
        Self::from_source_snapshot(
            core_generation_id,
            generation_manifest_version,
            identity_version,
            core_record_contract_fingerprint,
            lexical_schema_version,
            lexical_analyzer_version,
            policy_schema_hash,
            CoreSourceSnapshot::from_sources(sources.iter())?,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_source_snapshot(
        core_generation_id: impl Into<String>,
        generation_manifest_version: u32,
        identity_version: u16,
        core_record_contract_fingerprint: impl Into<String>,
        lexical_schema_version: u32,
        lexical_analyzer_version: u32,
        policy_schema_hash: impl Into<String>,
        sources: CoreSourceSnapshot,
    ) -> Result<Self, ProtocolError> {
        let head = Self {
            contract_version: CORE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: core_generation_id.into(),
            generation_manifest_version,
            identity_version,
            core_record_version: CORE_RECORD_VERSION,
            core_record_contract_fingerprint: core_record_contract_fingerprint.into(),
            normalization_revision: CORE_NORMALIZATION_REVISION,
            content_policy_revision: CORE_CONTENT_POLICY_REVISION,
            repository_contract_revision: CORE_REPOSITORY_CONTRACT_REVISION,
            lexical_schema_version,
            lexical_analyzer_version,
            policy_schema_hash: policy_schema_hash.into(),
            source_snapshot_sha256: sources.sha256,
            source_count: sources.source_count,
            event_count: sources.event_count,
        };
        head.validate()?;
        Ok(head)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != CORE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "Core generation head uses an unsupported materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(
            &self.core_record_contract_fingerprint,
            "Core record contract fingerprint",
        )?;
        validate_sha256(&self.policy_schema_hash, "Core policy schema")?;
        validate_sha256(&self.source_snapshot_sha256, "Core source snapshot")?;
        if self.core_record_version == 0
            || self.normalization_revision == 0
            || self.content_policy_revision == 0
            || self.repository_contract_revision == 0
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core generation head revisions are invalid",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core generation head exceeds its wire bound",
        )
    }

    pub fn validate_sources(&self, sources: &[CoreSourceState]) -> Result<(), ProtocolError> {
        self.validate_source_snapshot(CoreSourceSnapshot::from_sources(sources.iter())?)
    }

    pub fn validate_source_snapshot(
        &self,
        sources: CoreSourceSnapshot,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        if self.source_count != sources.source_count
            || self.event_count != sources.event_count
            || self.source_snapshot_sha256 != sources.sha256
        {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "Core source snapshot does not match its generation head",
            ));
        }
        Ok(())
    }
}

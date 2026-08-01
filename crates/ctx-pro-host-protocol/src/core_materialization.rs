use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityId, StableEntityKind, CORE_CONTENT_POLICY_REVISION,
    CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION, CORE_REPOSITORY_CONTRACT_REVISION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ErrorClass, ProtocolError};

mod validation;
#[cfg(test)]
use validation::canonical_sha256;
pub use validation::{core_materialization_id, core_record_sha256, core_source_snapshot_sha256};
use validation::{
    core_record_content_bytes, core_source_delta_exact_eq, encode_with_bound, hex_sha256,
    invalid_contract, validate_encoded_bound, validate_identity, validate_sha256,
    validate_source_states,
};

pub const CORE_MATERIALIZATION_CONTRACT_VERSION: u16 = 1;
pub const MAX_CORE_SOURCE_STATES: usize = 16_384;
pub const MAX_CORE_SOURCE_DELTA_PAGE_ITEMS: usize = 256;
pub const MAX_CORE_EVENT_STATE_PAGE_ITEMS: usize = 256;
pub const MAX_CORE_EVENT_DELTA_PAGE_ITEMS: usize = 256;
pub const MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES: usize = 68 * 1024 * 1024;
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
pub struct CoreSourceRemoval {
    pub source: SourceKey,
    pub removal_revision_sha256: String,
}

impl CoreSourceRemoval {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("removed Core source identity", error))?;
        validate_sha256(
            &self.removal_revision_sha256,
            "Core source removal revision",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CoreSourceDelta {
    Present(CoreSourceState),
    Removed(CoreSourceRemoval),
}

impl CoreSourceDelta {
    pub fn source(&self) -> &SourceKey {
        match self {
            Self::Present(state) => &state.source,
            Self::Removed(removal) => &removal.source,
        }
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Present(state) => state.validate(),
            Self::Removed(removal) => removal.validate(),
        }
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
        validate_source_states(sources)?;
        let source_count = u32::try_from(sources.len())
            .map_err(|_| ProtocolError::new(ErrorClass::Bounds, "Core source count overflowed"))?;
        let event_count = sources.iter().try_fold(0_u64, |total, source| {
            total.checked_add(source.event_count).ok_or_else(|| {
                ProtocolError::new(ErrorClass::Bounds, "Core event count overflowed")
            })
        })?;
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
            source_snapshot_sha256: core_source_snapshot_sha256(sources)?,
            source_count,
            event_count,
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
            || usize::try_from(self.source_count)
                .ok()
                .is_none_or(|count| count > MAX_CORE_SOURCE_STATES)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core generation head revisions or source count are invalid",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core generation head exceeds its wire bound",
        )
    }

    pub fn validate_sources(&self, sources: &[CoreSourceState]) -> Result<(), ProtocolError> {
        self.validate()?;
        validate_source_states(sources)?;
        let event_count = sources.iter().try_fold(0_u64, |total, source| {
            total.checked_add(source.event_count).ok_or_else(|| {
                ProtocolError::new(ErrorClass::Bounds, "Core event count overflowed")
            })
        })?;
        if usize::try_from(self.source_count).ok() != Some(sources.len())
            || self.event_count != event_count
            || self.source_snapshot_sha256 != core_source_snapshot_sha256(sources)?
        {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "Core source snapshot does not match its generation head",
            ));
        }
        Ok(())
    }
}

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
    fn materialization_id(&self, materializer_revision: &str) -> Result<String, ProtocolError> {
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
pub struct CoreSourceDeltaPage {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub page_index: u32,
    pub terminal: bool,
    pub deltas: Vec<CoreSourceDelta>,
}

impl CoreSourceDeltaPage {
    pub fn new(
        materialization_id: impl Into<String>,
        core_generation_id: impl Into<String>,
        page_index: u32,
        terminal: bool,
        deltas: Vec<CoreSourceDelta>,
    ) -> Result<Self, ProtocolError> {
        let page = Self {
            materialization_id: materialization_id.into(),
            core_generation_id: core_generation_id.into(),
            page_index,
            terminal,
            deltas,
        };
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        if self.deltas.is_empty() || self.deltas.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core source delta page exceeds its item bound",
            ));
        }
        let mut prior = None;
        for delta in &self.deltas {
            delta.validate()?;
            let current = delta.source().identity().digest();
            if prior.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core source deltas must be strictly ordered by stable source identity",
                ));
            }
            prior = Some(current);
        }
        validate_encoded_bound(
            self,
            MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES,
            "Core source delta page exceeds its wire bound",
        )
    }

    /// Captures only the fields needed to validate the page acknowledgement.
    ///
    /// The page must already have passed [`Self::validate`]. Keeping this
    /// compact identity lets an internal producer move the owned page into the
    /// transport without retaining or re-encoding the complete request.
    pub fn acknowledgement_identity(&self) -> CoreSourceDeltaPageAcknowledgementIdentity {
        CoreSourceDeltaPageAcknowledgementIdentity {
            materialization_id: self.materialization_id.clone(),
            core_generation_id: self.core_generation_id.clone(),
            page_index: self.page_index,
            deltas: self.deltas.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSourceDeltaPageAcknowledgementIdentity {
    materialization_id: String,
    core_generation_id: String,
    page_index: u32,
    deltas: Vec<CoreSourceDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyCoreSourceDeltaPageRequest {
    pub page: CoreSourceDeltaPage,
}

impl ApplyCoreSourceDeltaPageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.page.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceDeltaPageApplied {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub page_index: u32,
    pub changed_sources: u32,
    pub removed_sources: u32,
    pub reconcile_sources: Vec<CoreSourceReconciliation>,
    pub replayed: bool,
}

impl CoreSourceDeltaPageApplied {
    pub fn validate_for(&self, page: &CoreSourceDeltaPage) -> Result<(), ProtocolError> {
        page.validate()?;
        self.validate_for_identity(&page.acknowledgement_identity())
    }

    pub fn validate_for_identity(
        &self,
        identity: &CoreSourceDeltaPageAcknowledgementIdentity,
    ) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        if self.reconcile_sources.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core source delta acknowledgement exceeds its reconciliation item bound",
            ));
        }
        let mut prior = None;
        let mut present = 0_usize;
        let mut removed = 0_usize;
        for requested in &self.reconcile_sources {
            requested.validate()?;
            let current = requested.delta.source().identity().digest();
            if prior.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "requested Core materialization sources must be strictly ordered",
                ));
            }
            let exact_delta = identity
                .deltas
                .iter()
                .any(|delta| core_source_delta_exact_eq(delta, &requested.delta));
            if !exact_delta {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core source delta acknowledgement requested an absent or stale reconciliation",
                ));
            }
            match &requested.delta {
                CoreSourceDelta::Present(_) => present += 1,
                CoreSourceDelta::Removed(_) => removed += 1,
            }
            prior = Some(current);
        }
        if self.materialization_id != identity.materialization_id
            || self.core_generation_id != identity.core_generation_id
            || self.page_index != identity.page_index
            || usize::try_from(self.changed_sources).ok() != Some(present)
            || usize::try_from(self.removed_sources).ok() != Some(removed)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core source delta acknowledgement does not match its page CAS",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceReconciliation {
    pub delta: CoreSourceDelta,
}

impl CoreSourceReconciliation {
    fn validate(&self) -> Result<(), ProtocolError> {
        self.delta.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventState {
    pub event_id: StableEntityId,
    pub core_record_sha256: String,
    pub requires_replacement: bool,
}

impl CoreEventState {
    fn validate_for_source(&self, source: &SourceKey) -> Result<(), ProtocolError> {
        self.event_id
            .validate_contract()
            .map_err(|error| invalid_contract("Core event state identity", error))?;
        if self.event_id.entity_kind() != StableEntityKind::Event
            || self.event_id.source_digest() != source.identity().digest()
            || self.event_id.source_descriptor_digest() != source.exact_descriptor_digest()
        {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "Core event state belongs to another source",
            ));
        }
        validate_sha256(&self.core_record_sha256, "Core record state")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventStatePageRequest {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub reconciliation: CoreSourceReconciliation,
    pub page_index: u32,
    pub after_event_id: Option<StableEntityId>,
    pub maximum_items: u32,
}

impl CoreEventStatePageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.reconciliation.validate()?;
        if self.maximum_items == 0
            || usize::try_from(self.maximum_items)
                .ok()
                .is_none_or(|limit| limit > MAX_CORE_EVENT_STATE_PAGE_ITEMS)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core event state page limit is invalid",
            ));
        }
        if let Some(after) = &self.after_event_id {
            CoreEventState {
                event_id: *after,
                core_record_sha256: "0".repeat(64),
                requires_replacement: false,
            }
            .validate_for_source(self.reconciliation.delta.source())?;
        }
        validate_encoded_bound(
            self,
            MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES,
            "Core event state page request exceeds its wire bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventStatePage {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub reconciliation: CoreSourceReconciliation,
    pub page_index: u32,
    pub after_event_id: Option<StableEntityId>,
    pub states: Vec<CoreEventState>,
    pub terminal: bool,
    pub replayed: bool,
}

impl CoreEventStatePage {
    pub fn validate_for(&self, request: &CoreEventStatePageRequest) -> Result<(), ProtocolError> {
        request.validate()?;
        if self.materialization_id != request.materialization_id
            || self.core_generation_id != request.core_generation_id
            || self.reconciliation != request.reconciliation
            || self.page_index != request.page_index
            || self.after_event_id != request.after_event_id
            || self.states.len() > usize::try_from(request.maximum_items).unwrap_or(0)
            || (!self.terminal && self.states.is_empty())
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core event state page does not match its request CAS",
            ));
        }
        let mut prior = request.after_event_id.map(|event| event.digest());
        for state in &self.states {
            state.validate_for_source(self.reconciliation.delta.source())?;
            let current = state.event_id.digest();
            if prior.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core event states must be strictly ordered by event identity",
                ));
            }
            prior = Some(current);
        }
        validate_encoded_bound(
            self,
            MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES,
            "Core event state page exceeds its wire bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventReplacement {
    pub prior_core_record_sha256: String,
    pub record: CoreRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventTombstone {
    pub event_id: StableEntityId,
    pub prior_core_record_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CoreEventDelta {
    Added(CoreRecord),
    Replaced(CoreEventReplacement),
    Tombstoned(CoreEventTombstone),
}

impl CoreEventDelta {
    pub fn event_id(&self) -> StableEntityId {
        match self {
            Self::Added(record) => record.event_id,
            Self::Replaced(replacement) => replacement.record.event_id,
            Self::Tombstoned(tombstone) => tombstone.event_id,
        }
    }

    fn record(&self) -> Option<&CoreRecord> {
        match self {
            Self::Added(record) => Some(record),
            Self::Replaced(replacement) => Some(&replacement.record),
            Self::Tombstoned(_) => None,
        }
    }

    fn validate_for_source(&self, source: &SourceKey) -> Result<(), ProtocolError> {
        let state = CoreEventState {
            event_id: self.event_id(),
            core_record_sha256: "0".repeat(64),
            requires_replacement: false,
        };
        state.validate_for_source(source)?;
        if let Some(record) = self.record() {
            record
                .validate_contract()
                .map_err(|error| invalid_contract("Core event delta record", error))?;
            if !record.source.exact_descriptor_eq(source) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "Core event delta record belongs to another source",
                ));
            }
        }
        match self {
            Self::Added(_) => {}
            Self::Replaced(replacement) => validate_sha256(
                &replacement.prior_core_record_sha256,
                "prior Core record state",
            )?,
            Self::Tombstoned(tombstone) => validate_sha256(
                &tombstone.prior_core_record_sha256,
                "prior Core record state",
            )?,
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventDeltaPage {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub reconciliation: CoreSourceReconciliation,
    pub page_index: u32,
    pub terminal: bool,
    pub deltas: Vec<CoreEventDelta>,
}

impl CoreEventDeltaPage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.reconciliation.validate()?;
        if self.deltas.len() > MAX_CORE_EVENT_DELTA_PAGE_ITEMS
            || (!self.terminal && self.deltas.is_empty())
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core event delta page exceeds its item bound or is empty before terminal",
            ));
        }
        let source = self.reconciliation.delta.source();
        let removing_source = matches!(&self.reconciliation.delta, CoreSourceDelta::Removed(_));
        let mut prior = None;
        let mut content_bytes = 0_usize;
        for delta in &self.deltas {
            delta.validate_for_source(source)?;
            if removing_source && !matches!(delta, CoreEventDelta::Tombstoned(_)) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "removed Core sources accept only event tombstones",
                ));
            }
            let current = delta.event_id().digest();
            if prior.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core event deltas must be strictly ordered by event identity",
                ));
            }
            prior = Some(current);
            if let Some(record) = delta.record() {
                content_bytes = content_bytes
                    .checked_add(core_record_content_bytes(record)?)
                    .ok_or_else(|| {
                        ProtocolError::new(
                            ErrorClass::Bounds,
                            "Core event delta content bytes overflowed",
                        )
                    })?;
            }
        }
        if content_bytes > MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core event delta page exceeds its selected-content byte bound",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
            "Core event delta page exceeds its wire bound",
        )
    }

    pub fn content_bytes(&self) -> Result<usize, ProtocolError> {
        self.deltas.iter().try_fold(0_usize, |total, delta| {
            total
                .checked_add(delta.record().map_or(Ok(0), core_record_content_bytes)?)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "Core event delta content bytes overflowed",
                    )
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyCoreEventDeltaPageRequest {
    pub page: CoreEventDeltaPage,
}

impl ApplyCoreEventDeltaPageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.page.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventDeltaPageApplied {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub source: SourceKey,
    pub page_index: u32,
    pub additions: u32,
    pub replacements: u32,
    pub tombstones: u32,
    pub terminal: bool,
    pub replayed: bool,
}

impl CoreEventDeltaPageApplied {
    pub fn validate_for(&self, page: &CoreEventDeltaPage) -> Result<(), ProtocolError> {
        page.validate()?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("Core event delta acknowledgement source", error))?;
        let additions = page
            .deltas
            .iter()
            .filter(|delta| matches!(delta, CoreEventDelta::Added(_)))
            .count();
        let replacements = page
            .deltas
            .iter()
            .filter(|delta| matches!(delta, CoreEventDelta::Replaced(_)))
            .count();
        let tombstones = page
            .deltas
            .iter()
            .filter(|delta| matches!(delta, CoreEventDelta::Tombstoned(_)))
            .count();
        if self.materialization_id != page.materialization_id
            || self.core_generation_id != page.core_generation_id
            || !self
                .source
                .exact_descriptor_eq(page.reconciliation.delta.source())
            || self.page_index != page.page_index
            || usize::try_from(self.additions).ok() != Some(additions)
            || usize::try_from(self.replacements).ok() != Some(replacements)
            || usize::try_from(self.tombstones).ok() != Some(tombstones)
            || self.terminal != page.terminal
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core event delta acknowledgement does not match its page CAS",
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

#[cfg(test)]
#[path = "core_materialization/tests.rs"]
mod tests;

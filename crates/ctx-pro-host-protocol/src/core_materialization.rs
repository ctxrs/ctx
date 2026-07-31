use std::collections::BTreeSet;

use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityKind, CORE_CONTENT_POLICY_REVISION,
    CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION, CORE_REPOSITORY_CONTRACT_REVISION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ErrorClass, ProtocolError};

pub const CORE_MATERIALIZATION_CONTRACT_VERSION: u16 = 1;
pub const MAX_CORE_SOURCE_STATES: usize = 16_384;
pub const MAX_CORE_SOURCE_DELTA_PAGE_ITEMS: usize = 256;
pub const MAX_CORE_RECORD_PAGE_ITEMS: usize = 256;
pub const MAX_CORE_RECORD_PAGE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CORE_RECORD_PAGE_WIRE_BYTES: usize = 68 * 1024 * 1024;
pub const MAX_CORE_CONTROL_WIRE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CORE_MATERIALIZER_REVISION_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceState {
    pub source: SourceKey,
    pub source_revision_sha256: String,
    pub event_count: u64,
}

impl CoreSourceState {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("Core source identity", error))?;
        validate_sha256(&self.source_revision_sha256, "Core source revision")
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
        self.head.validate()?;
        if let Some(receipt) = &self.expected_prior_receipt {
            receipt.validate()?;
        }
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "begin Core materialization request exceeds its wire bound",
        )
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
        request.validate()?;
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "Core materializer revision")?;
        if let Some(receipt) = &self.expected_prior_receipt {
            receipt.validate()?;
        }
        if self.core_generation_id != request.head.core_generation_id
            || self.expected_prior_receipt != request.expected_prior_receipt
            || self.materialization_id
                != core_materialization_id(request, &self.materializer_revision)?
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
    pub materialize_sources: Vec<CoreSourceState>,
    pub replayed: bool,
}

impl CoreSourceDeltaPageApplied {
    pub fn validate_for(&self, page: &CoreSourceDeltaPage) -> Result<(), ProtocolError> {
        page.validate()?;
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        if self.materialize_sources.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core source delta acknowledgement exceeds its materialization item bound",
            ));
        }
        let removed = page
            .deltas
            .iter()
            .filter(|delta| matches!(delta, CoreSourceDelta::Removed(_)))
            .count();
        let mut prior = None;
        for requested in &self.materialize_sources {
            requested.validate()?;
            let current = requested.source.identity().digest();
            if prior.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "requested Core materialization sources must be strictly ordered",
                ));
            }
            let exact_changed_state = page.deltas.iter().any(|delta| match delta {
                CoreSourceDelta::Present(present) => core_source_state_exact_eq(present, requested),
                CoreSourceDelta::Removed(_) => false,
            });
            if !exact_changed_state {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core source delta acknowledgement requested an absent or stale source revision",
                ));
            }
            prior = Some(current);
        }
        if self.materialization_id != page.materialization_id
            || self.core_generation_id != page.core_generation_id
            || self.page_index != page.page_index
            || usize::try_from(self.changed_sources).ok() != Some(self.materialize_sources.len())
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
pub struct CoreRecordPage {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub source: CoreSourceState,
    pub source_index: u32,
    pub page_index: u32,
    pub terminal: bool,
    pub records: Vec<CoreRecord>,
}

impl CoreRecordPage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        materialization_id: impl Into<String>,
        core_generation_id: impl Into<String>,
        source: CoreSourceState,
        source_index: u32,
        page_index: u32,
        terminal: bool,
        records: Vec<CoreRecord>,
    ) -> Result<Self, ProtocolError> {
        let page = Self {
            materialization_id: materialization_id.into(),
            core_generation_id: core_generation_id.into(),
            source,
            source_index,
            page_index,
            terminal,
            records,
        };
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.source.validate()?;
        if self.records.len() > MAX_CORE_RECORD_PAGE_ITEMS
            || (!self.terminal && self.records.is_empty())
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core record page exceeds its item bound or is empty before terminal",
            ));
        }
        let mut prior = None;
        let mut event_ids = BTreeSet::new();
        let mut content_bytes = 0_usize;
        for record in &self.records {
            record
                .validate_contract()
                .map_err(|error| invalid_contract("Core record", error))?;
            if !record.source.exact_descriptor_eq(&self.source.source)
                || record.event_id.entity_kind() != StableEntityKind::Event
            {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "Core record page contains a record owned by another source",
                ));
            }
            let current = record.event_id.digest();
            if prior.is_some_and(|prior| prior >= current) || !event_ids.insert(current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core record page records must be strictly ordered and unique by event identity",
                ));
            }
            prior = Some(current);
            content_bytes = content_bytes
                .checked_add(core_record_content_bytes(record)?)
                .ok_or_else(|| {
                    ProtocolError::new(ErrorClass::Bounds, "Core page content bytes overflowed")
                })?;
            if content_bytes > MAX_CORE_RECORD_PAGE_CONTENT_BYTES {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "Core record page exceeds its complete-content byte bound",
                ));
            }
        }
        validate_encoded_bound(
            self,
            MAX_CORE_RECORD_PAGE_WIRE_BYTES,
            "Core record page exceeds its wire bound",
        )
    }

    pub fn content_bytes(&self) -> Result<usize, ProtocolError> {
        self.records.iter().try_fold(0_usize, |total, record| {
            total
                .checked_add(core_record_content_bytes(record)?)
                .ok_or_else(|| {
                    ProtocolError::new(ErrorClass::Bounds, "Core page content bytes overflowed")
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializeCoreRecordPageRequest {
    pub page: CoreRecordPage,
}

impl MaterializeCoreRecordPageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.page.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRecordPageMaterialized {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub source: SourceKey,
    pub source_revision_sha256: String,
    pub source_index: u32,
    pub page_index: u32,
    pub accepted_records: u32,
    pub terminal: bool,
    pub replayed: bool,
}

impl CoreRecordPageMaterialized {
    pub fn validate_for(&self, page: &CoreRecordPage) -> Result<(), ProtocolError> {
        page.validate()?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("Core record acknowledgement source", error))?;
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.source_revision_sha256, "Core source revision")?;
        if self.materialization_id != page.materialization_id
            || self.core_generation_id != page.core_generation_id
            || !self.source.exact_descriptor_eq(&page.source.source)
            || self.source_revision_sha256 != page.source.source_revision_sha256
            || self.source_index != page.source_index
            || self.page_index != page.page_index
            || usize::try_from(self.accepted_records).ok() != Some(page.records.len())
            || self.terminal != page.terminal
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core record acknowledgement does not match its page CAS",
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
    pub record_pages: u32,
    pub materialized_records: u64,
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
            || self.materialized_records > self.head.event_count
            || (self.changed_sources == 0) != (self.record_pages == 0)
            || self.record_pages < self.changed_sources
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

pub fn core_source_snapshot_sha256(sources: &[CoreSourceState]) -> Result<String, ProtocolError> {
    validate_source_states(sources)?;
    canonical_sha256(sources, "Core source snapshot encoding failed")
}

pub fn core_materialization_id(
    request: &BeginCoreMaterializationRequest,
    materializer_revision: &str,
) -> Result<String, ProtocolError> {
    request.validate()?;
    validate_identity(materializer_revision, "Core materializer revision")?;
    canonical_sha256(
        &(request, materializer_revision),
        "Core materialization ID encoding failed",
    )
}

fn validate_source_states(sources: &[CoreSourceState]) -> Result<(), ProtocolError> {
    if sources.len() > MAX_CORE_SOURCE_STATES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "Core source snapshot exceeds its source count bound",
        ));
    }
    let mut prior = None;
    for source in sources {
        source.validate()?;
        let current = source.source.identity().digest();
        if prior.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core source snapshot must be strictly ordered by stable source identity",
            ));
        }
        prior = Some(current);
    }
    Ok(())
}

fn core_source_state_exact_eq(left: &CoreSourceState, right: &CoreSourceState) -> bool {
    left.source.exact_descriptor_eq(&right.source)
        && left.source_revision_sha256 == right.source_revision_sha256
        && left.event_count == right.event_count
}

fn core_record_content_bytes(record: &CoreRecord) -> Result<usize, ProtocolError> {
    let body = record
        .content
        .normalized_body
        .as_ref()
        .map_or(0, String::len);
    let structured = record
        .content
        .structured_content
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core structured content encoding failed",
            )
        })?
        .map_or(0, |encoded| encoded.len());
    body.checked_add(structured).ok_or_else(|| {
        ProtocolError::new(ErrorClass::Bounds, "Core record content bytes overflowed")
    })
}

fn validate_identity(value: &str, label: &'static str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_CORE_MATERIALIZER_REVISION_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{label} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &'static str) -> Result<(), ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{label} must be lowercase SHA-256"),
        ));
    }
    Ok(())
}

fn validate_encoded_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    message: &'static str,
) -> Result<(), ProtocolError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| ProtocolError::new(ErrorClass::Internal, "protocol encoding failed"))?;
    if encoded.len() > maximum {
        return Err(ProtocolError::new(ErrorClass::Bounds, message));
    }
    Ok(())
}

fn canonical_sha256<T: Serialize + ?Sized>(
    value: &T,
    message: &'static str,
) -> Result<String, ProtocolError> {
    let encoded =
        serde_json::to_vec(value).map_err(|_| ProtocolError::new(ErrorClass::Internal, message))?;
    let digest = Sha256::digest(encoded);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn invalid_contract(label: &'static str, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(ErrorClass::InvalidRequest, format!("{label}: {error}"))
}

#[cfg(test)]
#[path = "core_materialization/tests.rs"]
mod tests;

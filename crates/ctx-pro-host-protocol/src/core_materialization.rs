use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityId, StableEntityKind, CORE_CONTENT_POLICY_REVISION,
    CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION, CORE_REPOSITORY_CONTRACT_REVISION,
};
use serde::{Deserialize, Serialize};

use crate::{ErrorClass, ProtocolError};

mod control;
mod page_builder;
mod validation;
pub use control::{
    BeginCoreMaterializationRequest, CoreMaterializationBegan,
    CoreMaterializationBeginAcknowledgementIdentity, CoreMaterializationFinished,
    CoreMaterializationReceipt, CoreMaterializationReceiptIdentity,
    FinishCoreMaterializationRequest,
};
pub use page_builder::CoreEventDeltaPageBuilder;
#[cfg(test)]
use validation::canonical_sha256;
pub(crate) use validation::encoded_len;
pub use validation::{core_materialization_id, core_record_sha256, core_source_snapshot_sha256};
use validation::{
    core_record_content_bytes, core_source_delta_exact_eq, invalid_contract,
    validate_encoded_bound, validate_sha256, validate_source_states,
};

pub const CORE_MATERIALIZATION_CONTRACT_VERSION: u16 = 3;
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
}

impl CoreSourceRemoval {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("removed Core source identity", error))?;
        Ok(())
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
        if (!self.terminal && self.deltas.is_empty())
            || self.deltas.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core source delta page exceeds its item bound",
            ));
        }
        let mut prior = None;
        for delta in &self.deltas {
            delta.validate()?;
            if matches!(delta, CoreSourceDelta::Removed(_)) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "Core source pages are current snapshots and cannot carry removals",
                ));
            }
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
    pub fn acknowledgement_identity(
        &self,
        acknowledgement_page_index: u32,
    ) -> CoreSourceDeltaPageAcknowledgementIdentity {
        CoreSourceDeltaPageAcknowledgementIdentity {
            materialization_id: self.materialization_id.clone(),
            core_generation_id: self.core_generation_id.clone(),
            page_index: self.page_index,
            terminal: self.terminal,
            deltas: self.deltas.clone(),
            acknowledgement_page_index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSourceDeltaPageAcknowledgementIdentity {
    materialization_id: String,
    core_generation_id: String,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreSourceDelta>,
    acknowledgement_page_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyCoreSourceDeltaPageRequest {
    pub page: CoreSourceDeltaPage,
    pub acknowledgement_page_index: u32,
}

impl ApplyCoreSourceDeltaPageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.validate_with_control_frame_wire_bound(MAX_CORE_CONTROL_WIRE_BYTES)
    }

    fn validate_with_control_frame_wire_bound(&self, maximum: usize) -> Result<(), ProtocolError> {
        self.page.validate()?;
        if crate::apply_core_source_delta_page_request_frame_wire_bytes(u64::MAX, self)? > maximum {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core source delta request exceeds its complete frame wire bound",
            ));
        }
        Ok(())
    }

    /// Captures the complete source-page and acknowledgement-page CAS before
    /// an owned request moves into the transport.
    pub fn acknowledgement_identity(&self) -> CoreSourceDeltaPageAcknowledgementIdentity {
        self.page
            .acknowledgement_identity(self.acknowledgement_page_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceDeltaPageApplied {
    pub materialization_id: String,
    pub core_generation_id: String,
    pub page_index: u32,
    pub acknowledgement_page_index: u32,
    pub acknowledgement_terminal: bool,
    pub changed_sources: u32,
    pub removed_sources: u32,
    pub reconcile_sources: Vec<CoreSourceReconciliation>,
    pub replayed: bool,
}

impl CoreSourceDeltaPageApplied {
    pub fn validate_for(
        &self,
        request: &ApplyCoreSourceDeltaPageRequest,
    ) -> Result<(), ProtocolError> {
        request.validate()?;
        self.validate_for_identity(&request.acknowledgement_identity())
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
        if !self.acknowledgement_terminal && self.reconcile_sources.is_empty() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core source delta acknowledgement cannot be empty before terminal",
            ));
        }
        if !identity.terminal && !self.acknowledgement_terminal {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "nonterminal Core source delta pages must complete in one acknowledgement page",
            ));
        }
        let mut prior_index = None;
        let mut present = 0_usize;
        let mut removed = 0_usize;
        for requested in &self.reconcile_sources {
            requested.validate()?;
            if prior_index.is_some_and(|prior| prior >= requested.materialize_index) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "requested Core materialization indices must be strictly ordered",
                ));
            }
            match &requested.delta {
                CoreSourceDelta::Present(_) => {
                    if identity.acknowledgement_page_index != 0 {
                        return Err(ProtocolError::new(
                            ErrorClass::Sequence,
                            "current Core sources are valid only on acknowledgement page zero",
                        ));
                    }
                    let exact_delta = identity
                        .deltas
                        .iter()
                        .any(|delta| core_source_delta_exact_eq(delta, &requested.delta));
                    if !exact_delta {
                        return Err(ProtocolError::new(
                            ErrorClass::Sequence,
                            "Core source acknowledgement requested an absent or stale current source",
                        ));
                    }
                    present += 1;
                }
                CoreSourceDelta::Removed(_) if identity.terminal => removed += 1,
                CoreSourceDelta::Removed(_) => {
                    return Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "stored-minus-snapshot removals are valid only on the terminal source page",
                    ));
                }
            }
            prior_index = Some(requested.materialize_index);
        }
        if self.materialization_id != identity.materialization_id
            || self.core_generation_id != identity.core_generation_id
            || self.page_index != identity.page_index
            || self.acknowledgement_page_index != identity.acknowledgement_page_index
            || usize::try_from(self.changed_sources).ok() != Some(present)
            || usize::try_from(self.removed_sources).ok() != Some(removed)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core source delta acknowledgement does not match its page CAS",
            ));
        }
        if crate::core_source_delta_page_applied_frame_wire_bytes(u64::MAX, self)?
            > MAX_CORE_CONTROL_WIRE_BYTES
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core source delta acknowledgement exceeds its complete frame wire bound",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceReconciliation {
    pub materialize_index: u32,
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
        validate_core_event_delta_page_header(
            &self.materialization_id,
            &self.core_generation_id,
            &self.reconciliation,
        )?;
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

    /// Captures only the fields needed to validate the page acknowledgement.
    ///
    /// The page must already have passed [`Self::validate`]. Keeping this
    /// compact identity lets a producer move the owned page into transport
    /// without retaining or revalidating the complete request.
    pub fn acknowledgement_identity(&self) -> CoreEventDeltaPageAcknowledgementIdentity {
        let mut additions = 0_u32;
        let mut replacements = 0_u32;
        let mut tombstones = 0_u32;
        for delta in &self.deltas {
            match delta {
                CoreEventDelta::Added(_) => additions += 1,
                CoreEventDelta::Replaced(_) => replacements += 1,
                CoreEventDelta::Tombstoned(_) => tombstones += 1,
            }
        }
        CoreEventDeltaPageAcknowledgementIdentity {
            materialization_id: self.materialization_id.clone(),
            core_generation_id: self.core_generation_id.clone(),
            source: self.reconciliation.delta.source().clone(),
            page_index: self.page_index,
            additions,
            replacements,
            tombstones,
            terminal: self.terminal,
        }
    }
}

fn validate_core_event_delta_page_header(
    materialization_id: &str,
    core_generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
) -> Result<(), ProtocolError> {
    validate_sha256(materialization_id, "Core materialization ID")?;
    validate_sha256(core_generation_id, "Core generation ID")?;
    reconciliation.validate()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventDeltaPageAcknowledgementIdentity {
    materialization_id: String,
    core_generation_id: String,
    source: SourceKey,
    page_index: u32,
    additions: u32,
    replacements: u32,
    tombstones: u32,
    terminal: bool,
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
        self.validate_for_identity(&page.acknowledgement_identity())
    }

    pub fn validate_for_identity(
        &self,
        identity: &CoreEventDeltaPageAcknowledgementIdentity,
    ) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("Core event delta acknowledgement source", error))?;
        if self.materialization_id != identity.materialization_id
            || self.core_generation_id != identity.core_generation_id
            || !self.source.exact_descriptor_eq(&identity.source)
            || self.page_index != identity.page_index
            || self.additions != identity.additions
            || self.replacements != identity.replacements
            || self.tombstones != identity.tombstones
            || self.terminal != identity.terminal
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core event delta acknowledgement does not match its page CAS",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "core_materialization/tests.rs"]
mod tests;

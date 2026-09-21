use ctx_history_core::{CoreRecord, SourceKey, StableEntityId, StableEntityKind};
use serde::{Deserialize, Serialize};

use ctx_attribution_model::{
    CoreSourceState, ErrorClass, MAX_CORE_CONTROL_WIRE_BYTES, ProtocolError,
};

mod validation;
use validation::{
    core_record_content_bytes, invalid_contract, validate_encoded_bound, validate_sha256,
};

pub const MAX_CORE_SOURCE_DELTA_PAGE_ITEMS: usize = 256;
use ctx_attribution_index::MAX_EVENT_INDEX_PAGE_ITEMS;
pub const MAX_CORE_EVENT_DELTA_PAGE_ITEMS: usize = 256;
pub const MAX_CORE_EVENT_DELTA_PAGES: usize = 16;
pub const MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES: usize = 68 * 1024 * 1024;

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
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        if self.reconcile_sources.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
            || (!self.acknowledgement_terminal && self.reconcile_sources.is_empty())
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "historical Core source delta effect exceeds its item bound",
            ));
        }
        let mut prior_index = None;
        let mut changed = 0_usize;
        let mut removed = 0_usize;
        for reconciliation in &self.reconcile_sources {
            reconciliation.validate()?;
            if prior_index.is_some_and(|prior| prior >= reconciliation.materialize_index) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "historical Core source reconciliations are not strictly ordered",
                ));
            }
            match reconciliation.delta {
                CoreSourceDelta::Present(_) => changed += 1,
                CoreSourceDelta::Removed(_) => removed += 1,
            }
            prior_index = Some(reconciliation.materialize_index);
        }
        if usize::try_from(self.changed_sources).ok() != Some(changed)
            || usize::try_from(self.removed_sources).ok() != Some(removed)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "historical Core source reconciliation counts are inconsistent",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "historical Core source delta effect exceeds its wire bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSourceReconciliation {
    pub materialize_index: u32,
    pub delta: CoreSourceDelta,
}

impl CoreSourceReconciliation {
    pub fn validate(&self) -> Result<(), ProtocolError> {
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
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.reconciliation.validate()?;
        if self.states.len() > MAX_EVENT_INDEX_PAGE_ITEMS
            || (!self.terminal && self.states.is_empty())
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "historical Core event state effect exceeds its item bound",
            ));
        }
        if let Some(after) = self.after_event_id {
            CoreEventState {
                event_id: after,
                core_record_sha256: "0".repeat(64),
                requires_replacement: false,
            }
            .validate_for_source(self.reconciliation.delta.source())?;
        }
        let mut prior = self.after_event_id.map(|event| event.digest());
        for state in &self.states {
            state.validate_for_source(self.reconciliation.delta.source())?;
            let current = state.event_id.digest();
            if prior.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "historical Core event states are not strictly ordered",
                ));
            }
            prior = Some(current);
        }
        validate_encoded_bound(
            self,
            MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES,
            "historical Core event state effect exceeds its wire bound",
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

    /// Returns the immutable Core record carried by an add or replacement.
    #[must_use]
    pub fn record(&self) -> Option<&CoreRecord> {
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
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.materialization_id, "Core materialization ID")?;
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("historical Core event delta source", error))?;
        let mutations = self
            .additions
            .checked_add(self.replacements)
            .and_then(|count| count.checked_add(self.tombstones))
            .ok_or_else(|| {
                ProtocolError::new(
                    ErrorClass::Bounds,
                    "historical Core event delta count overflowed",
                )
            })?;
        if usize::try_from(mutations)
            .ok()
            .is_none_or(|count| count > MAX_CORE_EVENT_DELTA_PAGE_ITEMS)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "historical Core event delta effect exceeds its item bound",
            ));
        }
        Ok(())
    }
}

//! Private serde adapters; no published wire format or index API changes.
use crate::{
    EventIndexEntry, IndexedCopiedEventOrigin, IndexedCoreEventLineage, IndexedCoreEventOriginKind,
    IndexedCoreEventState, IndexedCoreEventTombstone, SegmentCoreCoverage,
};
use ctx_attribution_model::{EventCopyProofKind, SessionRelationshipKind};
use ctx_history_core::StableEntityId;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(remote = "EventIndexEntry")]
#[expect(
    clippy::large_enum_variant,
    reason = "Serde remote definition is never instantiated; it mirrors the existing by-value event enum"
)]
pub(super) enum Entry {
    State {
        #[serde(with = "State")]
        state: IndexedCoreEventState,
        shadows_older: bool,
    },
    Tombstone(#[serde(with = "Tombstone")] IndexedCoreEventTombstone),
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "IndexedCoreEventState")]
struct State {
    source_storage_key: String,
    event_id: StableEntityId,
    #[serde(with = "Lineage")]
    lineage: IndexedCoreEventLineage,
    event_sequence: u64,
    core_record_sha256: String,
    core_record_leaf_sha256: String,
    flat_record_count: u32,
    event_output_root: String,
    coverage: SegmentCoreCoverage,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "IndexedCoreEventTombstone")]
struct Tombstone {
    source_storage_key: String,
    event_id: StableEntityId,
    prior_event_state_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "IndexedCoreEventLineage")]
struct Lineage {
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    session_relationship: SessionRelationshipKind,
    #[serde(with = "Origin")]
    origin_kind: IndexedCoreEventOriginKind,
    #[serde(with = "copied_option")]
    copied_from: Option<IndexedCopiedEventOrigin>,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "IndexedCoreEventOriginKind")]
enum Origin {
    Unknown,
    UniqueToSession,
    CopiedFromAncestor,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "IndexedCopiedEventOrigin")]
struct Copied {
    ancestor_session_id: StableEntityId,
    ancestor_event_id: StableEntityId,
    proof: EventCopyProofKind,
}
mod copied_option {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<IndexedCopiedEventOrigin>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Value<'a>(#[serde(with = "Copied")] &'a IndexedCopiedEventOrigin);
        value.as_ref().map(Value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<IndexedCopiedEventOrigin>, D::Error> {
        #[derive(Deserialize)]
        struct Value(#[serde(with = "Copied")] IndexedCopiedEventOrigin);
        Ok(Option::<Value>::deserialize(deserializer)?.map(|value| value.0))
    }
}

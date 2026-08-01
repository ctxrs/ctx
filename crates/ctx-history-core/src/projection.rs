//! Shared contracts for source-backed history projections.
//!
//! Provider adapters own discovery and native parsing. They do not own
//! projection identity, source certification, or publication state machines.

mod certification;
mod errors;
mod identity;
mod native;
mod source;

pub use certification::{
    CertifiedSource, CertifiedSourceAppend, ScannedSourceCounts, SourceFrontier,
};
pub use errors::{ProjectionContractError, ProjectionContractResult};
pub use identity::{
    derive_event_id, derive_session_id, EventIdentityInput, SessionIdentityInput, StableEntityId,
    StableEntityKind, IDENTITY_VERSION, STABLE_ENTITY_ID_CANONICAL_LEN,
};
pub use native::{NativeItemKey, NativeSessionKey, PositionStability, SubrecordSelector, TypedKey};
pub use source::{
    CertifiedSourceDeletion, CertifiedSourceInventory, SourceAnchor, SourceInventoryObservation,
    SourceKey, SourceObservation,
};

#[cfg(test)]
use identity::{
    STABLE_ENTITY_ID_DIGEST_OFFSET, STABLE_ENTITY_ID_KIND_OFFSET,
    STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET, STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET,
    STABLE_ENTITY_ID_UUID_OFFSET,
};

#[cfg(test)]
use native::{encode_native_item_key, encode_native_session_key, encode_subrecord_selector};

#[cfg(test)]
mod tests;

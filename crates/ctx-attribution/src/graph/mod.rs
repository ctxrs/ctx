//! Work-owned semantic state and immutable query graph.

pub(crate) mod git;
mod identity;
#[allow(dead_code)]
#[doc(hidden)]
pub mod model;
#[doc(hidden)]
pub mod segment;
pub mod segment_graph;
pub mod segment_state;

pub use identity::{
    GRAPH_EVIDENCE_FINGERPRINT, GRAPH_SCHEMA_FINGERPRINT, GRAPH_SEMANTICS_FINGERPRINT,
};

/// Whether a prepared attribution fact family can be consumed by the shipping Flat
/// serving projection.
#[doc(hidden)]
pub fn is_serving_fact_family(value: &str) -> bool {
    segment::SCHEMA_CRITICAL_FACT_FAMILIES.contains(&value)
}

#[doc(hidden)]
pub fn fact_type_may_confer_producer_authority(value: &str) -> bool {
    segment::fact_type_may_confer_producer_authority(value)
}

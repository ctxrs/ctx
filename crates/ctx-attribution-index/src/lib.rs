//! Bounded derived attribution indexes with immutable plaintext generations.

mod coverage;
pub mod event_index;
mod filesystem;
pub mod flat;
mod identity;
pub mod manifest;
pub mod model;
pub mod pinned_generation;
pub mod projection_commitment;
pub mod segment_file;
pub mod store;

pub use coverage::{MAX_SEGMENT_CORE_EVENTS, SegmentCoreCoverage};
pub use event_index::*;
pub use flat::*;
pub use identity::{core_source_storage_id, stable_id, stable_id_bytes};
pub use manifest::*;
pub use model::*;
pub use pinned_generation::*;
pub use projection_commitment::*;
pub use segment_file::*;
pub use store::*;

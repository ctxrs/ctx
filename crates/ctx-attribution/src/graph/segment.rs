//! Work-graph model and projection over Flat-store serving primitives.

pub(crate) use ctx_attribution_index::*;

#[doc(hidden)]
pub mod model;
#[doc(hidden)]
pub mod projection;

pub use model::{ServingCitationQueryExt, ServingRecordQueryExt, ServingResourceQueryExt};
pub use projection::{
    CorePageProjectionMetrics, PreparedCorePageProjection, ProjectedCoreBatch,
    ProjectedCoreRecordEvidence, ProjectionError, ProjectionOmission, ProjectionOmissionReason,
    prepare_core_page_projection, project_core_batch, project_tombstone,
    projected_core_record_evidence,
};

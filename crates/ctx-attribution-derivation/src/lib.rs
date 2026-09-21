//! Attribution facts and evidence previews derived from retained Core records.
//!
//! Callers own snapshot pins, index publication, queries, and rendering. This
//! library prepares bounded factual units and previews from supplied records.

pub mod core_materialization;
pub mod envelope;
pub mod evidence_preview;
pub mod feed_model;
pub mod ingest;
mod protocol;
pub mod worker_budget;

pub use core_materialization::{CoreProjectionPreparer, PreparedCoreEventDeltaPage};
pub use evidence_preview::{VerifiedEvidenceRecord, project_evidence_previews};

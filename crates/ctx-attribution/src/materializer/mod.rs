//! Bounded direct Core materialization and atomic index publication.

mod lifecycle;
pub(crate) mod locking;
pub(crate) mod model;
mod publication;
mod publication_plan;
mod reconciliation_cursor;
mod staging;
mod storage;
#[cfg(test)]
mod tests;

pub use crate::core_materialization::CORE_MATERIALIZER_REVISION;
pub use lifecycle::{CoreGenerationStart, CoreMaterializationSession};

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use ctx_attribution_index::SegmentFileError;
use ctx_attribution_index::{EventIndexError, FlatSegmentError, ManifestError, SegmentStoreError};

/// Immutable-segment implementation of the Core graph lifecycle.
pub struct SegmentMaterializer {
    root: PathBuf,
    expected_materializer_revision: Option<String>,
    active: Option<publication::ActiveGeneration>,
    force_next_rebuild: bool,
    metrics: model::MaterializerMetrics,
    writer_lease: Option<locking::OperationLock>,
    rollback_cleanup_failed: bool,
}

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum SegmentMaterializerError {
    #[error("segment materializer I/O failed while {operation} {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("segment materializer state is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("segment materializer exceeded a hard bound")]
    Bounds,
    #[error("segment materializer requires a complete projection rebuild")]
    RebuildRequired,
    #[error("segment materializer compare-and-swap failed")]
    Conflict,
    #[error("another segment materializer operation is active")]
    Busy,
    #[error("cancelled: attribution materialization was cancelled")]
    Cancelled,
    #[error("segment materializer encoding is invalid")]
    Encoding,
    #[error(transparent)]
    File(#[from] SegmentFileError),
    #[error(transparent)]
    Manifest(ManifestError),
    #[error(transparent)]
    Store(SegmentStoreError),
    #[error(transparent)]
    Flat(FlatSegmentError),
    #[error(transparent)]
    EventIndex(EventIndexError),
    #[error(transparent)]
    Graph(#[from] crate::graph::segment_graph::SegmentGraphError),
}

impl From<ManifestError> for SegmentMaterializerError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<SegmentStoreError> for SegmentMaterializerError {
    fn from(error: SegmentStoreError) -> Self {
        match error {
            SegmentStoreError::CompareAndSwap { .. } => Self::Conflict,
            other => Self::Store(other),
        }
    }
}

impl From<FlatSegmentError> for SegmentMaterializerError {
    fn from(error: FlatSegmentError) -> Self {
        match error {
            FlatSegmentError::Bound(_) => Self::Bounds,
            other => Self::Flat(other),
        }
    }
}

impl From<EventIndexError> for SegmentMaterializerError {
    fn from(error: EventIndexError) -> Self {
        Self::EventIndex(error)
    }
}

impl From<crate::core_materialization::CoreStoreError> for SegmentMaterializerError {
    fn from(error: crate::core_materialization::CoreStoreError) -> Self {
        match error {
            crate::core_materialization::CoreStoreError::Conflict => Self::Conflict,
            crate::core_materialization::CoreStoreError::Bounds => Self::Bounds,
            crate::core_materialization::CoreStoreError::RebuildRequired => Self::RebuildRequired,
            crate::core_materialization::CoreStoreError::Backend => {
                Self::Corrupt("Core feed rejected the materialization state")
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct StatusRequest {
    pub requested_core_generation_id: Option<String>,
}

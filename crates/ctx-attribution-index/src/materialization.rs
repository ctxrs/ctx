//! Disposable ordered indexes used while reconciling immutable generations.
//! These files are runtime scratch, never published format or recovery state.

pub mod event_proofs;
pub mod runtime_file;
pub mod source_inventory;

/// Per-segment source metadata representation bound, independent of inventory size.
pub const MAX_METADATA_SEGMENT_ENTRIES: usize = 100_000;

#[derive(Debug, thiserror::Error)]
pub enum MaterializationIndexError {
    #[error("materialization index I/O failed while {operation} {path}")]
    Io {
        operation: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cancelled: materialization index preparation was cancelled")]
    Cancelled,
    #[error("materialization index exceeded a representation bound")]
    Bounds,
    #[error("materialization index ordering or identity conflict")]
    Conflict,
    #[error("materialization index is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("materialization index encoding is invalid")]
    Encoding,
    #[error(transparent)]
    EventIndex(#[from] crate::EventIndexError),
}

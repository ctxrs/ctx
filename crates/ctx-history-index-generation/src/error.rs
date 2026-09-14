use thiserror::Error;

pub type Result<T> = std::result::Result<T, GenerationError>;

/// Tantivy retains several I/O errors in fields instead of its source chain.
/// Both generation and query errors use this one resource-diagnostic adapter.
pub fn tantivy_file_limit_hint(error: &tantivy::TantivyError) -> String {
    use tantivy::directory::error::{OpenDirectoryError, OpenReadError, OpenWriteError};
    use tantivy::TantivyError;
    let source = match error {
        TantivyError::IoError(source)
        | TantivyError::OpenReadError(OpenReadError::IoError {
            io_error: source, ..
        })
        | TantivyError::OpenWriteError(OpenWriteError::IoError {
            io_error: source, ..
        })
        | TantivyError::OpenDirectoryError(OpenDirectoryError::IoError {
            io_error: source, ..
        })
        | TantivyError::OpenDirectoryError(OpenDirectoryError::FailedToCreateTempDir(source)) => {
            source
        }
        _ => return String::new(),
    };
    ctx_history_platform::open_file_limit_hint(source)
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("{0}{}", ctx_history_platform::open_file_limit_hint(.0))]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}{}", tantivy_file_limit_hint(.0))]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("the lexical index has no active generation pointer")]
    MissingActiveGenerationPointer,
    #[error("unsupported active generation pointer version {0}")]
    UnsupportedActiveGenerationPointer(u32),
    #[error("the active generation pointer is malformed or non-canonical")]
    InvalidActiveGenerationPointer,
    #[error("the durable generation-retention lease is malformed, non-canonical, or not owner-private; resolve the unfinished lease owner before publishing Core")]
    InvalidGenerationRetentionLease,
    #[error("unsupported durable generation-retention lease version {0}")]
    UnsupportedGenerationRetentionLease(u32),
    #[error("generation-retention lease owner kind or identity is invalid")]
    InvalidGenerationRetentionLeaseOwner,
    #[error("generation {requested_generation_id} cannot be leased because it is not the active or previous retained generation")]
    GenerationRetentionLeaseTargetNotRetained { requested_generation_id: String },
    #[error("generation-retention lease already holds generation {retained_generation_id} for owner kind {owner_kind}")]
    GenerationRetentionLeaseConflict {
        retained_generation_id: String,
        owner_kind: String,
    },
    #[error("generation-retention lease owner changed before release")]
    GenerationRetentionLeaseOwnerMismatch,
    #[error("the generation id is malformed")]
    InvalidGenerationId,
    #[error("generation manifest {0} is missing")]
    MissingManifest(String),
    #[error("generation manifest digest mismatch: expected {expected}, got {actual}")]
    ManifestDigestMismatch { expected: String, actual: String },
    #[error("lexical index settings do not match the current schema contract")]
    IndexSettingsMismatch,
    #[error("generation changed concurrently")]
    ConcurrentGenerationChange,
    #[error("generation artifact checksum does not match its physical authority")]
    ChecksumMismatch,
    #[error("current-generation republish source topology is unsafe: {0}")]
    CurrentRepublishSourceTopology(&'static str),
    #[error("current-generation republish exceeds file limit: {actual} > {maximum}")]
    CurrentRepublishFileLimit { actual: usize, maximum: usize },
    #[error("current-generation republish exceeds byte limit: {actual} > {maximum}")]
    CurrentRepublishByteLimit { actual: u64, maximum: u64 },
    #[error("current-generation republish has insufficient headroom: required {required}, available {available}")]
    CurrentRepublishInsufficientHeadroom { required: u64, available: u64 },
    #[error("count overflow")]
    CountOverflow,
}

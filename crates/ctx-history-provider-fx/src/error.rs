use thiserror::Error;

pub type FxProviderResult<T> = Result<T, FxProviderError>;

#[derive(Debug, Error)]
pub enum FxProviderError {
    #[error("fx I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("fx JSON is malformed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("fx authority is invalid: {0}")]
    InvalidAuthority(&'static str),
    #[error("fx watermark is invalid: {0}")]
    InvalidWatermark(&'static str),
    #[error("fx event frame is invalid: {0}")]
    InvalidFrame(&'static str),
    #[error("unsupported fx event schema {0}")]
    UnsupportedEventSchema(u64),
    #[error("unknown fx state-changing event kind {0}")]
    UnknownEventKind(String),
    #[error("unsupported marker-less fx legacy schema {0}")]
    UnsupportedLegacySchema(u64),
    #[error("fx legacy snapshot is invalid: {0}")]
    InvalidLegacy(&'static str),
    #[error("fx event sequence is not contiguous: expected {expected}, found {actual}")]
    NonContiguousSequence { expected: u64, actual: u64 },
    #[error("fx log generation changed within the committed prefix")]
    GenerationChanged,
    #[error("fx committed boundary does not match its watermark")]
    WatermarkMismatch,
    #[error("fx canonical state is invalid: {0}")]
    InvalidState(&'static str),
    #[error("fx state replacement is invalid: {0}")]
    InvalidReplacement(&'static str),
    #[error("fx {resource} limit exceeded: {actual}, maximum {maximum}")]
    LimitExceeded {
        resource: &'static str,
        actual: u64,
        maximum: u64,
    },
    #[error("fx Core projection is invalid: {0}")]
    Projection(#[from] ctx_history_core::ProjectionContractError),
    #[error("fx Core record is invalid: {0}")]
    Core(#[from] ctx_history_core::CoreRecordError),
}

impl FxProviderError {
    /// Provider bytes caused this failure. I/O and scratch failures remain
    /// systemic and are intentionally not classified as source corruption.
    pub const fn is_source_fatal(&self) -> bool {
        !matches!(self, Self::Io(_))
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    EntitlementExpired,
    KeyStoreUnavailable,
    KeyStoreLocked,
    NotMaterialized,
    ProtocolMismatch,
    MissingSource,
    MissingRepository,
    ResourceNotFound,
    StaleFact,
    LineOutOfRange,
    StaleSnapshot,
    Ambiguous,
    Corrupt,
    InvalidRequest,
    Bounds,
    RebuildRequired,
    Sequence,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    pub class: ErrorClass,
    pub message: String,
    pub retryable: bool,
}

impl ProtocolError {
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retryable: false,
        }
    }
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("managed-pair installation verification is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("managed-pair installation verification failed: {0}")]
    Verification(String),
}

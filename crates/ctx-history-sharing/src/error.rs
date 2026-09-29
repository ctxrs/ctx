use serde::{Deserialize, Serialize};

/// Deliberately excludes remote bodies, URLs, paths, and bearer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    #[error("sharing endpoint must be HTTPS (HTTP is allowed only on literal loopback)")]
    InvalidEndpoint,
    #[error("sharing configuration is invalid")]
    InvalidConfig,
    #[error("sharing credentials are invalid or not owner-private")]
    Credentials,
    #[error("the required sharing credential is absent")]
    MissingCredential,
    #[error("sharing state could not be read or saved")]
    State,
    #[error("sharing is not connected")]
    NotConnected,
    #[error("sharing state is already bound to another destination")]
    DestinationChanged,
    #[error("sharing policy revision must increase")]
    PolicyConflict,
    #[error("sharing is paused or the revision is outside the current policy")]
    PolicyDenied,
    #[error("another sharing worker owns this queue")]
    Busy,
    #[error("remote history is unavailable")]
    Unavailable,
    #[error("remote history authentication failed")]
    Unauthorized,
    #[error("remote history access is denied")]
    Forbidden,
    #[error("remote history was not found")]
    NotFound,
    #[error("remote history revision or writer conflicts with the server")]
    Conflict,
    #[error("remote upload staging expired")]
    StagingExpired,
    #[error("remote history request is too large")]
    TooLarge,
    #[error("remote history is rate limited")]
    RateLimited,
    #[error("remote history returned an invalid response")]
    Protocol,
    #[error("remote history request was rejected (HTTP {0})")]
    HttpStatus(u16),
    #[error("committed history could not be prepared for sharing")]
    Archive,
}

impl Error {
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::RateLimited | Self::StagingExpired
        )
    }

    pub(crate) fn http(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            409 => Self::Conflict,
            410 => Self::StagingExpired,
            413 => Self::TooLarge,
            429 => Self::RateLimited,
            500..=599 => Self::Unavailable,
            _ => Self::HttpStatus(status),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

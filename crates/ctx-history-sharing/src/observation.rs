//! Exact local facts only. A caller may bucket and deliver them after consent.
use crate::{Error, SelectionDecision, TickOutcome};
use std::{sync::Arc, time::Duration};

/// Callbacks must be bounded, in-memory and non-panicking.
pub type SharingObserver = Arc<dyn Fn(SharingObservation) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingFailure {
    Configuration,
    Credentials,
    State,
    NotConnected,
    DestinationChanged,
    PolicyConflict,
    PolicyDenied,
    Busy,
    Unavailable,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    StagingExpired,
    TooLarge,
    RateLimited,
    Protocol,
    HttpRejected,
    Archive,
}

impl From<&Error> for SharingFailure {
    fn from(error: &Error) -> Self {
        (*error).into()
    }
}

impl From<Error> for SharingFailure {
    fn from(error: Error) -> Self {
        match error {
            Error::InvalidEndpoint | Error::InvalidConfig => Self::Configuration,
            Error::Credentials | Error::MissingCredential => Self::Credentials,
            Error::State => Self::State,
            Error::NotConnected => Self::NotConnected,
            Error::DestinationChanged => Self::DestinationChanged,
            Error::PolicyConflict => Self::PolicyConflict,
            Error::PolicyDenied => Self::PolicyDenied,
            Error::Busy => Self::Busy,
            Error::Unavailable => Self::Unavailable,
            Error::Unauthorized => Self::Unauthorized,
            Error::Forbidden => Self::Forbidden,
            Error::NotFound => Self::NotFound,
            Error::Conflict => Self::Conflict,
            Error::StagingExpired => Self::StagingExpired,
            Error::TooLarge => Self::TooLarge,
            Error::RateLimited => Self::RateLimited,
            Error::Protocol => Self::Protocol,
            Error::HttpStatus(_) => Self::HttpRejected,
            Error::Archive => Self::Archive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingPhase {
    Settings,
    Admission,
    Capture,
    BeginUpload,
    UploadStatus,
    UploadChunk,
    Publish,
    Receipt,
    Settlement,
    Checkpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingTick {
    Disabled,
    Paused,
    Idle,
    Progress,
    Failed,
}

impl From<TickOutcome> for SharingTick {
    fn from(outcome: TickOutcome) -> Self {
        match outcome {
            TickOutcome::Disabled => Self::Disabled,
            TickOutcome::Paused => Self::Paused,
            TickOutcome::Idle => Self::Idle,
            TickOutcome::Progress => Self::Progress,
            TickOutcome::Failed(_) => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingObservation {
    WorkerStarted,
    WorkerStartFailed(SharingFailure),
    WorkerStopped,
    Tick {
        phase: SharingPhase,
        outcome: SharingTick,
        duration: Duration,
        failure: Option<SharingFailure>,
        /// A successful progress tick after a failed tick, not proof that all
        /// publications or the entire destination have recovered.
        progress_after_failure: bool,
    },
    Selection {
        decision: SelectionDecision,
        count: u64,
        complete: bool,
    },
    Queued {
        bytes: u64,
        records: u64,
    },
    Transfer {
        bytes: u64,
    },
    /// A validated receipt has been saved and its pending entry retired.
    Accepted {
        bytes: u64,
        records: u64,
        recovered_receipt: bool,
    },
    Settled {
        already_accepted: bool,
    },
    Retry {
        phase: SharingPhase,
        failure: SharingFailure,
        attempts: u32,
        delay: Duration,
    },
}

//! Optional, content-free engine observations. Callbacks must be bounded,
//! in-memory and non-panicking. Consent and delivery belong to the caller.
use std::{sync::Arc, time::Duration};

use crate::{Error, HistoryServer};

pub type ServerObserver = Arc<dyn Fn(ServerObservation) + Send + Sync>;
pub type ServerRuntimeHook = Arc<dyn Fn(ServerRuntimeTick) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRuntimeTick {
    Ready,
    Interval,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOperation {
    Health,
    Enroll,
    Principals,
    Credentials,
    RevokePrincipal,
    RevokeCredential,
    Whoami,
    Invite,
    Grants,
    RevokeMember,
    BeginUpload,
    UploadStatus,
    UploadChunk,
    Publish,
    CancelPublish,
    Withdraw,
    Remove,
    Receipt,
    Publications,
    Publication,
    Status,
    Search,
    Event,
    Session,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerFailure {
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Cancelled,
    Expired,
    Invalid,
    Capacity,
    RequestCapacity,
    WorkCapacity,
    Timeout,
    Interrupted,
    Body,
    BodyTooLarge,
    Method,
    Unavailable,
    Index,
    Io,
    Catalog,
    Json,
    Core,
    Identity,
    Archive,
    Other,
}

impl From<&Error> for ServerFailure {
    fn from(error: &Error) -> Self {
        match error {
            Error::Unauthorized => Self::Unauthorized,
            Error::Forbidden => Self::Forbidden,
            Error::NotFound => Self::NotFound,
            Error::Conflict => Self::Conflict,
            Error::OperationCancelled => Self::Cancelled,
            Error::Expired => Self::Expired,
            Error::Invalid(_) => Self::Invalid,
            Error::Capacity => Self::Capacity,
            Error::Unavailable => Self::Unavailable,
            Error::Index(_) => Self::Index,
            Error::Io(_) => Self::Io,
            Error::Sql(_) => Self::Catalog,
            Error::Json(_) => Self::Json,
            Error::Core(_) => Self::Core,
            Error::Identity(_) => Self::Identity,
            Error::Archive(_) => Self::Archive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStage {
    Configuration,
    AuthorityOpen,
    Runtime,
    Bind,
    ReadyCallback,
    Serve,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerLifecycle {
    Ready,
    Liveness,
    Failed,
    Stopped,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerResponseClass {
    Unavailable,
    Success,
    Redirect,
    ClientError,
    ServerError,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerBodyOutcome {
    /// HEAD has no response body; generated payload is intentionally suppressed.
    Suppressed,
    Complete,
    Failed,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerExecutionFacts {
    pub duration: Duration,
    pub failure: Option<ServerFailure>,
}

/// Counts are capped at 1,001, meaning at least that many, never an exact total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerBacklog {
    pub pending_operations_capped: u64,
    pub staged_uploads_capped: u64,
    pub collections_capped: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerReadFacts {
    pub returned: u64,
    /// Search JSON bytes; event/session retained Core bytes. HTTP response_bytes
    /// independently measures every actual transport data frame.
    pub bytes: Option<u64>,
    pub limit: Option<u64>,
    pub continuation_requested: bool,
    pub has_more: Option<bool>,
    pub complete: Option<bool>,
    pub exhaustive: Option<bool>,
    pub response_limited: Option<bool>,
    pub snippets_truncated: Option<u64>,
    pub coverage_lag: Option<u64>,
    pub query_duration: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerPublicationKind {
    Published,
    Withdrawn,
    Removed,
    Cancelled,
    AlreadyAccepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPublicationFacts {
    pub kind: ServerPublicationKind,
    pub replay: bool,
    pub bytes: Option<u64>,
    pub records: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerIndexFacts {
    pub processed_operations: Option<u64>,
    /// Records successfully passed to the writer, even if activation later fails.
    pub records: Option<u64>,
    /// Verified input payload bytes; unavailable after a partial payload failure.
    pub bytes: Option<u64>,
    pub coverage_lag: Option<u64>,
    pub reads_available: Option<bool>,
    pub activated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerObservation {
    Lifecycle {
        kind: ServerLifecycle,
        stage: ServerStage,
        duration: Duration,
        failure: Option<ServerFailure>,
        backlog: Option<ServerBacklog>,
    },
    /// One terminal per admitted HTTP middleware call, including interruption
    /// before a response exists (response class Unavailable). Complete means every body
    /// frame was yielded to the transport, never proof of peer consumption.
    Request {
        operation: ServerOperation,
        duration: Duration,
        failure: Option<ServerFailure>,
        body_handed_off: Option<bool>,
        response_class: ServerResponseClass,
        body_outcome: ServerBodyOutcome,
        response_bytes: u64,
        execution: Option<ServerExecutionFacts>,
        read: Option<ServerReadFacts>,
    },
    Execution {
        operation: ServerOperation,
        duration: Duration,
        failure: Option<ServerFailure>,
    },
    Read {
        operation: ServerOperation,
        facts: ServerReadFacts,
    },
    Upload {
        operation: ServerOperation,
        bytes: u64,
        replay: bool,
    },
    Publication(ServerPublicationFacts),
    /// Indexer health transitions; failure None means recovered, not a new run.
    IndexerHealth {
        failure: Option<ServerFailure>,
    },
    Index {
        duration: Duration,
        failure: Option<ServerFailure>,
        facts: ServerIndexFacts,
    },
}

impl HistoryServer {
    pub(crate) fn observe(&self, fact: ServerObservation) {
        if let Some(observer) = &self.observer {
            observer(fact);
        }
    }

    pub(crate) fn lifecycle(
        &self,
        kind: ServerLifecycle,
        stage: ServerStage,
        duration: Duration,
        failure: Option<ServerFailure>,
    ) {
        if self.observer.is_some() {
            self.observe(ServerObservation::Lifecycle {
                kind,
                stage,
                duration,
                failure,
                backlog: if matches!(kind, ServerLifecycle::Ready | ServerLifecycle::Liveness) {
                    self.observed_backlog()
                } else {
                    None
                },
            });
        }
    }

    fn observed_backlog(&self) -> Option<ServerBacklog> {
        // Optional measurements never wait for product authority or inspect
        // retained files. Each covering query visits at most 1,001 rows.
        let connection = self.authority.try_lock().ok()?;
        if self
            .authority_unavailable
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        let count = |sql| {
            connection
                .query_row(sql, [], |row| row.get::<_, u64>(0))
                .ok()
        };
        Some(ServerBacklog {
            pending_operations_capped: count(
                "SELECT count(*) FROM (SELECT 1 FROM pending LIMIT 1001)",
            )?,
            staged_uploads_capped: count(
                "SELECT count(*) FROM (SELECT 1 FROM uploads LIMIT 1001)",
            )?,
            collections_capped: count(
                "SELECT count(*) FROM (SELECT 1 FROM collections LIMIT 1001)",
            )?,
        })
    }
}

use thiserror::Error;

use crate::graph::segment_graph::SegmentGraphError;
use crate::materializer::SegmentMaterializerError;
use crate::protocol::{BlameDiagnosticDetails, BlameDiagnosticReason, ErrorClass, ProtocolError};
use crate::query::{AmbiguityCandidates, QueryError, QueryOperation};

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("invalid attribution input: {0}")]
    InvalidInput(String),
    #[error("work graph requires materialization")]
    MaterializationRequired,
    #[error("query cursor is invalid")]
    InvalidCursor,
    #[error("query cursor refers to an older graph state")]
    StaleCursor,
    #[error("one query record exceeds the protocol frame bound")]
    QueryRecordTooLarge,
    #[cfg(test)]
    #[error("a writable graph backend is required")]
    WritableBackendRequired,
    #[error("authorized Git is unavailable")]
    GitAuthorityUnavailable,
    #[error("attribution index materialization is already active")]
    MaterializerBusy,
    #[error("attribution index materialization exceeded a hard bound")]
    MaterializerBounds,
    #[error("attribution index storage is unavailable")]
    SegmentUnavailable,
    #[error("attribution index requires rebuilding")]
    RebuildRequired,
    #[error("attribution query failed")]
    Query(#[from] QueryError),
}

impl From<SegmentMaterializerError> for AdapterError {
    fn from(error: SegmentMaterializerError) -> Self {
        match error {
            SegmentMaterializerError::Busy => Self::MaterializerBusy,
            SegmentMaterializerError::Bounds | SegmentMaterializerError::BoundDetail(_) => {
                Self::MaterializerBounds
            }
            SegmentMaterializerError::RebuildRequired => Self::RebuildRequired,
            SegmentMaterializerError::Graph(SegmentGraphError::Unavailable) => {
                Self::MaterializationRequired
            }
            _ => Self::SegmentUnavailable,
        }
    }
}

impl From<SegmentGraphError> for AdapterError {
    fn from(error: SegmentGraphError) -> Self {
        match error {
            SegmentGraphError::InvalidInput(message) => Self::InvalidInput(message),
            SegmentGraphError::MaterializationRequired | SegmentGraphError::Unavailable => {
                Self::MaterializationRequired
            }
            SegmentGraphError::Identity(_) => Self::RebuildRequired,
            SegmentGraphError::InvalidCursor => Self::InvalidCursor,
            SegmentGraphError::StaleCursor => Self::StaleCursor,
            SegmentGraphError::QueryRecordTooLarge => Self::QueryRecordTooLarge,
            SegmentGraphError::GitAuthorityUnavailable => Self::GitAuthorityUnavailable,
            SegmentGraphError::Query(error) => Self::Query(error),
            _ => Self::SegmentUnavailable,
        }
    }
}

pub(crate) fn protocol_error(error: &AdapterError) -> ProtocolError {
    match error {
        AdapterError::InvalidInput(_) => {
            ProtocolError::new(ErrorClass::InvalidRequest, "invalid attribution request")
        }
        AdapterError::QueryRecordTooLarge => ProtocolError::new(
            ErrorClass::Bounds,
            "attribution operation exceeded a hard bound",
        ),
        AdapterError::MaterializerBounds => ProtocolError::new(
            ErrorClass::Bounds,
            "Core materialization exceeded a hard bound",
        ),
        AdapterError::MaterializationRequired => ProtocolError::new(
            ErrorClass::NotMaterialized,
            "work graph requires materialization before querying",
        ),
        AdapterError::RebuildRequired => ProtocolError::new(
            ErrorClass::RebuildRequired,
            "attribution index requires rebuilding",
        ),
        AdapterError::InvalidCursor => {
            ProtocolError::new(ErrorClass::InvalidRequest, "query cursor is invalid")
        }
        AdapterError::StaleCursor => {
            ProtocolError::new(ErrorClass::StaleFact, "query cursor is stale")
        }
        AdapterError::MaterializerBusy => {
            let mut error = ProtocolError::new(
                ErrorClass::NotMaterialized,
                "graph rebuild is already active",
            );
            error.retryable = true;
            error
        }
        #[cfg(test)]
        AdapterError::WritableBackendRequired => ProtocolError::new(
            ErrorClass::Sequence,
            "Core materialization requires a writable graph backend",
        ),
        AdapterError::GitAuthorityUnavailable | AdapterError::Query(QueryError::GitUnavailable) => {
            blame_protocol_error(
                ErrorClass::MissingSource,
                "requested attribution operation is unavailable",
                BlameDiagnosticReason::GitUnavailable,
            )
        }
        AdapterError::SegmentUnavailable => {
            ProtocolError::new(ErrorClass::Corrupt, "attribution index is unavailable")
        }
        AdapterError::Query(QueryError::InvalidBounds) => {
            ProtocolError::new(ErrorClass::Bounds, "query bounds are invalid")
        }
        AdapterError::Query(QueryError::AttributionLimitExceeded(_)) => ProtocolError::new(
            ErrorClass::Bounds,
            "one blame match exceeded the attribution bound",
        ),
        AdapterError::Query(QueryError::InvalidRequest(_)) => {
            ProtocolError::new(ErrorClass::InvalidRequest, "query request is invalid")
        }
        AdapterError::Query(QueryError::TargetNotFound(_)) => blame_protocol_error(
            ErrorClass::ResourceNotFound,
            "blame target was not found",
            BlameDiagnosticReason::TargetNotIndexed,
        ),
        AdapterError::Query(QueryError::RepositorySelectorNotFound) => blame_protocol_error(
            ErrorClass::ResourceNotFound,
            "repository selector was not found",
            BlameDiagnosticReason::RepositorySelectorNotIndexed,
        ),
        AdapterError::Query(QueryError::RepositoryNotBound) => blame_protocol_error(
            ErrorClass::MissingRepository,
            "blame target is not bound to an authorized repository",
            BlameDiagnosticReason::RepositoryNotBound,
        ),
        AdapterError::Query(QueryError::RepositoryUnavailable) => blame_protocol_error(
            ErrorClass::MissingRepository,
            "authorized repository checkout is unavailable",
            BlameDiagnosticReason::CheckoutUnavailable,
        ),
        AdapterError::Query(QueryError::OperationUnavailable(operation)) => blame_protocol_error(
            ErrorClass::OperationUnavailable,
            "requested attribution operation is unavailable",
            operation_unavailable_reason(*operation),
        ),
        AdapterError::Query(QueryError::AmbiguousTarget(details)) => ambiguity_protocol_error(
            "multiple commits match the requested selector",
            BlameDiagnosticReason::TargetAmbiguous,
            details,
        ),
        AdapterError::Query(QueryError::AmbiguousRepositoryCandidates(details)) => {
            ambiguity_protocol_error(
                "multiple authorized repositories match the requested resource",
                BlameDiagnosticReason::RepositoryAmbiguous,
                details,
            )
        }
        AdapterError::Query(QueryError::AmbiguousCommitRewrite(details)) => {
            ambiguity_protocol_error(
                "multiple surviving commits match the requested rewritten commit",
                BlameDiagnosticReason::CommitRewriteAmbiguous,
                details,
            )
        }
        AdapterError::Query(QueryError::LineOutOfRange) => ProtocolError::new(
            ErrorClass::LineOutOfRange,
            "requested line range is outside HEAD",
        ),
        AdapterError::Query(QueryError::StaleSnapshot) => {
            let mut error = ProtocolError::new(
                ErrorClass::StaleSnapshot,
                "repository HEAD changed during file blame",
            );
            error.retryable = true;
            error
        }
        AdapterError::Query(QueryError::UncitedFact(_) | QueryError::InvalidCitation) => {
            ProtocolError::new(ErrorClass::Corrupt, "attribution index evidence is invalid")
        }
        AdapterError::Query(_) => {
            ProtocolError::new(ErrorClass::Internal, "attribution operation failed")
        }
    }
}

fn blame_protocol_error(
    class: ErrorClass,
    message: &'static str,
    reason: BlameDiagnosticReason,
) -> ProtocolError {
    ProtocolError::new(class, message).with_blame_details(BlameDiagnosticDetails {
        reason,
        candidates: Vec::new(),
        candidates_truncated: false,
    })
}

fn ambiguity_protocol_error(
    message: &'static str,
    reason: BlameDiagnosticReason,
    details: &AmbiguityCandidates,
) -> ProtocolError {
    ProtocolError::new(ErrorClass::Ambiguous, message).with_blame_details(BlameDiagnosticDetails {
        reason,
        candidates: details.candidates.clone(),
        candidates_truncated: details.candidates_truncated,
    })
}

const fn operation_unavailable_reason(operation: QueryOperation) -> BlameDiagnosticReason {
    match operation {
        QueryOperation::FileBlame => BlameDiagnosticReason::FileBlameNotCovered,
        QueryOperation::CommitBlame => BlameDiagnosticReason::CommitBlameNotCovered,
        QueryOperation::PullRequestBlame => BlameDiagnosticReason::PullRequestBlameNotCovered,
    }
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;

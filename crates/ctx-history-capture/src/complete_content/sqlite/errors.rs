//! Stable translation from provider, I/O, and SQLite failures to content errors.

use super::*;

pub(super) fn error(
    request: &CompleteMessageRequest,
    kind: CompleteContentErrorKind,
) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}

pub(super) fn map_capture_error(
    request: &CompleteMessageRequest,
    cause: CaptureError,
) -> CompleteContentError {
    match cause {
        CaptureError::Io(cause) => map_io_error(request, cause),
        CaptureError::SourceChangedDuringCapture => {
            error(request, CompleteContentErrorKind::SourceChanged)
        }
        CaptureError::InvalidProviderTranscriptPath { .. } => {
            error(request, CompleteContentErrorKind::SourceUnreadable)
        }
        CaptureError::Sqlite(cause) => map_sqlite_error(request, cause),
        _ => error(request, CompleteContentErrorKind::ContentVerificationFailed),
    }
}

pub(super) fn map_bounded_sqlite_error(
    request: &CompleteMessageRequest,
    cause: CompleteContentSqliteBoundError,
) -> CompleteContentError {
    map_bounded_sqlite_error_for_event(request.event_id, cause)
}

pub(crate) fn map_bounded_sqlite_error_for_event(
    event_id: uuid::Uuid,
    cause: CompleteContentSqliteBoundError,
) -> CompleteContentError {
    match cause {
        CompleteContentSqliteBoundError::Capture(CaptureError::Io(cause)) => {
            if cause.kind() == std::io::ErrorKind::NotFound {
                CompleteContentError::new(CompleteContentErrorKind::SourceMissing, event_id)
            } else {
                CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
            }
        }
        CompleteContentSqliteBoundError::Capture(CaptureError::SourceChangedDuringCapture) => {
            CompleteContentError::new(CompleteContentErrorKind::SourceChanged, event_id)
        }
        CompleteContentSqliteBoundError::Capture(CaptureError::InvalidProviderTranscriptPath {
            ..
        }) => CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id),
        CompleteContentSqliteBoundError::Capture(CaptureError::Sqlite(cause)) => {
            if let rusqlite::Error::SqliteFailure(failure, _) = &cause {
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::TooBig | rusqlite::ErrorCode::OperationInterrupted
                ) {
                    return CompleteContentError::new(
                        CompleteContentErrorKind::ContentTooLarge,
                        event_id,
                    );
                }
            }
            CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
        }
        CompleteContentSqliteBoundError::Capture(_) => CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            event_id,
        ),
        CompleteContentSqliteBoundError::ContentTooLarge => {
            CompleteContentError::new(CompleteContentErrorKind::ContentTooLarge, event_id)
        }
    }
}

pub(super) fn map_io_error(
    request: &CompleteMessageRequest,
    cause: std::io::Error,
) -> CompleteContentError {
    if cause.kind() == std::io::ErrorKind::NotFound {
        error(request, CompleteContentErrorKind::SourceMissing)
    } else {
        error(request, CompleteContentErrorKind::SourceUnreadable)
    }
}

pub(super) fn map_sqlite_error(
    request: &CompleteMessageRequest,
    cause: rusqlite::Error,
) -> CompleteContentError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &cause {
        return match failure.code {
            rusqlite::ErrorCode::TooBig | rusqlite::ErrorCode::OperationInterrupted => {
                error(request, CompleteContentErrorKind::ContentTooLarge)
            }
            rusqlite::ErrorCode::TypeMismatch | rusqlite::ErrorCode::SchemaChanged => {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            }
            _ => error(request, CompleteContentErrorKind::SourceUnreadable),
        };
    }
    if matches!(
        cause,
        rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::Utf8Error(..)
            | rusqlite::Error::InvalidColumnType(..)
    ) {
        return error(request, CompleteContentErrorKind::ContentVerificationFailed);
    }
    error(request, CompleteContentErrorKind::SourceUnreadable)
}

use std::fmt;

use anyhow::Error;

#[derive(Debug)]
pub(super) struct DaemonQueryRequestMayHaveBeenSubmitted;

impl fmt::Display for DaemonQueryRequestMayHaveBeenSubmitted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daemon query request may have been submitted")
    }
}

impl std::error::Error for DaemonQueryRequestMayHaveBeenSubmitted {}

pub(super) fn mark_request_may_have_been_submitted(error: Error) -> Error {
    error.context(DaemonQueryRequestMayHaveBeenSubmitted)
}

pub(super) fn request_may_have_been_submitted(error: &Error) -> bool {
    error
        .downcast_ref::<DaemonQueryRequestMayHaveBeenSubmitted>()
        .is_some()
}

#[cfg(windows)]
pub(super) fn mark_windows_pending_submission(pending_submission: Option<&mut bool>) {
    if let Some(pending_submission) = pending_submission {
        *pending_submission = true;
    }
}

#[cfg(test)]
#[path = "submission_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "windows_submission_tests.rs"]
mod windows_tests;

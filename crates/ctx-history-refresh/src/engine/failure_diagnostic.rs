use super::*;

/// Content-free diagnostics; never authority for outcome or retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RefreshFailureDiagnostic {
    pub(super) stage: RefreshFailureStage,
    pub(super) kind: RefreshFailureKind,
}

impl RefreshFailureDiagnostic {
    pub(super) fn new(stage: RefreshFailureStage, error: Option<&anyhow::Error>) -> Self {
        let kind = error.map_or(RefreshFailureKind::Unknown, |error| {
            if error.chain().any(|cause| cause.is::<std::io::Error>()) {
                RefreshFailureKind::Io
            } else if error.chain().any(|cause| {
                cause.is::<IndexError>()
                    || matches!(
                        cause.downcast_ref::<SourceBackedCoordinatorError>(),
                        Some(SourceBackedCoordinatorError::Index(_))
                    )
            }) {
                RefreshFailureKind::Index
            } else if error.chain().any(|cause| {
                cause.is::<SourceBackedRouteError>()
                    || cause.is::<SourceBackedAdmissionRouteFailures>()
                    || cause.is::<SourceBackedCoordinatorError>()
                    || cause.is::<ZeroSourcePublicationBlocked>()
            }) {
                RefreshFailureKind::Provider
            } else {
                RefreshFailureKind::Unknown
            }
        });
        Self { stage, kind }
    }

    // Missing, partial, and future diagnostic pairs must not block recovery.
    pub(super) fn from_job(job: &Value) -> Option<Self> {
        Some(Self {
            stage: job.get("refresh_failure_stage")?.as_str()?.parse().ok()?,
            kind: job.get("refresh_failure_kind")?.as_str()?.parse().ok()?,
        })
    }
}

#[cfg(test)]
mod tests;

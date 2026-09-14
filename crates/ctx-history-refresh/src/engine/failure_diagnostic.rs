use super::*;

/// Content-free diagnostics; never authority for outcome or retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RefreshFailureDiagnostic {
    pub(super) stage: RefreshFailureStage,
    pub(super) kind: RefreshFailureKind,
    pub(super) coverage_reason: Option<ZeroSourcePublicationBlockReason>,
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
        let coverage_reason = error.and_then(|error| {
            error.chain().find_map(|cause| {
                cause
                    .downcast_ref::<ZeroSourcePublicationBlocked>()
                    .and_then(|error| error.reason())
            })
        });
        Self {
            stage,
            kind,
            coverage_reason,
        }
    }

    // Missing, partial, and future diagnostic pairs must not block recovery.
    pub(super) fn from_job(job: &Value) -> Option<Self> {
        Some(Self {
            stage: job.get("refresh_failure_stage")?.as_str()?.parse().ok()?,
            kind: job.get("refresh_failure_kind")?.as_str()?.parse().ok()?,
            coverage_reason: job
                .get("refresh_coverage_reason")
                .and_then(Value::as_str)
                .and_then(ZeroSourcePublicationBlockReason::parse),
        })
    }
}

#[cfg(test)]
mod tests;

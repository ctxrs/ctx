use anyhow::Result;

use ctx_history_capture::CaptureError;

use super::ImportMaintenancePendingReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainFixedPointBlocker {
    RetryableExternal,
    Maintenance(ImportMaintenancePendingReason),
}

impl DrainFixedPointBlocker {
    pub(crate) fn diagnostic(self) -> String {
        match self {
            Self::RetryableExternal => {
                "import work is waiting for a retryable source condition".to_owned()
            }
            Self::Maintenance(reason) => reason.diagnostic(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainFixedPointAction {
    Complete,
    Reinventory,
    RetryableBlocked(DrainFixedPointBlocker),
}

pub(crate) fn drain_fixed_point_action(
    has_pending_work: bool,
    made_durable_progress: bool,
    retryable_blocker: Option<DrainFixedPointBlocker>,
) -> Result<DrainFixedPointAction> {
    if let Some(blocker) = retryable_blocker {
        return Ok(DrainFixedPointAction::RetryableBlocked(blocker));
    }
    if made_durable_progress {
        return Ok(DrainFixedPointAction::Reinventory);
    }
    if !has_pending_work {
        return Ok(DrainFixedPointAction::Complete);
    }
    Err(anyhow::Error::new(CaptureError::SystemInvariant(
        "drain import work remained pending without durable progress",
    )))
}

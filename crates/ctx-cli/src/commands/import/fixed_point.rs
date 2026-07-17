use anyhow::Result;

use ctx_history_capture::CaptureError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainFixedPointAction {
    Complete,
    Reinventory,
    RetryableBlocked,
}

pub(crate) fn drain_fixed_point_action(
    has_pending_work: bool,
    made_durable_progress: bool,
    retryable_blocker: bool,
) -> Result<DrainFixedPointAction> {
    if made_durable_progress {
        return Ok(DrainFixedPointAction::Reinventory);
    }
    if !has_pending_work {
        return Ok(DrainFixedPointAction::Complete);
    }
    if retryable_blocker {
        return Ok(DrainFixedPointAction::RetryableBlocked);
    }
    Err(anyhow::Error::new(CaptureError::SystemInvariant(
        "drain import work remained pending without durable progress",
    )))
}

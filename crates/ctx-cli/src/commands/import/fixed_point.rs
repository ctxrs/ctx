use anyhow::Result;

use ctx_history_capture::CaptureError;
use ctx_history_store::Store;

use super::{repair_import_maintenance, ImportMaintenancePendingReason, ImportMaintenanceStep};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrainMaintenanceOutcome {
    pub(crate) made_durable_progress: bool,
    pub(crate) pending_reason: Option<ImportMaintenancePendingReason>,
}

impl DrainMaintenanceOutcome {
    pub(crate) fn is_complete(self) -> bool {
        self.pending_reason.is_none()
    }
}

pub(crate) fn drain_import_maintenance(store: &Store) -> Result<DrainMaintenanceOutcome> {
    let mut made_durable_progress = false;
    loop {
        match repair_import_maintenance(store)? {
            ImportMaintenanceStep::Complete => {
                return Ok(DrainMaintenanceOutcome {
                    made_durable_progress,
                    pending_reason: None,
                });
            }
            ImportMaintenanceStep::Progress => made_durable_progress = true,
            ImportMaintenanceStep::Pending(reason) => {
                return Ok(DrainMaintenanceOutcome {
                    made_durable_progress,
                    pending_reason: Some(reason),
                });
            }
        }
    }
}

pub(crate) fn drain_fixed_point_action(
    has_pending_work: bool,
    made_source_plan_progress: bool,
    retryable_blocker: Option<DrainFixedPointBlocker>,
) -> Result<DrainFixedPointAction> {
    if let Some(DrainFixedPointBlocker::Maintenance(reason)) = retryable_blocker {
        return Ok(DrainFixedPointAction::RetryableBlocked(
            DrainFixedPointBlocker::Maintenance(reason),
        ));
    }
    if made_source_plan_progress {
        return Ok(DrainFixedPointAction::Reinventory);
    }
    if let Some(blocker) = retryable_blocker {
        return Ok(DrainFixedPointAction::RetryableBlocked(blocker));
    }
    if !has_pending_work {
        return Ok(DrainFixedPointAction::Complete);
    }
    Err(anyhow::Error::new(CaptureError::SystemInvariant(
        "drain import work remained pending without durable progress",
    )))
}

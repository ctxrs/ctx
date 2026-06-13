use std::sync::Mutex;

use super::policy::now_ms;

mod heartbeat;
mod incidents;
mod model;
mod snapshot;

use model::DesktopWebviewRecoveryState;
pub(super) use model::{HeartbeatTimeoutEvaluation, PreparedRecoveryIncident};

#[derive(Debug, Default)]
pub(crate) struct DesktopWebviewRecoveryController {
    inner: Mutex<DesktopWebviewRecoveryState>,
}

#[cfg(test)]
mod tests;

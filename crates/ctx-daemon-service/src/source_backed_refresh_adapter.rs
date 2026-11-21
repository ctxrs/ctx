pub(super) mod journal;
pub(super) mod runtime;
pub(super) mod wire;

use std::sync::Arc;

#[cfg(not(test))]
use ctx_history_refresh::RefreshEngine;

#[cfg(test)]
use super::source_backed_refresh_coordinator::CoreRefreshEngine;
use crate::daemon_wakeup::DaemonWakeup;

#[cfg(not(test))]
pub(super) fn refresh_engine_with_wakeup(
    config: &'static dyn crate::DaemonConfigPort,
    wakeup: Arc<DaemonWakeup>,
) -> RefreshEngine {
    RefreshEngine::new(
        Arc::new(journal::DaemonRefreshJournal::default()),
        Arc::new(runtime::DaemonRefreshRuntime::with_wakeup(config, wakeup)),
    )
}

#[cfg(test)]
pub(super) fn refresh_engine(config: &'static dyn crate::DaemonConfigPort) -> CoreRefreshEngine {
    CoreRefreshEngine::with_config(config)
}

#[cfg(test)]
pub(super) fn refresh_engine_with_wakeup(
    config: &'static dyn crate::DaemonConfigPort,
    wakeup: Arc<DaemonWakeup>,
) -> CoreRefreshEngine {
    CoreRefreshEngine::with_config_and_wakeup(config, wakeup)
}

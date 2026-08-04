pub(super) mod journal;
pub(super) mod runtime;
pub(super) mod wire;

#[cfg(not(test))]
use std::sync::Arc;

#[cfg(not(test))]
use ctx_history_refresh::RefreshEngine;

#[cfg(test)]
use super::source_backed_refresh_coordinator::CoreRefreshEngine;

#[cfg(not(test))]
pub(super) fn refresh_engine() -> RefreshEngine {
    RefreshEngine::new(
        Arc::new(journal::DaemonRefreshJournal),
        Arc::new(runtime::DaemonRefreshRuntime),
    )
}

#[cfg(test)]
pub(super) fn refresh_engine() -> CoreRefreshEngine {
    CoreRefreshEngine::new()
}

use super::*;

pub(super) struct SourceBackedRefreshRequestPolicy {
    pub(super) intent: RefreshIntent,
    pub(super) trigger: RefreshRequestTrigger,
    pub(super) allow_daemon_autostart: bool,
}

impl SourceBackedRefreshRequestPolicy {
    pub(super) fn refresh(trigger: RefreshRequestTrigger) -> Self {
        Self {
            intent: RefreshIntent::AutomaticMaintenance,
            trigger,
            allow_daemon_autostart: true,
        }
    }

    pub(super) fn import(selection: RefreshSelection, allow_daemon_autostart: bool) -> Self {
        Self {
            intent: RefreshIntent::SelectedImport(selection),
            trigger: RefreshRequestTrigger::Import,
            allow_daemon_autostart,
        }
    }
}

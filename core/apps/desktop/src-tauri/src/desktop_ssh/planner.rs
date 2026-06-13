#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteBootstrapPlan {
    ConnectToRunningDaemon,
    StartManagedDaemon,
    InstallManagedDaemonThenStart,
    RefuseBecauseStartRemoteDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RemoteBootstrapPlannerInput {
    pub(super) start_remote: bool,
    pub(super) no_start_remote: bool,
    pub(super) existing_daemon_reachable: bool,
    pub(super) managed_binary_present: bool,
}

pub(super) fn plan_remote_bootstrap(input: RemoteBootstrapPlannerInput) -> RemoteBootstrapPlan {
    if input.existing_daemon_reachable {
        return RemoteBootstrapPlan::ConnectToRunningDaemon;
    }
    if !input.start_remote || input.no_start_remote {
        return RemoteBootstrapPlan::RefuseBecauseStartRemoteDisabled;
    }
    if input.managed_binary_present {
        return RemoteBootstrapPlan::StartManagedDaemon;
    }
    RemoteBootstrapPlan::InstallManagedDaemonThenStart
}

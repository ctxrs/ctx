use anyhow::anyhow;
use ctx_upgrade_engine::{DaemonUpgradeLease, PreparedAutomaticUpgrade};

pub(super) fn abort_prepared_automatic_upgrade<L: DaemonUpgradeLease>(
    prepared: Option<PreparedAutomaticUpgrade>,
    handoff: Option<L>,
    error: anyhow::Error,
) -> anyhow::Error {
    let install_path = prepared
        .as_ref()
        .map(|prepared| prepared.install_path().to_path_buf());
    let state_error = prepared.and_then(|prepared| prepared.abort(&error).err());
    let restart_error = match (handoff, install_path.as_deref()) {
        (Some(handoff), Some(install_path)) => handoff.resume_with(install_path).err(),
        (Some(_), None) => Some(anyhow!(
            "automatic upgrade handoff has no validated executable for restart"
        )),
        (None, _) => None,
    };
    let mut cleanup = Vec::new();
    if let Some(state_error) = state_error {
        cleanup.push(format!(
            "failed to terminalize automatic upgrade state: {state_error:#}"
        ));
    }
    if let Some(restart_error) = restart_error {
        cleanup.push(format!(
            "failed to restart daemon after automatic upgrade abort: {restart_error:#}"
        ));
    }
    if cleanup.is_empty() {
        error
    } else {
        error.context(cleanup.join("; "))
    }
}

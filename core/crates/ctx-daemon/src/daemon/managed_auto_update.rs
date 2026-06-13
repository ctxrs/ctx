use std::sync::Arc;

use anyhow::Result;

use super::{daemon_turn_activity_summary, DaemonState};

struct ManagedDaemonAutoUpdateAppHooks {
    state: Arc<DaemonState>,
}

#[async_trait::async_trait]
impl ctx_update_service::ManagedDaemonAutoUpdateHooks for ManagedDaemonAutoUpdateAppHooks {
    async fn acquire_update_drain(&self, reason: &str, owner: &str) -> bool {
        self.state
            .core
            .update_drain
            .acquire(reason.to_string(), owner.to_string())
            .await
            .is_some()
    }

    async fn release_update_drain(&self) {
        let _ = self.state.core.update_drain.release().await;
    }

    async fn daemon_is_idle(&self) -> Result<bool> {
        let activity = daemon_turn_activity_summary(&self.state).await?;
        Ok(activity.queued_turn_count == 0 && activity.running_turn_count == 0)
    }
}

pub(super) fn spawn_managed_daemon_auto_update(state: Arc<DaemonState>, bind: Vec<String>) {
    if !ctx_update_service::managed_daemon_auto_update_configured_from_env() {
        return;
    }
    let current_version = match ctx_update_service::current_build_identity(env!(
        "CARGO_PKG_VERSION"
    )) {
        Ok(identity) => identity.exact_version.clone(),
        Err(err) => {
            tracing::warn!(err = %err, "managed daemon auto-update disabled; build identity unavailable");
            return;
        }
    };
    let config = ctx_update_service::ManagedDaemonAutoUpdateConfig {
        data_root: state.core.data_root.clone(),
        bind,
        current_version,
    };
    let hooks: Arc<dyn ctx_update_service::ManagedDaemonAutoUpdateHooks> =
        Arc::new(ManagedDaemonAutoUpdateAppHooks { state });
    ctx_update_service::spawn_managed_daemon_auto_update(config, hooks);
}

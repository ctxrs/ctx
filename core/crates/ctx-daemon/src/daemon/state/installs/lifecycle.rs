use super::*;

mod finish;

impl DaemonState {
    pub async fn find_running_install(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> Option<InstallId> {
        let outcome = self
            .providers
            .find_running_install(provider_id, target)
            .await;
        self.emit_provider_install_ops_events(outcome.ops_events);
        outcome.install_id
    }

    pub async fn start_install(
        &self,
        provider_id: String,
        target: Option<InstallTarget>,
    ) -> (InstallId, bool) {
        let outcome = self.providers.start_install(provider_id, target).await;
        self.emit_provider_install_ops_events(outcome.ops_events);
        (outcome.install_id, outcome.started_new)
    }

    pub async fn cancel_install(
        &self,
        install_id: InstallId,
    ) -> Option<ctx_provider_install::install_state::InstallInfo> {
        let outcome = self.providers.cancel_install(install_id).await?;
        self.emit_provider_install_ops_events(outcome.ops_events);
        Some(outcome.info)
    }
}

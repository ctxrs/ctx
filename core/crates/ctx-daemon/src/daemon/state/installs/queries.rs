use super::*;

impl DaemonState {
    pub async fn set_install_progress_pct_override(&self, install_id: InstallId, pct: Option<u8>) {
        self.providers
            .set_install_progress_pct_override(install_id, pct)
            .await;
    }

    pub async fn get_install_sender(
        &self,
        install_id: InstallId,
    ) -> Option<broadcast::Sender<InstallProgressEvent>> {
        self.providers.get_install_sender(install_id).await
    }

    pub async fn get_install_info(
        &self,
        install_id: InstallId,
    ) -> Option<ctx_provider_install::install_state::InstallInfo> {
        let outcome = self.providers.get_install_info(install_id).await;
        self.emit_provider_install_ops_events(outcome.ops_events);
        outcome.info
    }

    pub async fn get_install_polling_info(
        &self,
        install_id: InstallId,
    ) -> Option<ctx_provider_install::install_state::InstallInfo> {
        let outcome = self.providers.get_install_polling_info(install_id).await;
        self.emit_provider_install_ops_events(outcome.ops_events);
        outcome.info
    }

    pub async fn get_install_events(
        &self,
        install_id: InstallId,
    ) -> Option<Vec<InstallProgressEvent>> {
        let outcome = self.providers.get_install_events(install_id).await;
        self.emit_provider_install_ops_events(outcome.ops_events);
        outcome.events
    }

    pub async fn is_install_cancelled(&self, install_id: InstallId) -> bool {
        self.providers.is_install_cancelled(install_id).await
    }
}

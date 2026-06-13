use super::*;

impl DaemonState {
    pub async fn register_install_progress_mirror(
        &self,
        source_install_id: InstallId,
        mirror_install_id: InstallId,
    ) -> bool {
        self.providers
            .register_install_progress_mirror(source_install_id, mirror_install_id)
            .await
    }

    pub async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent) {
        self.providers.emit_install_event(install_id, event).await;
    }
}

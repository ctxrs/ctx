use super::super::*;

impl DaemonState {
    pub async fn finish_install(
        &self,
        install_id: InstallId,
        ok: bool,
        error: Option<String>,
        error_code: Option<InstallErrorCode>,
    ) {
        let Some(event) = self
            .providers
            .finish_install(install_id, ok, error, error_code)
            .await
        else {
            return;
        };
        self.emit_provider_install_ops_event(event);
    }
}

use super::*;
use ctx_daemon::daemon::TitleGenerationLocalHandle;
use ctx_managed_installs::title_generation_local::{
    TitleGenerationLocalModelStatus, TitleGenerationLocalRuntimeStatus,
};

#[derive(Debug, Serialize)]
pub(in crate::api) struct TitleGenerationLocalStatusResponse {
    pub ready: bool,
    pub runtime: TitleGenerationLocalRuntimeStatus,
    pub model: TitleGenerationLocalModelStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_id: Option<InstallId>,
    pub install_running: bool,
}

#[derive(Debug, Serialize)]
pub(in crate::api) struct TitleGenerationLocalInstallResponse {
    pub install_id: InstallId,
}

pub(in crate::api) async fn get_title_generation_local_status(
    State(state): State<TitleGenerationLocalHandle>,
) -> Result<Json<TitleGenerationLocalStatusResponse>, StatusCode> {
    let status = state
        .title_generation_local_status()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(TitleGenerationLocalStatusResponse {
        ready: status.ready,
        runtime: status.runtime,
        model: status.model,
        install_id: status.install_id,
        install_running: status.install_running,
    }))
}

pub(in crate::api) async fn install_title_generation_local(
    State(state): State<TitleGenerationLocalHandle>,
) -> Result<Json<TitleGenerationLocalInstallResponse>, StatusCode> {
    let install_id = state.start_title_generation_local_install().await;
    Ok(Json(TitleGenerationLocalInstallResponse { install_id }))
}

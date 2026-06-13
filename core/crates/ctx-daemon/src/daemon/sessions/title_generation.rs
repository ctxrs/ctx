use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::daemon::{DaemonState, SessionTitleModelModeHandle};
use async_trait::async_trait;
use ctx_core::models::Session;
use ctx_managed_installs::title_generation_local::{
    TitleGenerationLocalModelStatus, TitleGenerationLocalRuntimeStatus,
};
use ctx_observability::logs;
use ctx_observability::ops_events::OpsEvents;
use ctx_provider_install::install_state::{
    InstallErrorCode, InstallId, InstallInfo, InstallProgressEvent,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_title_service::title_generation::{self, TitleGenerationOutcome};
use ctx_settings_model as user_settings;
use ctx_store::Store;

mod persistence;
pub use persistence::apply_session_title_update;

pub const TITLE_GENERATION_LOCAL_INSTALL_KEY: &str = "title_generation_local";

#[derive(Debug, Clone)]
pub struct TitleGenerationLocalStatusSnapshot {
    pub ready: bool,
    pub runtime: TitleGenerationLocalRuntimeStatus,
    pub model: TitleGenerationLocalModelStatus,
    pub install_id: Option<InstallId>,
    pub install_running: bool,
}

#[derive(Clone)]
pub struct TitleGenerationLocalHandle {
    data_root: PathBuf,
    install: TitleGenerationLocalInstallEffect,
}

impl TitleGenerationLocalHandle {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        install: TitleGenerationLocalInstallEffect,
    ) -> Self {
        Self { data_root, install }
    }

    pub async fn title_generation_local_status(
        &self,
    ) -> anyhow::Result<TitleGenerationLocalStatusSnapshot> {
        title_generation_local_status(&self.data_root, &self.install).await
    }

    pub async fn start_title_generation_local_install(&self) -> InstallId {
        self.install.start_install().await
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct TitleGenerationLocalInstallEffect {
    data_root: PathBuf,
    providers: Arc<ProviderRuntime>,
    ops_events: OpsEvents,
}

impl TitleGenerationLocalInstallEffect {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        providers: Arc<ProviderRuntime>,
        ops_events: OpsEvents,
    ) -> Self {
        Self {
            data_root,
            providers,
            ops_events,
        }
    }

    async fn find_running_install(&self) -> Option<InstallId> {
        let outcome = self
            .providers
            .find_running_install(TITLE_GENERATION_LOCAL_INSTALL_KEY, None)
            .await;
        let install_id = outcome.install_id;
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            outcome.ops_events,
        );
        install_id
    }

    async fn start_install(&self) -> InstallId {
        let outcome = self
            .providers
            .start_install(TITLE_GENERATION_LOCAL_INSTALL_KEY.to_string(), None)
            .await;
        let install_id = outcome.install_id;
        let started_new = outcome.started_new;
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            outcome.ops_events,
        );
        if started_new {
            let install_host = Arc::new(self.clone())
                as Arc<dyn ctx_managed_installs::title_generation::TitleGenerationLocalInstallHost>;
            tokio::spawn(async move {
                if let Err(error) =
                    ctx_managed_installs::install_title_generation_local_with_progress(
                        install_host,
                        install_id,
                    )
                    .await
                {
                    tracing::error!("local title generation install failed: {error:#}");
                }
            });
        }
        install_id
    }
}

impl ctx_managed_installs::title_generation::TitleGenerationLocalInstallHost
    for TitleGenerationLocalInstallEffect
{
    fn data_root(&self) -> &Path {
        &self.data_root
    }
}

#[async_trait]
impl ctx_managed_installs::InstallProgressHost for TitleGenerationLocalInstallEffect {
    async fn get_install_info(&self, install_id: InstallId) -> Option<InstallInfo> {
        let outcome = self.providers.get_install_info(install_id).await;
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            outcome.ops_events,
        );
        outcome.info
    }

    async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent) {
        self.providers.emit_install_event(install_id, event).await;
    }

    async fn finish_install(
        &self,
        install_id: InstallId,
        success: bool,
        error: Option<String>,
        error_code: Option<InstallErrorCode>,
    ) {
        let Some(event) = self
            .providers
            .finish_install(install_id, success, error, error_code)
            .await
        else {
            return;
        };
        crate::daemon::provider_capability_hosts::emit_provider_install_ops_events(
            &self.ops_events,
            vec![event],
        );
    }

    async fn is_install_cancelled(&self, install_id: InstallId) -> bool {
        self.providers.is_install_cancelled(install_id).await
    }
}

async fn title_generation_local_status(
    data_root: &Path,
    install: &TitleGenerationLocalInstallEffect,
) -> anyhow::Result<TitleGenerationLocalStatusSnapshot> {
    let status = ctx_managed_installs::title_generation_local::local_status(data_root).await?;
    let install_id = install.find_running_install().await;
    Ok(TitleGenerationLocalStatusSnapshot {
        ready: status.ready,
        runtime: status.runtime,
        model: status.model,
        install_id,
        install_running: install_id.is_some(),
    })
}

pub async fn configured_title_generation_settings(
    state: &DaemonState,
) -> Option<user_settings::TitleGenerationSettings> {
    configured_title_generation_settings_for_store(state.global_store()).await
}

pub async fn configured_title_generation_settings_for_store(
    global_store: &Store,
) -> Option<user_settings::TitleGenerationSettings> {
    let settings = match ctx_settings_service::load_settings(global_store).await {
        Ok(settings) => settings,
        Err(err) => {
            tracing::warn!(
                "failed to load title-generation settings: {}",
                logs::redact_sensitive(&err.to_string())
            );
            return None;
        }
    };
    settings
        .title_generation
        .as_ref()
        .filter(|cfg| title_generation::is_configured(cfg))
        .cloned()
}

pub async fn maybe_generate_session_title(
    state: Arc<DaemonState>,
    session: Session,
    prompt: String,
    force: bool,
    cfg: Option<user_settings::TitleGenerationSettings>,
) -> anyhow::Result<Option<TitleGenerationOutcome>> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Ok(None);
    }

    let current = session.title.trim();
    if !force && !current.is_empty() && current != title_generation::DEFAULT_SESSION_TITLE {
        return Ok(None);
    }

    let outcome =
        title_generation::generate_title_for_prompt(cfg.as_ref(), &prompt, &state.core.data_root)
            .await?;
    apply_session_title_update(&state, &session, outcome.clone()).await?;
    Ok(Some(outcome))
}

pub async fn maybe_generate_session_title_with_handle(
    handle: &SessionTitleModelModeHandle,
    session: Session,
    prompt: String,
    force: bool,
    cfg: Option<user_settings::TitleGenerationSettings>,
) -> anyhow::Result<Option<TitleGenerationOutcome>> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Ok(None);
    }

    let current = session.title.trim();
    if !force && !current.is_empty() && current != title_generation::DEFAULT_SESSION_TITLE {
        return Ok(None);
    }

    let outcome =
        title_generation::generate_title_for_prompt(cfg.as_ref(), &prompt, handle.data_root())
            .await?;
    persistence::apply_session_title_update_with_handle(handle, &session, outcome.clone()).await?;
    Ok(Some(outcome))
}

pub async fn schedule_session_title_generation(
    state: Arc<DaemonState>,
    session: Session,
    prompt: String,
    force: bool,
) -> bool {
    let cfg = configured_title_generation_settings(&state).await;
    if cfg.is_some() {
        tokio::spawn(async move {
            let _ = maybe_generate_session_title(state, session, prompt, force, cfg).await;
        });
        true
    } else {
        let _ = maybe_generate_session_title(state, session, prompt, force, cfg).await;
        false
    }
}

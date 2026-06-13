use ctx_observability::telemetry::TelemetryConfig;
use ctx_settings_model::{PublicSettings, Settings};
use ctx_settings_service::route_contract::SettingsRouteError;

use crate::daemon::{
    provider_guard, provider_restart, resource_governance, tool_cgroup, DaemonState, SettingsHandle,
};

pub async fn load_settings(state: &DaemonState) -> anyhow::Result<Settings> {
    ctx_settings_service::load_settings(state.global_store()).await
}

pub async fn save_settings(state: &DaemonState, settings: &Settings) -> anyhow::Result<()> {
    ctx_settings_service::save_settings(state.global_store(), settings).await
}

pub async fn public_settings_for_response(
    state: &DaemonState,
    settings: &Settings,
) -> PublicSettings {
    let mut public = ctx_settings_service::to_public(settings);
    public.resource_governance = resource_governance::build_public_settings(state, settings).await;
    public.tool_limits = tool_cgroup::build_public_settings(state, settings).await;
    public
}

impl SettingsHandle {
    pub async fn settings_snapshot_for_response(
        &self,
    ) -> Result<PublicSettings, SettingsRouteError> {
        let settings = self
            .load_settings()
            .await
            .map_err(SettingsRouteError::internal)?;
        Ok(self.public_settings_for_response(&settings).await)
    }

    pub async fn update_settings_for_request(
        &self,
        req: ctx_settings_model::UpdateSettingsReq,
    ) -> Result<PublicSettings, SettingsRouteError> {
        let current = self
            .load_settings()
            .await
            .map_err(SettingsRouteError::internal)?;
        let host_execution_policy = ctx_settings_service::HostExecutionPolicy::current()
            .map_err(SettingsRouteError::internal)?;
        if req.execution.as_ref().is_some_and(|execution| {
            matches!(execution.mode, ctx_settings_model::ExecutionMode::Host)
        }) {
            host_execution_policy
                .validate_execution_environment(ctx_core::models::ExecutionEnvironment::Host)
                .map_err(SettingsRouteError::forbidden)?;
        }
        let next = ctx_settings_service::apply_update(current, req);
        self.save_settings(&next)
            .await
            .map_err(SettingsRouteError::internal)?;
        self.apply_settings_side_effects(&next).await;
        Ok(self.public_settings_for_response(&next).await)
    }

    pub async fn load_settings(&self) -> anyhow::Result<Settings> {
        ctx_settings_service::load_settings(self.store()).await
    }

    pub async fn save_settings(&self, settings: &Settings) -> anyhow::Result<()> {
        ctx_settings_service::save_settings(self.store(), settings).await
    }

    pub async fn public_settings_for_response(&self, settings: &Settings) -> PublicSettings {
        let mut public = ctx_settings_service::to_public(settings);
        public.resource_governance = resource_governance::build_public_settings_parts(
            self.resource_sampler(),
            self.resource_governance(),
            settings,
        )
        .await;
        public.tool_limits =
            tool_cgroup::build_public_settings_parts(self.resource_sampler(), settings).await;
        public
    }

    pub async fn apply_settings_side_effects(&self, settings: &Settings) {
        let mut telemetry_cfg = TelemetryConfig::default();
        if let Some(telemetry) = settings.telemetry.as_ref() {
            telemetry_cfg.enabled = telemetry.enabled;
            if !telemetry.endpoint.trim().is_empty() {
                telemetry_cfg.endpoint = telemetry.endpoint.clone();
            }
        }
        self.telemetry().update_config(telemetry_cfg).await;
        let perf_enabled = settings
            .telemetry
            .as_ref()
            .map(|t| t.enabled)
            .unwrap_or(true);
        self.perf_telemetry()
            .update_remote_enabled(perf_enabled)
            .await;
        if let Err(err) = resource_governance::apply_settings_parts(
            self.resource_sampler(),
            self.resource_governance(),
            self.providers(),
            self.terminals(),
            settings,
        )
        .await
        {
            tracing::warn!("failed to apply resource governance settings: {err:#}");
        }
        if let Err(err) = provider_guard::apply_settings_parts(
            self.providers(),
            self.resource_sampler(),
            settings,
        )
        .await
        {
            tracing::warn!("failed to apply provider guard settings: {err:#}");
        }
        if let Err(err) = provider_restart::apply_settings_parts(
            self.providers(),
            self.resource_sampler(),
            settings,
        )
        .await
        {
            tracing::warn!("failed to apply provider restart settings: {err:#}");
        }
        if let Err(err) = tool_cgroup::apply_settings_parts(self.resource_sampler(), settings).await
        {
            tracing::warn!("failed to apply tool cgroup settings: {err:#}");
        }
    }
}

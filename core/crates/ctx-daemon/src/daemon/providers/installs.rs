use std::sync::Arc;

use ctx_observability::logs;
use ctx_provider_install::install_state::{
    InstallId, InstallInfo, InstallProgressEvent, InstallTarget,
};
use ctx_provider_install::{
    ProviderInstallJsonRouteError, ProviderInstallJsonRouteErrorStatus,
    ProviderInstallStartRouteResponse, ProviderInstallStatusBatchItem,
    ProviderInstallStatusOnlyRouteError, ProviderInstallStatusesRouteRequest,
    ProviderInstallStatusesRouteResponse,
};
use tokio::sync::broadcast;

use crate::daemon::ProviderInstallHandle;

pub use ctx_provider_runtime::provider_launch::install::StartProviderInstallError;

pub struct ProviderInstallEventStreamRoute {
    pub history: Vec<InstallProgressEvent>,
    pub receiver: broadcast::Receiver<InstallProgressEvent>,
}

pub fn parse_provider_install_target(raw: Option<&str>) -> Result<InstallTarget, String> {
    ctx_managed_installs::parse_install_target(raw).map_err(|error| error.to_string())
}

pub async fn start_provider_install<H>(
    state: &Arc<H>,
    provider_id: &str,
    target: InstallTarget,
) -> Result<InstallId, StartProviderInstallError>
where
    H: ctx_provider_runtime::provider_launch::install::ProviderInstallHost,
{
    let (install_id, _) = ctx_provider_runtime::provider_launch::install::start_provider_install(
        state,
        provider_id,
        target,
    )
    .await?;
    Ok(install_id)
}

pub async fn start_all_provider_installs<H>(
    state: &Arc<H>,
    target: InstallTarget,
) -> Result<Vec<(String, InstallId)>, StartProviderInstallError>
where
    H: ctx_provider_runtime::provider_launch::install::ProviderInstallHost,
{
    ctx_provider_runtime::provider_launch::install::start_all_provider_installs(state, target).await
}

impl ProviderInstallHandle {
    pub async fn start_provider_install_for_route(
        &self,
        provider_id: &str,
        raw_target: Option<&str>,
    ) -> Result<ProviderInstallStartRouteResponse, ProviderInstallJsonRouteError> {
        let target = parse_provider_install_target(raw_target)
            .map_err(ProviderInstallJsonRouteError::bad_request)?;
        let install_host = Arc::new(self.clone());
        let install_id = start_provider_install(&install_host, provider_id, target)
            .await
            .map_err(provider_install_start_route_error)?;
        Ok(ProviderInstallStartRouteResponse::new(
            provider_id.to_string(),
            install_id,
            target,
        ))
    }

    pub async fn start_all_provider_installs_for_route(
        &self,
        raw_target: Option<&str>,
    ) -> Result<Vec<ProviderInstallStartRouteResponse>, ProviderInstallJsonRouteError> {
        let target = parse_provider_install_target(raw_target)
            .map_err(ProviderInstallJsonRouteError::bad_request)?;
        let install_host = Arc::new(self.clone());
        let installs = start_all_provider_installs(&install_host, target)
            .await
            .map_err(provider_install_start_route_error)?;
        Ok(installs
            .into_iter()
            .map(|(provider_id, install_id)| {
                ProviderInstallStartRouteResponse::new(provider_id, install_id, target)
            })
            .collect())
    }

    pub async fn get_provider_install_for_route(
        &self,
        raw_install_id: &str,
    ) -> Result<InstallInfo, ProviderInstallStatusOnlyRouteError> {
        let install_id = parse_install_id_for_status_route(raw_install_id)?;
        self.get_install_polling_info(install_id)
            .await
            .ok_or(ProviderInstallStatusOnlyRouteError::NotFound)
    }

    pub async fn get_provider_install_statuses_for_route(
        &self,
        request: ProviderInstallStatusesRouteRequest,
    ) -> Result<ProviderInstallStatusesRouteResponse, ProviderInstallJsonRouteError> {
        let install_ids = request
            .into_raw_install_ids()
            .into_iter()
            .map(|raw| {
                let parsed = uuid::Uuid::parse_str(raw.trim()).map_err(|_| {
                    ProviderInstallJsonRouteError::bad_request(format!("invalid install id: {raw}"))
                })?;
                Ok(InstallId::from(parsed))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut installs = Vec::with_capacity(install_ids.len());
        for install_id in install_ids {
            let info = self.get_install_polling_info(install_id).await;
            installs.push(ProviderInstallStatusBatchItem::new(
                install_id.to_string(),
                info,
            ));
        }

        Ok(ProviderInstallStatusesRouteResponse::new(installs))
    }

    pub async fn cancel_provider_install_for_route(
        &self,
        raw_install_id: &str,
    ) -> Result<InstallInfo, ProviderInstallStatusOnlyRouteError> {
        let install_id = parse_install_id_for_status_route(raw_install_id)?;
        self.cancel_install(install_id)
            .await
            .ok_or(ProviderInstallStatusOnlyRouteError::NotFound)
    }

    pub async fn list_provider_install_events_for_route(
        &self,
        raw_install_id: &str,
    ) -> Result<Vec<InstallProgressEvent>, ProviderInstallStatusOnlyRouteError> {
        let install_id = parse_install_id_for_status_route(raw_install_id)?;
        self.list_install_events(install_id)
            .await
            .ok_or(ProviderInstallStatusOnlyRouteError::NotFound)
    }

    pub async fn open_provider_install_event_stream_for_route(
        &self,
        raw_install_id: &str,
    ) -> Result<ProviderInstallEventStreamRoute, ProviderInstallStatusOnlyRouteError> {
        let install_id = parse_install_id_for_status_route(raw_install_id)?;
        let Some(sender) = self.install_event_sender(install_id).await else {
            return Err(ProviderInstallStatusOnlyRouteError::NotFound);
        };
        let history = self
            .list_install_events(install_id)
            .await
            .unwrap_or_default();
        Ok(ProviderInstallEventStreamRoute {
            history,
            receiver: sender.subscribe(),
        })
    }
}

fn provider_install_start_route_error(
    error: StartProviderInstallError,
) -> ProviderInstallJsonRouteError {
    let status = if error.code.as_deref() == Some("install_target_disabled") {
        ProviderInstallJsonRouteErrorStatus::Forbidden
    } else {
        ProviderInstallJsonRouteErrorStatus::BadRequest
    };
    ProviderInstallJsonRouteError::start_failure(
        status,
        logs::redact_sensitive(&error.message),
        error.code,
    )
}

fn parse_install_id_for_status_route(
    raw_install_id: &str,
) -> Result<InstallId, ProviderInstallStatusOnlyRouteError> {
    raw_install_id
        .parse::<InstallId>()
        .map_err(|_| ProviderInstallStatusOnlyRouteError::BadRequest)
}

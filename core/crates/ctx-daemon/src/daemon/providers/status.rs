pub use ctx_provider_runtime::provider_status_service::ProviderStatusResponseError;
use ctx_provider_runtime::{
    provider_status_service as status_service, ProviderStatusListRouteError,
    ProviderStatusRouteError, ProviderStatusRouteQuery,
};
use ctx_providers::adapters::ProviderStatus;

use crate::daemon::providers::parse_provider_install_target;
use crate::daemon::ProviderStatusHandle;

impl ProviderStatusHandle {
    pub async fn providers_statuses_for_route(
        &self,
        query: ProviderStatusRouteQuery,
    ) -> Result<Vec<ProviderStatus>, ProviderStatusListRouteError> {
        self.sync_plugin_provider_adapters().await;
        let target = parse_provider_install_target(query.target())
            .map_err(|_| ProviderStatusListRouteError)?;
        Ok(status_service::providers_statuses_response(self, target, false).await)
    }

    pub async fn provider_status_for_route(
        &self,
        provider_id: &str,
        query: ProviderStatusRouteQuery,
    ) -> Result<ProviderStatus, ProviderStatusRouteError> {
        self.sync_plugin_provider_adapters().await;
        let target = parse_provider_install_target(query.target())
            .map_err(ProviderStatusRouteError::bad_request)?;
        status_service::provider_status_response(self, provider_id, target)
            .await
            .map_err(provider_status_route_error)
    }
}

fn provider_status_route_error(error: ProviderStatusResponseError) -> ProviderStatusRouteError {
    match error {
        ProviderStatusResponseError::NotFound { provider_id } => {
            ProviderStatusRouteError::not_found(format!("provider not found: {provider_id}"))
        }
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use ctx_provider_runtime::ProviderStatusRouteErrorKind;

    #[test]
    fn provider_status_route_error_preserves_not_found_body() {
        let error = provider_status_route_error(ProviderStatusResponseError::NotFound {
            provider_id: "missing-provider".to_string(),
        });

        assert_eq!(error.kind(), ProviderStatusRouteErrorKind::NotFound);
        assert_eq!(
            error.body()["error"].as_str(),
            Some("provider not found: missing-provider")
        );
    }
}

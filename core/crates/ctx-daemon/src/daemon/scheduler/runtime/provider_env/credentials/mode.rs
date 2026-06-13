use super::*;
use ctx_harness_sources::HarnessRouteBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderRuntimeCredentialMode {
    Subscription,
    UserManagedEndpoint,
    CtxManagedRelay,
}

pub(super) fn provider_runtime_credential_mode(
    runtime_source_mode: HarnessRuntimeSourceMode,
) -> ProviderRuntimeCredentialMode {
    match runtime_source_mode {
        HarnessRuntimeSourceMode::Subscription => ProviderRuntimeCredentialMode::Subscription,
        HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::UserManaged) => {
            ProviderRuntimeCredentialMode::UserManagedEndpoint
        }
        HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::CtxManagedRelay) => {
            ProviderRuntimeCredentialMode::CtxManagedRelay
        }
    }
}

impl ProviderRuntimeCredentialMode {
    pub(super) fn is_user_managed_endpoint(self) -> bool {
        self == Self::UserManagedEndpoint
    }

    pub(super) fn is_ctx_managed_relay(self) -> bool {
        self == Self::CtxManagedRelay
    }

    pub(super) fn is_subscription(self) -> bool {
        self == Self::Subscription
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_managed_relay_does_not_use_subscription_or_endpoint_credentials() {
        assert_eq!(
            provider_runtime_credential_mode(HarnessRuntimeSourceMode::Endpoint(
                HarnessRouteBackend::CtxManagedRelay
            )),
            ProviderRuntimeCredentialMode::CtxManagedRelay
        );
        assert_eq!(
            provider_runtime_credential_mode(HarnessRuntimeSourceMode::Endpoint(
                HarnessRouteBackend::UserManaged
            )),
            ProviderRuntimeCredentialMode::UserManagedEndpoint
        );
        assert_eq!(
            provider_runtime_credential_mode(HarnessRuntimeSourceMode::Subscription),
            ProviderRuntimeCredentialMode::Subscription
        );
    }
}

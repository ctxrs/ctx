use anyhow::Context;
use ctx_route_contracts::health::{DaemonHealthSnapshot, HealthCompatibility};
use ctx_update_service::BuildIdentity;
use serde::Serialize;

use crate::daemon::HealthHandle;

const MOBILE_API_MIN_VERSION: i64 = 1;
const MOBILE_API_MAX_VERSION: i64 = 1;

pub type HealthSnapshotError = anyhow::Error;

fn build_health_snapshot(
    health: &HealthHandle,
    identity: &BuildIdentity,
    include_sensitive: bool,
) -> anyhow::Result<DaemonHealthSnapshot> {
    let version = identity.exact_version.clone();
    let compatibility_token = if include_sensitive {
        identity.compatibility_token.clone()
    } else {
        String::new()
    };

    Ok(DaemonHealthSnapshot {
        version: version.clone(),
        daemon_version: version.clone(),
        pid: include_sensitive.then_some(std::process::id()),
        data_root: include_sensitive.then(|| health.data_root().to_string_lossy().to_string()),
        daemon_url: include_sensitive.then(|| health.daemon_url().to_string()),
        auth_required: health.auth_required(),
        open_file_limit: if include_sensitive {
            ctx_resource_utilization::process_limits::current_open_file_limit()
                .map(route_json_value)
                .transpose()?
        } else {
            None
        },
        storage: if include_sensitive {
            Some(route_json_value(health.storage_guard_snapshot())?)
        } else {
            None
        },
        compatibility: HealthCompatibility {
            desktop_exact_version: version,
            desktop_build_id: identity.build_id.clone(),
            desktop_dev_instance_id: compatibility_token.clone(),
            protocol_compatibility_token: compatibility_token,
            mobile_api_min: MOBILE_API_MIN_VERSION,
            mobile_api_max: MOBILE_API_MAX_VERSION,
        },
    })
}

fn route_json_value<T: Serialize>(value: T) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(value).context("serializing health route payload")
}

fn health_build_identity(package_version: &'static str) -> BuildIdentity {
    match ctx_update_service::current_build_identity(package_version) {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(
                "failed to load health build identity; falling back to package version: {error:#}"
            );
            BuildIdentity {
                schema_version: 1,
                exact_version: package_version.to_string(),
                build_id: package_version.to_string(),
                compatibility_token: package_version.to_string(),
            }
        }
    }
}

impl HealthHandle {
    pub fn health_snapshot(
        &self,
        package_version: &'static str,
        include_sensitive: bool,
    ) -> Result<DaemonHealthSnapshot, HealthSnapshotError> {
        let identity = health_build_identity(package_version);
        build_health_snapshot(self, &identity, include_sensitive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_for_test(package_version: &'static str) -> BuildIdentity {
        BuildIdentity {
            schema_version: 1,
            exact_version: package_version.to_string(),
            build_id: package_version.to_string(),
            compatibility_token: "test-compatibility-token".to_string(),
        }
    }

    #[test]
    fn health_snapshot_identity_uses_caller_supplied_build_identity() {
        let identity = identity_for_test("ctx-http-package-version");
        let compatibility = HealthCompatibility {
            desktop_exact_version: identity.exact_version.clone(),
            desktop_build_id: identity.build_id.clone(),
            desktop_dev_instance_id: identity.compatibility_token.clone(),
            protocol_compatibility_token: identity.compatibility_token.clone(),
            mobile_api_min: MOBILE_API_MIN_VERSION,
            mobile_api_max: MOBILE_API_MAX_VERSION,
        };

        let serialized = serde_json::to_value(compatibility).unwrap();
        assert_eq!(
            serialized["desktop_exact_version"].as_str(),
            Some("ctx-http-package-version")
        );
        assert_eq!(
            serialized["desktop_build_id"].as_str(),
            Some("ctx-http-package-version")
        );
    }

    #[test]
    fn health_build_identity_falls_back_when_configured_artifact_is_unavailable() {
        let _guard = EnvVarGuard::set_missing_build_identity_path();

        let identity = health_build_identity("ctx-http-package-version");

        assert_eq!(identity.exact_version, "ctx-http-package-version");
        assert_eq!(identity.build_id, "ctx-http-package-version");
        assert_eq!(identity.compatibility_token, "ctx-http-package-version");
    }

    struct EnvVarGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set_missing_build_identity_path() -> Self {
            let previous = std::env::var_os(ctx_update_service::BUILD_IDENTITY_PATH_ENV);
            std::env::set_var(
                ctx_update_service::BUILD_IDENTITY_PATH_ENV,
                std::env::temp_dir().join("ctx-missing-build-identity-for-health-test.json"),
            );
            Self { previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(ctx_update_service::BUILD_IDENTITY_PATH_ENV, previous);
            } else {
                std::env::remove_var(ctx_update_service::BUILD_IDENTITY_PATH_ENV);
            }
        }
    }
}

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

impl HealthHandle {
    pub fn health_snapshot(
        &self,
        package_version: &'static str,
        include_sensitive: bool,
    ) -> Result<DaemonHealthSnapshot, HealthSnapshotError> {
        let identity = ctx_update_service::current_build_identity(package_version)?;
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
}

use anyhow::Context;
use ctx_observability::logs;
use ctx_route_contracts::diagnostics::DaemonDiagnosticsSnapshot;

use crate::daemon::health::HealthSnapshotError;
use crate::daemon::DiagnosticsHandle;

pub type DiagnosticsSnapshotError = anyhow::Error;

async fn linux_sandbox_runtime_diagnostics(data_root: &std::path::Path) -> serde_json::Value {
    ctx_linux_sandbox_runtime::linux_sandbox_runtime_status(data_root)
        .await
        .map(|status| serde_json::to_value(status).unwrap_or_else(|_| serde_json::json!({})))
        .unwrap_or_else(|err| linux_sandbox_runtime_error_diagnostics(&err))
}

fn linux_sandbox_runtime_error_diagnostics(error: &anyhow::Error) -> serde_json::Value {
    serde_json::json!({"error": logs::redact_sensitive(&error.to_string())})
}

impl DiagnosticsHandle {
    pub async fn diagnostics_snapshot(
        &self,
        package_version: &'static str,
    ) -> Result<DaemonDiagnosticsSnapshot, DiagnosticsSnapshotError> {
        let daemon = self
            .health()
            .health_snapshot(package_version, true)
            .map_err(|error: HealthSnapshotError| error)?;
        let startup_prewarm = self.execution_setup().startup_status().await;
        let linux_sandbox_runtime = linux_sandbox_runtime_diagnostics(self.data_root()).await;
        let provider_diagnostics =
            crate::daemon::providers::provider_diagnostics_snapshot_for_runtime(
                self.data_root(),
                self.providers(),
            )
            .await;
        let providers = provider_diagnostics
            .providers
            .into_iter()
            .map(|provider| {
                serde_json::to_value(provider).context("serializing provider diagnostics")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let log_files = logs::list_log_files(self.data_root()).await;

        Ok(DaemonDiagnosticsSnapshot {
            daemon,
            platform: serde_json::json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            }),
            logs: serde_json::json!({
                "dir": logs::logs_dir(self.data_root()).to_string_lossy(),
                "files": log_files,
            }),
            execution: serde_json::json!({
                "startup_prewarm": startup_prewarm,
                "linux_sandbox_runtime": linux_sandbox_runtime,
            }),
            providers,
            managed_installs: provider_diagnostics.managed_installs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_sandbox_runtime_error_diagnostics_uses_error_field() {
        let value =
            linux_sandbox_runtime_error_diagnostics(&anyhow::anyhow!("failed with secret-token"));

        assert!(value["error"]
            .as_str()
            .is_some_and(|error| error.contains("failed")));
    }
}

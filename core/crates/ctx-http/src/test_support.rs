use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use ctx_daemon::daemon::{
    ProviderAccountsHandle, ProviderHarnessConfigHandle, ProviderInstallHandle, SettingsHandle,
};
use ctx_daemon::test_support::TestDaemon;
use ctx_providers::adapters::ProviderAdapter;
use ctx_providers::fake::FakeProviderAdapter;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(test)]
use std::path::{Path, PathBuf};

/// Tests that mutate process-global environment or manifest override state
/// must hold this lock for the full lifetime of the test.
pub(crate) fn process_env_test_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// Workspace-runtime tests historically used a sandbox-specific name for the
/// shared sandbox-runtime lock. Keep that lock separate from the broader
/// process-env lock so long-lived runtime jobs are not queued behind unrelated
/// bundle/env tests.
pub(crate) fn sandbox_cli_env_test_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

pub(crate) struct TestDaemonFixture {
    daemon: TestDaemon,
    data_dir: tempfile::TempDir,
}

impl TestDaemonFixture {
    pub(crate) async fn new(base_url: impl Into<String>) -> Self {
        Self::with_providers(fake_providers(), base_url).await
    }

    pub(crate) async fn with_providers(
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        base_url: impl Into<String>,
    ) -> Self {
        Self::with_providers_and_auth_token(providers, base_url, None).await
    }

    pub(crate) async fn with_providers_and_auth_token(
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        base_url: impl Into<String>,
        auth_token: Option<String>,
    ) -> Self {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let daemon = TestDaemon::new_with_providers_for_test(
            data_dir.path().to_path_buf(),
            providers,
            base_url.into(),
            auth_token,
        )
        .await
        .expect("create test daemon");
        Self { daemon, data_dir }
    }

    pub(crate) fn daemon(&self) -> &TestDaemon {
        &self.daemon
    }

    pub(crate) fn data_root(&self) -> &Path {
        self.data_dir.path()
    }

    pub(crate) fn router(&self) -> axum::Router {
        crate::api::router(crate::api::RouteHandles::from_daemon_route_handles(
            self.daemon.route_handles(),
        ))
    }

    pub(crate) fn provider_accounts(&self) -> ProviderAccountsHandle {
        self.daemon.provider_accounts_handle_for_test()
    }

    pub(crate) fn provider_harness_config(&self) -> ProviderHarnessConfigHandle {
        self.daemon.provider_harness_config_handle_for_test()
    }

    pub(crate) fn provider_install(&self) -> ProviderInstallHandle {
        self.daemon.provider_install_handle_for_test()
    }

    pub(crate) fn settings(&self) -> SettingsHandle {
        self.daemon.settings_handle_for_test()
    }
}

pub(crate) struct DataRootTestDaemonFixture {
    daemon: TestDaemon,
}

impl DataRootTestDaemonFixture {
    pub(crate) async fn new(data_root: &Path, base_url: impl Into<String>) -> Self {
        Self::with_providers(data_root, fake_providers(), base_url).await
    }

    pub(crate) async fn with_providers(
        data_root: &Path,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        base_url: impl Into<String>,
    ) -> Self {
        Self::with_providers_and_auth_token(data_root, providers, base_url, None).await
    }

    pub(crate) async fn with_providers_and_auth_token(
        data_root: &Path,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        base_url: impl Into<String>,
        auth_token: Option<String>,
    ) -> Self {
        let daemon = TestDaemon::new_with_providers_for_test(
            data_root.to_path_buf(),
            providers,
            base_url.into(),
            auth_token,
        )
        .await
        .expect("create data-root test daemon");
        Self { daemon }
    }

    pub(crate) fn daemon(&self) -> &TestDaemon {
        &self.daemon
    }

    pub(crate) fn router(&self) -> axum::Router {
        crate::api::router(crate::api::RouteHandles::from_daemon_route_handles(
            self.daemon.route_handles(),
        ))
    }
}

fn fake_providers() -> HashMap<String, Arc<dyn ProviderAdapter>> {
    HashMap::from([(
        "fake".to_string(),
        Arc::new(FakeProviderAdapter::new()) as Arc<dyn ProviderAdapter>,
    )])
}

#[cfg(unix)]
pub(crate) fn write_running_container_sandbox_cli_shim(
    dir: &Path,
    log_path: &Path,
    container_name: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("sandbox-cli-running-container-test.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  printf '{{}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  echo 'transient image store failure' >&2\n  exit 125\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 1\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"inspect\" ] && [ \"$2\" = \"{container}\" ]; then\n  suffix=${{2#ctx-harness-}}\n  printf '[{{\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"ctx-ws-%s\",\"Destination\":\"/ctx/ws\"}}]}}]\\n' \"$suffix\"\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$3\" = \"{container}\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$5\" = \"{container}\" ]; then\n  printf 'true\\n'\n  exit 0\nfi\nif [ \"$1\" = \"exec\" ]; then\n  shift\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in\n      --interactive)\n        shift\n        ;;\n      --user|--workdir|--env)\n        shift 2\n        ;;\n      *)\n        break\n        ;;\n    esac\n  done\n  container_name=\"$1\"\n  shift\n  command=\"$1\"\n  shift\n  if [ \"$container_name\" != \"{container}\" ]; then\n    echo \"unexpected container: $container_name\" >&2\n    exit 1\n  fi\n  if [ \"$command\" = \"tar\" ] && [ \"$1\" = \"-xf\" ] && [ \"$2\" = \"-\" ]; then\n    cat >/dev/null\n    exit 0\n  fi\n  if [ \"$command\" = \"git\" ] && [ \"$1\" = \"checkout\" ]; then\n    exit 0\n  fi\n  if [ \"$command\" = \"id\" ] && [ \"$1\" = \"-u\" ]; then\n    printf '1000\\n'\n    exit 0\n  fi\n  if [ \"$command\" = \"id\" ] && [ \"$1\" = \"-g\" ]; then\n    printf '1000\\n'\n    exit 0\n  fi\n  if [ \"$command\" = \"df\" ] && [ \"$1\" = \"-Pk\" ]; then\n    printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n'\n    printf 'overlay 10485760 1024 7340032 1%% /ctx/ws\\n'\n    exit 0\n  fi\n  if [ \"$command\" = \"sh\" ] && [ \"$1\" = \"-lc\" ]; then\n    case \"$2\" in\n      *\"git rev-parse --is-inside-work-tree\"*)\n        printf 'true\\n'\n        exit 0\n        ;;\n      *)\n        exit 0\n        ;;\n    esac\n  fi\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            container = container_name,
        ),
    )
    .expect("write running-container sandbox CLI shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod running-container sandbox CLI shim");
    path
}

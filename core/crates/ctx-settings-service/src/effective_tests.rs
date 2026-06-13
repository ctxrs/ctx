use std::path::Path;

use ctx_core::models::ExecutionEnvironment as SessionExecutionEnvironment;
use ctx_provider_install::install_state::InstallTarget;
use ctx_settings_model::{ContainerNetworkMode, ExecutionMode, ExecutionSettings, Settings};
use ctx_store::Store;
use ctx_workspace_config::{ExecutionConfigUpdate, ExecutionEnvironment};

use crate::{
    effective_execution_settings, effective_execution_settings_classified,
    effective_execution_settings_for_environment, install_target_for_settings, save_settings,
    update_workspace_execution_config_for_loaded_settings,
    validate_workspace_execution_settings_override,
    workspace_execution_config_snapshot_for_loaded_settings, WorkspaceExecutionConfigSnapshotError,
    WorkspaceExecutionConfigUpdateError, EXECUTION_POLICY_TEST_ENV_LOCK,
};

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

struct ExecutionEnvGuards {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    _policy: EnvVarGuard,
    _mode: EnvVarGuard,
}

async fn clean_execution_env() -> ExecutionEnvGuards {
    let lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    ExecutionEnvGuards {
        _lock: lock,
        _policy: EnvVarGuard::remove("CTX_HOST_EXECUTION_POLICY"),
        _mode: EnvVarGuard::remove("CTX_EXECUTION_MODE"),
    }
}

async fn sandbox_only_execution_env() -> ExecutionEnvGuards {
    let lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    ExecutionEnvGuards {
        _lock: lock,
        _policy: EnvVarGuard::set("CTX_HOST_EXECUTION_POLICY", "sandbox_only"),
        _mode: EnvVarGuard::remove("CTX_EXECUTION_MODE"),
    }
}

async fn open_store(path: &Path) -> Store {
    Store::open(path).await.expect("open store")
}

async fn stores() -> (tempfile::TempDir, Store, Store) {
    let temp = tempfile::tempdir().expect("tempdir");
    let global = open_store(&temp.path().join("global.sqlite")).await;
    let workspace = open_store(&temp.path().join("workspace.sqlite")).await;
    (temp, global, workspace)
}

async fn set_daemon_execution_settings(global_store: &Store, execution: ExecutionSettings) {
    save_settings(
        global_store,
        &Settings {
            execution: Some(execution),
            ..Settings::default()
        },
    )
    .await
    .expect("save daemon settings");
}

#[tokio::test]
async fn workspace_execution_config_snapshot_uses_daemon_default_without_override() {
    let _env = clean_execution_env().await;
    let (_temp, _global_store, workspace_store) = stores().await;
    let settings = Settings {
        execution: Some(ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        }),
        ..Settings::default()
    };

    let snapshot =
        workspace_execution_config_snapshot_for_loaded_settings(&settings, &workspace_store)
            .await
            .expect("snapshot");

    assert_eq!(snapshot.source, "daemon_default");
    assert_eq!(snapshot.environment, "sandbox");
}

#[tokio::test]
async fn workspace_execution_config_snapshot_applies_workspace_override_before_projection() {
    let _env = clean_execution_env().await;
    let (_temp, _global_store, workspace_store) = stores().await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Sandbox,
            network_mode: Some(ContainerNetworkMode::All),
            allowlist: Some(vec!["api.openai.com".to_string()]),
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let snapshot = workspace_execution_config_snapshot_for_loaded_settings(
        &Settings::default(),
        &workspace_store,
    )
    .await
    .expect("snapshot");

    assert_eq!(snapshot.source, "workspace");
    assert_eq!(snapshot.environment, "sandbox");
    assert_eq!(snapshot.network_mode.as_deref(), Some("all"));
    assert_eq!(snapshot.allowlist, Some(vec!["api.openai.com".to_string()]));
}

#[tokio::test]
async fn workspace_execution_config_snapshot_classifies_malformed_workspace_config() {
    let _env = clean_execution_env().await;
    let (_temp, _global_store, workspace_store) = stores().await;
    workspace_store
        .upsert_runtime_settings_document(
            1,
            r#"{
  "execution": {
    "environment": 7
  }
}"#,
        )
        .await
        .expect("write malformed workspace settings");

    let err = workspace_execution_config_snapshot_for_loaded_settings(
        &Settings::default(),
        &workspace_store,
    )
    .await
    .expect_err("malformed config should be classified");

    assert!(matches!(
        err,
        WorkspaceExecutionConfigSnapshotError::InvalidWorkspaceConfig(_)
    ));
}

#[tokio::test]
async fn workspace_execution_config_snapshot_classifies_persisted_policy_denial() {
    let _env = clean_execution_env().await;
    let (_temp, _global_store, workspace_store) = stores().await;
    let settings = Settings {
        execution: Some(ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        }),
        ..Settings::default()
    };
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Host,
            network_mode: None,
            allowlist: None,
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let err = workspace_execution_config_snapshot_for_loaded_settings(&settings, &workspace_store)
        .await
        .expect_err("policy denial should be classified");

    let WorkspaceExecutionConfigSnapshotError::RequestOrPolicy(error) = err else {
        panic!("expected request/policy classification");
    };
    assert!(crate::is_execution_policy_denial(&error));
}

#[tokio::test]
async fn update_workspace_execution_config_persists_normalized_update() {
    let _env = clean_execution_env().await;
    let (_temp, _global_store, workspace_store) = stores().await;
    let update = ctx_workspace_config::parse_execution_config_update_input(
        "sandbox",
        Some(" allowlist "),
        Some(vec![" api.openai.com ".to_string(), "".to_string()]),
        true,
    )
    .expect("parse update");

    update_workspace_execution_config_for_loaded_settings(
        &Settings::default(),
        &workspace_store,
        update,
    )
    .await
    .expect("persist update");

    let loaded = ctx_workspace_config::load_execution_settings_override(&workspace_store)
        .await
        .expect("load override")
        .expect("override");
    assert_eq!(loaded.mode, Some(ExecutionMode::Sandbox));
    assert_eq!(
        loaded.container.network_mode,
        Some(ContainerNetworkMode::Allowlist)
    );
    assert_eq!(
        loaded.container.allowlist,
        Some(vec!["api.openai.com".to_string()])
    );
}

#[tokio::test]
async fn update_workspace_execution_config_classifies_policy_denial() {
    let _env = clean_execution_env().await;
    let (_temp, _global_store, workspace_store) = stores().await;
    let settings = Settings {
        execution: Some(ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        }),
        ..Settings::default()
    };
    let update =
        ctx_workspace_config::parse_execution_config_update_input("host", None, None, true)
            .expect("parse update");

    let err =
        update_workspace_execution_config_for_loaded_settings(&settings, &workspace_store, update)
            .await
            .expect_err("host update should be denied");

    let WorkspaceExecutionConfigUpdateError::RequestOrPolicy(error) = err else {
        panic!("expected request/policy classification");
    };
    assert!(crate::is_execution_policy_denial(&error));
}

#[test]
fn install_target_for_settings_matches_execution_mode() {
    let host = ExecutionSettings {
        mode: ExecutionMode::Host,
        ..ExecutionSettings::default()
    };
    let container = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        ..ExecutionSettings::default()
    };

    assert_eq!(install_target_for_settings(&host), InstallTarget::Host);
    assert_eq!(
        install_target_for_settings(&container),
        InstallTarget::Container
    );
}

#[tokio::test]
async fn persisted_host_session_cannot_broaden_daemon_sandbox_policy() {
    let _env = clean_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        },
    )
    .await;

    let err = effective_execution_settings_for_environment(
        &global_store,
        &workspace_store,
        SessionExecutionEnvironment::Host,
    )
    .await
    .expect_err("persisted host session must not broaden daemon sandbox policy");
    let message = format!("{err:#}");
    assert!(message.contains("host is not allowed"));
    assert!(crate::is_execution_policy_denial(&err));
}

#[tokio::test]
async fn persisted_sandbox_session_can_restrict_daemon_host_policy() {
    let _env = clean_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;

    let effective = effective_execution_settings_for_environment(
        &global_store,
        &workspace_store,
        SessionExecutionEnvironment::Sandbox,
    )
    .await
    .expect("persisted sandbox session may restrict host default");

    assert_eq!(effective.mode, ExecutionMode::Sandbox);
}

#[tokio::test]
async fn sandbox_only_policy_allows_persisted_sandbox_session_with_existing_host_default() {
    let _env = sandbox_only_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Host,
            ..ExecutionSettings::default()
        },
    )
    .await;

    let effective = effective_execution_settings_for_environment(
        &global_store,
        &workspace_store,
        SessionExecutionEnvironment::Sandbox,
    )
    .await
    .expect("persisted sandbox session should survive sandbox-only policy enablement");

    assert_eq!(effective.mode, ExecutionMode::Sandbox);
}

#[tokio::test]
async fn sandbox_only_policy_normalizes_persisted_workspace_host_override_to_sandbox() {
    let _env = sandbox_only_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Host,
            network_mode: None,
            allowlist: None,
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let effective = effective_execution_settings(&global_store, &workspace_store)
        .await
        .expect("sandbox-only policy should constrain persisted host override to sandbox");

    assert_eq!(effective.mode, ExecutionMode::Sandbox);
}

#[tokio::test]
async fn sandbox_only_policy_drops_stale_container_fields_from_persisted_workspace_host_override() {
    let _env = sandbox_only_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        },
    )
    .await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Host,
            network_mode: Some(ContainerNetworkMode::All),
            allowlist: Some(vec!["example.com".to_string()]),
            image: Some("ignored.example/legacy-host".to_string()),
        },
    )
    .await
    .expect("write workspace override");

    let effective = effective_execution_settings(&global_store, &workspace_store)
        .await
        .expect("stale host override container fields should be ignored under sandbox-only");

    assert_eq!(effective.mode, ExecutionMode::Sandbox);
    assert_eq!(
        effective.container.network_mode,
        ContainerNetworkMode::LlmOnly
    );
    assert!(effective.container.allowlist.is_empty());
    assert_eq!(effective.container.image, None);
}

#[tokio::test]
async fn sandbox_only_policy_rejects_new_workspace_host_override() {
    let _env = sandbox_only_execution_env().await;
    let base = ExecutionSettings::default();
    let override_config = ctx_workspace_config::ExecutionSettingsOverride {
        mode: Some(ExecutionMode::Host),
        ..Default::default()
    };

    let err = validate_workspace_execution_settings_override(&base, &override_config)
        .expect_err("new workspace host override must be rejected");
    let message = format!("{err:#}");
    assert!(message.contains("host execution is disabled by daemon policy"));
}

#[tokio::test]
async fn sandbox_only_policy_rejects_ctx_execution_mode_host_override() {
    let _env_guard = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    let _policy = EnvVarGuard::set("CTX_HOST_EXECUTION_POLICY", "sandbox_only");
    let _mode = EnvVarGuard::set("CTX_EXECUTION_MODE", "host");
    let (_temp, global_store, workspace_store) = stores().await;

    let err = effective_execution_settings_classified(&global_store, &workspace_store)
        .await
        .expect_err("sandbox-only policy must reject CTX_EXECUTION_MODE=host");
    let message = format!("{:#}", err.into_inner());
    assert!(message.contains("CTX_EXECUTION_MODE=host is disabled"));
}

#[tokio::test]
async fn sandbox_only_policy_normalizes_existing_host_default_to_sandbox() {
    let _env = sandbox_only_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Host,
            ..ExecutionSettings::default()
        },
    )
    .await;

    let effective = effective_execution_settings(&global_store, &workspace_store)
        .await
        .expect("sandbox-only policy should repair stored host default at read time");

    assert_eq!(effective.mode, ExecutionMode::Sandbox);
}

#[tokio::test]
async fn sandbox_only_policy_drops_stale_container_fields_from_existing_host_default() {
    let _env = sandbox_only_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Host,
            container: ctx_settings_model::ContainerExecutionSettings {
                network_mode: ContainerNetworkMode::All,
                allowlist: vec!["example.com".to_string()],
                image: Some("ignored.example/legacy-host".to_string()),
                ..Default::default()
            },
        },
    )
    .await;

    let effective = effective_execution_settings(&global_store, &workspace_store)
        .await
        .expect("sandbox-only policy should ignore stale host default container fields");

    assert_eq!(effective.mode, ExecutionMode::Sandbox);
    assert_eq!(
        effective.container.network_mode,
        ContainerNetworkMode::LlmOnly
    );
    assert!(effective.container.allowlist.is_empty());
    assert_eq!(effective.container.image, None);
}

#[tokio::test]
async fn workspace_override_cannot_broaden_daemon_sandbox_to_host() {
    let _env = clean_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        },
    )
    .await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Host,
            network_mode: None,
            allowlist: None,
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let err = effective_execution_settings_classified(&global_store, &workspace_store)
        .await
        .expect_err("workspace host override must not broaden daemon sandbox policy");
    let message = format!("{:#}", err.into_inner());
    assert!(message.contains("cannot select host"));
}

#[tokio::test]
async fn workspace_override_can_restrict_daemon_host_to_sandbox() {
    let _env = clean_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Sandbox,
            network_mode: Some(ContainerNetworkMode::All),
            allowlist: None,
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let effective = effective_execution_settings(&global_store, &workspace_store)
        .await
        .expect("workspace may restrict host default to sandbox");
    assert_eq!(effective.mode, ExecutionMode::Sandbox);
    assert_eq!(effective.container.network_mode, ContainerNetworkMode::All);
}

#[tokio::test]
async fn workspace_override_cannot_broaden_daemon_sandbox_network_mode() {
    let _env = clean_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        },
    )
    .await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Sandbox,
            network_mode: Some(ContainerNetworkMode::All),
            allowlist: None,
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let err = effective_execution_settings_classified(&global_store, &workspace_store)
        .await
        .expect_err("workspace network override must not broaden daemon sandbox policy");
    let message = format!("{:#}", err.into_inner());
    assert!(message.contains("cannot broaden sandbox network mode"));
}

#[tokio::test]
async fn workspace_allowlist_override_must_be_subset_of_daemon_allowlist() {
    let _env = clean_execution_env().await;
    let (_temp, global_store, workspace_store) = stores().await;
    set_daemon_execution_settings(
        &global_store,
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ctx_settings_model::ContainerExecutionSettings {
                network_mode: ContainerNetworkMode::Allowlist,
                allowlist: vec!["api.openai.com".to_string()],
                ..Default::default()
            },
        },
    )
    .await;
    ctx_workspace_config::update_execution_config(
        &workspace_store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Sandbox,
            network_mode: Some(ContainerNetworkMode::Allowlist),
            allowlist: Some(vec![
                "api.openai.com".to_string(),
                "example.com".to_string(),
            ]),
            image: None,
        },
    )
    .await
    .expect("write workspace override");

    let err = effective_execution_settings_classified(&global_store, &workspace_store)
        .await
        .expect_err("workspace allowlist must not broaden daemon sandbox allowlist");
    let message = format!("{:#}", err.into_inner());
    assert!(message.contains("example.com"));
    assert!(message.contains("daemon sandbox allowlist"));
}

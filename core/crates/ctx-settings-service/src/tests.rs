use super::*;
use ctx_settings_model::update::{
    UpdateDictationSettingsReq, UpdateLiveKitDictationSettingsReq,
    UpdateTitleGenerationRemoteSettingsReq, UpdateTitleGenerationSettingsReq,
};
use ctx_store::Store;
use serde_json::json;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
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

struct CleanExecutionEnvGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    _policy: EnvVarGuard,
    _mode: EnvVarGuard,
}

async fn clean_execution_env() -> CleanExecutionEnvGuard {
    let lock = EXECUTION_POLICY_TEST_ENV_LOCK.lock().await;
    CleanExecutionEnvGuard {
        _lock: lock,
        _policy: EnvVarGuard::remove("CTX_HOST_EXECUTION_POLICY"),
        _mode: EnvVarGuard::remove("CTX_EXECUTION_MODE"),
    }
}

fn runtime_settings_secret_sidecar_path(
    root: &std::path::Path,
    db_file_name: &str,
    secret_ref: &str,
) -> std::path::PathBuf {
    root.join("runtime_settings_secrets")
        .join(db_file_name)
        .join(format!("{secret_ref}.json"))
}

fn sqlite_artifact_paths(db_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    vec![
        db_path.to_path_buf(),
        db_path.with_extension("sqlite-wal"),
        db_path.with_extension("sqlite-shm"),
        db_path.with_extension("sqlite-journal"),
    ]
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

async fn assert_secret_absent_from_sqlite_artifacts(db_path: &std::path::Path, secret: &str) {
    for artifact_path in sqlite_artifact_paths(db_path) {
        let bytes = match tokio::fs::read(&artifact_path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => panic!(
                "failed to read sqlite artifact {}: {err}",
                artifact_path.display()
            ),
        };
        assert!(
            !bytes_contain(&bytes, secret.as_bytes()),
            "found secret bytes in sqlite artifact {}",
            artifact_path.display()
        );
    }
}

#[test]
fn network_profiles_defaults_are_safe_for_system_tasks() {
    let settings = NetworkProfilesSettings::default();
    assert_eq!(settings.agent_default.mode, ContainerNetworkMode::LlmOnly);
    assert_eq!(settings.merge_queue.mode, ContainerNetworkMode::All);
    assert_eq!(settings.worktree_setup.mode, ContainerNetworkMode::All);
    assert_eq!(settings.user_shell.mode, ContainerNetworkMode::All);
    assert!(settings.merge_queue.allowlist.is_empty());
    assert!(settings.worktree_setup.allowlist.is_empty());
}

#[test]
fn container_machine_defaults_are_stable() {
    let settings = ContainerExecutionSettings::default();
    #[cfg(target_os = "macos")]
    {
        assert_eq!(settings.runtime, ContainerRuntimeKind::SharedVmContainer);
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(settings.runtime, ContainerRuntimeKind::NativeContainer);
    }
    assert_eq!(settings.mount_mode, ContainerMountMode::DiskIsolated);
    assert_eq!(
        settings.machine.memory_profile,
        ContainerMachineMemoryProfile::Economy
    );
    assert_eq!(settings.machine.custom_memory_mb, None);
    assert_eq!(default_container_machine_idle_shutdown_seconds(), 60 * 60);
    assert_eq!(
        settings.machine.idle_shutdown_seconds,
        default_container_machine_idle_shutdown_seconds()
    );
    assert_eq!(
        settings.machine.host_pressure_swap_threshold_mb,
        default_container_machine_host_pressure_swap_threshold_mb()
    );
}

#[test]
fn normalize_container_execution_settings_coerces_legacy_mount_mode_to_disk_isolated() {
    let mut settings = ContainerExecutionSettings {
        runtime: ContainerRuntimeKind::SharedVmContainer,
        mount_mode: ContainerMountMode::Legacy,
        ..ContainerExecutionSettings::default()
    };

    normalize_container_execution_settings(&mut settings);

    assert_eq!(settings.mount_mode, ContainerMountMode::DiskIsolated);
}

#[test]
fn apply_update_preserves_internal_runtime_fields() {
    let next = apply_update(
        Settings::default(),
        UpdateSettingsReq {
            dictation: None,
            title_generation: None,
            oracle: None,
            telemetry: None,
            resource_governance: None,
            provider_guard: None,
            tool_limits: None,
            provider_restart: None,
            subagents: None,
            sandboxing: None,
            execution: Some(update::UpdateExecutionSettingsReq {
                mode: ExecutionMode::Sandbox,
                container: update::UpdateContainerExecutionSettingsReq {
                    network_mode: ContainerNetworkMode::LlmOnly,
                    allowlist: vec![],
                    image: None,
                    machine: ContainerMachineSettings::default(),
                },
            }),
            network_profiles: None,
        },
    );

    assert_eq!(
        next.execution
            .as_ref()
            .expect("execution settings")
            .container
            .mount_mode,
        ContainerMountMode::DiskIsolated
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        next.execution
            .as_ref()
            .expect("execution settings")
            .container
            .runtime,
        ContainerRuntimeKind::SharedVmContainer
    );
}

#[test]
fn apply_update_forces_full_provider_control_mode() {
    let next = apply_update(
        Settings::default(),
        UpdateSettingsReq {
            dictation: None,
            title_generation: None,
            oracle: None,
            telemetry: None,
            resource_governance: None,
            provider_guard: None,
            tool_limits: None,
            provider_restart: None,
            subagents: None,
            sandboxing: Some(update::UpdateSandboxingSettingsReq {
                provider_control_mode: ProviderControlMode::HarnessNative,
            }),
            execution: None,
            network_profiles: None,
        },
    );

    assert_eq!(
        next.sandboxing
            .as_ref()
            .expect("sandboxing settings")
            .provider_control_mode,
        ProviderControlMode::Full
    );
}

#[test]
fn to_public_redacts_secret_values() {
    let settings = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: "lk-key".to_string(),
                api_secret: Some("lk-secret".to_string()),
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        title_generation: Some(TitleGenerationSettings {
            mode: TitleGenerationMode::Remote,
            remote: TitleGenerationRemoteSettings {
                base_url: "https://titles.example".to_string(),
                api_key: "title-key".to_string(),
                model: "gpt-test".to_string(),
                use_json: true,
            },
            local: TitleGenerationLocalSettings::default(),
        }),
        oracle: Some(OracleSettings {
            api_key: "oracle-key".to_string(),
            ..OracleSettings::default()
        }),
        execution: Some(ExecutionSettings::default()),
        ..Settings::default()
    };

    let public = to_public(&settings);
    let livekit = public
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref())
        .expect("dictation livekit");
    assert!(livekit.api_key_set);
    assert!(livekit.api_secret_set);

    let title_remote = public
        .title_generation
        .as_ref()
        .map(|titling| &titling.remote)
        .expect("title generation");
    assert!(title_remote.api_key_set);
    assert_eq!(title_remote.base_url, "https://titles.example");
    assert_eq!(title_remote.model, "gpt-test");
    assert_eq!(
        public
            .execution
            .as_ref()
            .expect("execution settings")
            .container
            .machine
            .memory_profile,
        ContainerMachineMemoryProfile::Economy
    );
    let oracle = public.oracle.as_ref().expect("oracle");
    assert!(oracle.api_key_set);
}

#[tokio::test]
async fn save_settings_persists_runtime_secrets_outside_sqlite() {
    let _env = clean_execution_env().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let settings = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: "lk-key".to_string(),
                api_secret: Some("lk-secret".to_string()),
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        title_generation: Some(TitleGenerationSettings {
            mode: TitleGenerationMode::Remote,
            remote: TitleGenerationRemoteSettings {
                base_url: "https://titles.example".to_string(),
                api_key: "title-key".to_string(),
                model: "gpt-test".to_string(),
                use_json: true,
            },
            local: TitleGenerationLocalSettings::default(),
        }),
        oracle: Some(OracleSettings {
            api_key: "oracle-key".to_string(),
            ..OracleSettings::default()
        }),
        ..Settings::default()
    };

    save_settings(&store, &settings).await.unwrap();
    let doc = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document");
    let settings_json = doc.settings_json;
    let secret_ref = doc.secret_ref.expect("runtime settings secret_ref");
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);

    assert!(!secret_ref.is_empty());
    assert!(!settings_json.contains("lk-key"));
    assert!(!settings_json.contains("lk-secret"));
    assert!(!settings_json.contains("title-key"));
    assert!(!settings_json.contains("oracle-key"));
    assert!(secret_path.exists());
    assert_secret_absent_from_sqlite_artifacts(&db_path, "lk-key").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "lk-secret").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "title-key").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "oracle-key").await;

    let loaded = load_settings(&store).await.unwrap();
    let livekit = loaded
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref())
        .unwrap();
    assert_eq!(livekit.api_key, "lk-key");
    assert_eq!(livekit.api_secret.as_deref(), Some("lk-secret"));
    assert_eq!(
        loaded.title_generation.as_ref().unwrap().remote.api_key,
        "title-key"
    );
    assert_eq!(loaded.oracle.as_ref().unwrap().api_key, "oracle-key");
}

#[tokio::test]
async fn load_settings_migrates_legacy_runtime_setting_secrets() {
    let _env = clean_execution_env().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let legacy = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: "lk-key".to_string(),
                api_secret: Some("lk-secret".to_string()),
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        title_generation: Some(TitleGenerationSettings {
            mode: TitleGenerationMode::Remote,
            remote: TitleGenerationRemoteSettings {
                base_url: "https://titles.example".to_string(),
                api_key: "title-key".to_string(),
                model: "gpt-test".to_string(),
                use_json: true,
            },
            local: TitleGenerationLocalSettings::default(),
        }),
        oracle: Some(OracleSettings {
            api_key: "oracle-key".to_string(),
            ..OracleSettings::default()
        }),
        ..Settings::default()
    };
    let legacy_json = serde_json::to_string_pretty(&legacy).unwrap();
    store
        .upsert_runtime_settings_document(1, &legacy_json)
        .await
        .unwrap();
    store.close().await;

    let store = Store::open(&db_path).await.unwrap();

    let loaded = load_settings(&store).await.unwrap();
    let livekit = loaded
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref())
        .unwrap();
    assert_eq!(livekit.api_key, "lk-key");
    assert_eq!(livekit.api_secret.as_deref(), Some("lk-secret"));
    assert_eq!(
        loaded.title_generation.as_ref().unwrap().remote.api_key,
        "title-key"
    );
    assert_eq!(loaded.oracle.as_ref().unwrap().api_key, "oracle-key");

    let doc = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document");
    let settings_json = doc.settings_json;
    let secret_ref = doc.secret_ref.expect("runtime settings secret_ref");
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);

    assert!(!settings_json.contains("lk-key"));
    assert!(!settings_json.contains("lk-secret"));
    assert!(!settings_json.contains("title-key"));
    assert!(!settings_json.contains("oracle-key"));
    assert!(secret_path.exists());
    assert_secret_absent_from_sqlite_artifacts(&db_path, "lk-key").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "lk-secret").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "title-key").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "oracle-key").await;
}

#[tokio::test]
async fn load_settings_removes_legacy_cloud_worker_settings_and_secrets() {
    let _env = clean_execution_env().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let legacy_settings_json = serde_json::to_string_pretty(&json!({
        "cloud_workers": {
            "gateway": {
                "provider": "aws",
                "gateway_url": "https://worker-gateway.example"
            },
            "aws": {
                "access_key_id": "aws-access-key",
                "secret_access_key": "aws-secret-key",
                "region": "us-east-1"
            }
        }
    }))
    .unwrap();
    let legacy_secret_json = serde_json::to_string_pretty(&json!({
        "version": RUNTIME_SETTINGS_SECRET_VERSION,
        "dictation_livekit_api_key": "",
        "dictation_livekit_api_secret": null,
        "title_generation_remote_api_key": "",
        "oracle_api_key": "",
        "aws_cloud_workers_access_key_id": "aws-access-key",
        "aws_cloud_workers_secret_access_key": "aws-secret-key"
    }))
    .unwrap();
    let doc = store
        .upsert_runtime_settings_document_with_secrets(
            1,
            &legacy_settings_json,
            &legacy_secret_json,
        )
        .await
        .unwrap();
    let secret_ref = doc.secret_ref.expect("runtime settings secret_ref");
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    assert!(secret_path.exists());

    let loaded = load_settings(&store).await.unwrap();
    let loaded_json = serde_json::to_string(&loaded).unwrap();
    assert!(!loaded_json.contains("cloud_workers"));

    let doc = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document");
    assert!(doc.secret_ref.is_none());
    assert!(!doc.settings_json.contains("cloud_workers"));
    assert!(!doc.settings_json.contains("aws-access-key"));
    assert!(!doc.settings_json.contains("aws-secret-key"));
    assert!(!secret_path.exists());
    assert_secret_absent_from_sqlite_artifacts(&db_path, "aws-access-key").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "aws-secret-key").await;
}

#[tokio::test]
async fn load_settings_removes_empty_legacy_cloud_worker_secret_fields() {
    let _env = clean_execution_env().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let settings = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: String::new(),
                api_secret: None,
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        ..Settings::default()
    };
    let settings_json = serde_json::to_string_pretty(&settings).unwrap();
    let legacy_secret_json = serde_json::to_string_pretty(&json!({
        "version": RUNTIME_SETTINGS_SECRET_VERSION,
        "dictation_livekit_api_key": "lk-key",
        "dictation_livekit_api_secret": "lk-secret",
        "title_generation_remote_api_key": "",
        "oracle_api_key": "",
        "aws_cloud_workers_access_key_id": "",
        "aws_cloud_workers_secret_access_key": ""
    }))
    .unwrap();
    let original_doc = store
        .upsert_runtime_settings_document_with_secrets(1, &settings_json, &legacy_secret_json)
        .await
        .unwrap();
    let original_secret_ref = original_doc
        .secret_ref
        .expect("runtime settings secret_ref");
    let original_secret_path =
        runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &original_secret_ref);
    assert!(original_secret_path.exists());

    let loaded = load_settings(&store).await.unwrap();
    let livekit = loaded
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref())
        .unwrap();
    assert_eq!(livekit.api_key, "lk-key");
    assert_eq!(livekit.api_secret.as_deref(), Some("lk-secret"));

    let doc = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document");
    let secret_ref = doc.secret_ref.expect("runtime settings secret_ref");
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    let secret_payload = tokio::fs::read_to_string(&secret_path).await.unwrap();
    assert!(secret_payload.contains("lk-key"));
    assert!(secret_payload.contains("lk-secret"));
    assert!(!secret_payload.contains("aws_cloud_workers_access_key_id"));
    assert!(!secret_payload.contains("aws_cloud_workers_secret_access_key"));
    assert!(!doc.settings_json.contains("lk-key"));
    assert!(!doc.settings_json.contains("lk-secret"));
    assert_secret_absent_from_sqlite_artifacts(&db_path, "lk-key").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "lk-secret").await;
}

#[tokio::test]
async fn load_settings_fails_closed_on_corrupt_runtime_setting_secret_sidecar() {
    let _env = clean_execution_env().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let settings = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: "lk-key".to_string(),
                api_secret: Some("lk-secret".to_string()),
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        ..Settings::default()
    };
    save_settings(&store, &settings).await.unwrap();
    let secret_ref = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document")
        .secret_ref
        .expect("runtime settings secret_ref");
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    tokio::fs::write(&secret_path, b"{not valid json")
        .await
        .unwrap();

    let err = load_settings(&store).await.unwrap_err();
    assert!(err
        .to_string()
        .contains("parsing runtime settings secret envelope"));
}

#[tokio::test]
async fn save_settings_without_runtime_secrets_clears_secret_ref_and_sidecar() {
    let _env = clean_execution_env().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let secret_settings = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: "lk-key".to_string(),
                api_secret: Some("lk-secret".to_string()),
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        ..Settings::default()
    };
    save_settings(&store, &secret_settings).await.unwrap();
    let secret_ref = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document")
        .secret_ref
        .expect("runtime settings secret_ref");
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    assert!(secret_path.exists());

    save_settings(&store, &Settings::default()).await.unwrap();

    let doc = store
        .get_runtime_settings_document()
        .await
        .unwrap()
        .expect("runtime settings document");
    assert!(doc.secret_ref.is_none());
    assert!(!secret_path.exists());

    let loaded = load_settings(&store).await.unwrap();
    let livekit = loaded
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref());
    assert!(livekit
        .is_none_or(|livekit| { livekit.api_key.is_empty() && livekit.api_secret.is_none() }));
}

#[test]
fn apply_update_preserves_existing_secret_values_when_omitted() {
    let current = Settings {
        dictation: Some(DictationSettings {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings {
                base_url: "https://livekit.example".to_string(),
                api_key: "lk-key".to_string(),
                api_secret: Some("lk-secret".to_string()),
                model: "auto".to_string(),
                language: "en".to_string(),
            }),
        }),
        title_generation: Some(TitleGenerationSettings {
            mode: TitleGenerationMode::Remote,
            remote: TitleGenerationRemoteSettings {
                base_url: "https://titles.example".to_string(),
                api_key: "title-key".to_string(),
                model: "gpt-test".to_string(),
                use_json: false,
            },
            local: TitleGenerationLocalSettings::default(),
        }),
        ..Settings::default()
    };

    let next = apply_update(
        current,
        UpdateSettingsReq {
            dictation: Some(UpdateDictationSettingsReq {
                enabled: true,
                provider: DictationProvider::LiveKitInference,
                livekit: Some(UpdateLiveKitDictationSettingsReq {
                    base_url: "https://livekit.next".to_string(),
                    api_key: None,
                    api_secret: None,
                    model: "new-model".to_string(),
                    language: "es".to_string(),
                }),
            }),
            title_generation: Some(UpdateTitleGenerationSettingsReq {
                mode: TitleGenerationMode::Remote,
                remote: UpdateTitleGenerationRemoteSettingsReq {
                    base_url: "https://titles.next".to_string(),
                    api_key: None,
                    model: "gpt-next".to_string(),
                    use_json: true,
                },
                local: TitleGenerationLocalSettings::default(),
            }),
            oracle: None,
            telemetry: None,
            resource_governance: None,
            provider_guard: None,
            tool_limits: None,
            provider_restart: None,
            subagents: None,
            sandboxing: None,
            execution: None,
            network_profiles: None,
        },
    );

    let livekit = next
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref())
        .expect("dictation livekit");
    assert_eq!(livekit.api_key, "lk-key");
    assert_eq!(livekit.api_secret.as_deref(), Some("lk-secret"));
    assert_eq!(livekit.base_url, "https://livekit.next");
    assert_eq!(livekit.model, "new-model");
    assert_eq!(livekit.language, "es");

    let title_remote = &next
        .title_generation
        .as_ref()
        .expect("title generation")
        .remote;
    assert_eq!(title_remote.api_key, "title-key");
    assert_eq!(title_remote.base_url, "https://titles.next");
    assert_eq!(title_remote.model, "gpt-next");
    assert!(title_remote.use_json);
}

#[test]
fn apply_update_replaces_container_machine_settings() {
    let current = Settings {
        execution: Some(ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings::default(),
        }),
        ..Settings::default()
    };

    let next = apply_update(
        current,
        UpdateSettingsReq {
            dictation: None,
            title_generation: None,
            oracle: None,
            telemetry: None,
            resource_governance: None,
            provider_guard: None,
            tool_limits: None,
            provider_restart: None,
            subagents: None,
            sandboxing: None,
            execution: Some(update::UpdateExecutionSettingsReq {
                mode: ExecutionMode::Sandbox,
                container: update::UpdateContainerExecutionSettingsReq {
                    network_mode: ContainerExecutionSettings::default().network_mode,
                    allowlist: ContainerExecutionSettings::default().allowlist,
                    image: ContainerExecutionSettings::default().image,
                    machine: ContainerMachineSettings {
                        memory_profile: ContainerMachineMemoryProfile::Custom,
                        custom_memory_mb: Some(6144),
                        idle_shutdown_seconds: 90,
                        host_pressure_swap_threshold_mb: 256,
                    },
                },
            }),
            network_profiles: None,
        },
    );

    let machine = &next
        .execution
        .as_ref()
        .expect("execution settings")
        .container
        .machine;
    assert_eq!(
        machine.memory_profile,
        ContainerMachineMemoryProfile::Custom
    );
    assert_eq!(machine.custom_memory_mb, Some(6144));
    assert_eq!(machine.idle_shutdown_seconds, 90);
    assert_eq!(machine.host_pressure_swap_threshold_mb, 256);
}

#[test]
fn apply_update_clamps_container_machine_idle_shutdown_seconds() {
    let next = apply_update(
        Settings::default(),
        UpdateSettingsReq {
            dictation: None,
            title_generation: None,
            oracle: None,
            telemetry: None,
            resource_governance: None,
            provider_guard: None,
            tool_limits: None,
            provider_restart: None,
            subagents: None,
            sandboxing: None,
            execution: Some(update::UpdateExecutionSettingsReq {
                mode: ExecutionMode::Sandbox,
                container: update::UpdateContainerExecutionSettingsReq {
                    network_mode: ContainerExecutionSettings::default().network_mode,
                    allowlist: ContainerExecutionSettings::default().allowlist,
                    image: ContainerExecutionSettings::default().image,
                    machine: ContainerMachineSettings {
                        idle_shutdown_seconds: 5,
                        ..ContainerMachineSettings::default()
                    },
                },
            }),
            network_profiles: None,
        },
    );

    let machine = &next
        .execution
        .as_ref()
        .expect("execution settings")
        .container
        .machine;
    assert_eq!(machine.idle_shutdown_seconds, 60);
}

#[test]
fn container_machine_settings_deserialize_clamps_idle_shutdown_seconds() {
    let parsed: ContainerMachineSettings = serde_json::from_value(json!({
        "memory_profile": "economy",
        "idle_shutdown_seconds": 15,
        "host_pressure_swap_threshold_mb": 512
    }))
    .expect("deserialize machine settings");

    assert_eq!(
        parsed.idle_shutdown_seconds,
        MIN_CONTAINER_MACHINE_IDLE_SHUTDOWN_SECONDS
    );
}

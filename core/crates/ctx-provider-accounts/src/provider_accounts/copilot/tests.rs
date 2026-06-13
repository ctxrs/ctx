use super::*;

fn assert_unsafe_account_id_error(err: anyhow::Error) {
    assert!(
        err.to_string().contains("single path segment"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn remove_copilot_account_rejects_unsafe_account_id() {
    let dir = tempfile::tempdir().unwrap();
    let err = remove_copilot_account(dir.path(), "..").await.unwrap_err();
    assert_unsafe_account_id_error(err);
}

#[tokio::test]
async fn copilot_active_account_projects_token_env() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_copilot_account(
        root,
        Some("Copilot Test".to_string()),
        "ghp_abc".to_string(),
        Some("dev@example.com".to_string()),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let env = copilot_env_for_active_account(root).await.unwrap();
    let home = PathBuf::from(env.get("HOME").expect("HOME"));
    assert_eq!(env.get("GH_TOKEN"), Some(&"ghp_abc".to_string()));
    assert_eq!(env.get("GITHUB_TOKEN"), Some(&"ghp_abc".to_string()));
    assert_eq!(
        env.get("COPILOT_GITHUB_TOKEN"),
        Some(&"ghp_abc".to_string())
    );
    assert_eq!(
        env.get("COPILOT_MODEL"),
        Some(&COPILOT_BOOTSTRAP_MODEL_ID.to_string())
    );
    assert!(home.starts_with(root));
    assert_eq!(
        env.get("XDG_CONFIG_HOME"),
        Some(&home.join(".config").to_string_lossy().to_string())
    );
    assert_eq!(
        env.get("XDG_STATE_HOME"),
        Some(
            &home
                .join(".local")
                .join("state")
                .to_string_lossy()
                .to_string()
        )
    );
    assert!(copilot_account_dir(root, &active_id).exists());
}

#[tokio::test]
async fn copilot_active_account_projects_runtime_root_home() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime_root = runtime.path();
    let registry = add_copilot_account(
        root,
        Some("Copilot Test".to_string()),
        "ghp_runtime".to_string(),
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let env = copilot_env_for_active_account_with_runtime_root(root, runtime_root)
        .await
        .unwrap();
    let home = PathBuf::from(env.get("HOME").expect("HOME"));
    assert!(home.starts_with(runtime_root));
    assert!(home.ends_with(active_id));
}

#[test]
fn copilot_model_catalog_returns_pinned_models_for_known_version() {
    let value = copilot_models_value_for_version("1.0.0").expect("catalog");
    let current = value
        .get("current_model_id")
        .and_then(serde_json::Value::as_str);
    let default = value
        .get("default_model_id")
        .and_then(serde_json::Value::as_str);
    let catalog_version = value
        .get("catalog_version")
        .and_then(serde_json::Value::as_str);
    let models = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .expect("models array");
    assert_eq!(current, Some(COPILOT_BOOTSTRAP_MODEL_ID));
    assert_eq!(default, Some(COPILOT_DEFAULT_MODEL_ID));
    assert_eq!(catalog_version, Some(COPILOT_CATALOG_VERSION_1_0_0));
    assert!(models.iter().any(|model| {
        model.get("id").and_then(serde_json::Value::as_str) == Some("claude-sonnet-4.6")
    }));
    assert!(models.iter().any(|model| {
        model.get("id").and_then(serde_json::Value::as_str) == Some("gpt-5-mini")
    }));
}

#[test]
fn copilot_model_catalog_aliases_host_cli_version() {
    let value = copilot_models_value_for_version("1.0.3.").expect("catalog");
    let catalog_version = value
        .get("catalog_version")
        .and_then(serde_json::Value::as_str);
    assert_eq!(catalog_version, Some(COPILOT_CATALOG_VERSION_1_0_3));
    let models = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .expect("models array");
    assert!(models.iter().any(|model| {
        model.get("id").and_then(serde_json::Value::as_str) == Some("claude-opus-4.6-fast")
    }));
}

#[tokio::test]
async fn adding_existing_copilot_account_updates_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let first = add_copilot_account(
        root,
        Some("Copilot Initial".to_string()),
        "ghp_abc".to_string(),
        Some("initial@example.com".to_string()),
    )
    .await
    .unwrap();
    let first_id = first.active_account_id.clone().expect("active account");

    let second = add_copilot_account(
        root,
        Some("Copilot Updated".to_string()),
        "ghp_abc".to_string(),
        Some("updated@example.com".to_string()),
    )
    .await
    .unwrap();

    assert_eq!(second.accounts.len(), 1);
    assert_eq!(second.active_account_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(second.accounts[0].label, "Copilot Updated");
    assert_eq!(
        second.accounts[0].email.as_deref(),
        Some("updated@example.com")
    );
}

#[tokio::test]
async fn adding_existing_copilot_account_fails_closed_on_malformed_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let secret_ref = "acct-1.json";
    save_copilot_registry(
        root,
        &CopilotAccountRegistry {
            active_account_id: Some("acct-1".to_string()),
            accounts: vec![CopilotAccountEntry {
                id: "acct-1".to_string(),
                label: "Existing".to_string(),
                kind: COPILOT_CREDENTIAL_KIND_GH_TOKEN.to_string(),
                email: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(secret_ref.to_string()),
            }],
        },
    )
    .await
    .unwrap();
    let secret_path = copilot_secret_path(root, secret_ref).unwrap();
    tokio::fs::create_dir_all(secret_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&secret_path, "{ invalid json")
        .await
        .unwrap();

    let err = add_copilot_account(
        root,
        Some("Copilot Updated".to_string()),
        "ghp_abc".to_string(),
        None,
    )
    .await
    .expect_err("malformed existing copilot secret should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid copilot secret"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1.json"),
        "expected secret path in error: {message}"
    );
}

#[tokio::test]
async fn deleting_active_copilot_account_clears_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_copilot_account(
        root,
        Some("Copilot Test".to_string()),
        "ghp_abc".to_string(),
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let _ = remove_copilot_account(root, &active_id).await.unwrap();
    let env = copilot_env_for_active_account(root).await.unwrap();
    assert!(env.is_empty());
}

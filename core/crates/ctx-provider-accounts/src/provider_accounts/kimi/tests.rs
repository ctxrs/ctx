use super::*;

fn assert_unsafe_account_id_error(err: anyhow::Error) {
    assert!(
        err.to_string().contains("single path segment"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn remove_kimi_account_rejects_unsafe_account_id() {
    let dir = tempfile::tempdir().unwrap();
    let err = remove_kimi_account(dir.path(), "..").await.unwrap_err();
    assert_unsafe_account_id_error(err);
}

#[tokio::test]
async fn kimi_active_account_projects_share_dir_and_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_kimi_account(
        root,
        Some("Kimi Test".to_string()),
        Some("moonshot".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        Some("dev@example.com".to_string()),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let env = kimi_env_for_active_account(root).await.unwrap();
    let share_dir = env
        .get(KIMI_SHARE_DIR_ENV)
        .expect("KIMI_SHARE_DIR should be set");
    assert!(share_dir.contains(&active_id));
    let canonical_credentials_path = Path::new(share_dir)
        .join("credentials")
        .join("kimi-code.json");
    assert!(canonical_credentials_path.exists());
    let legacy_credentials_path = Path::new(share_dir)
        .join("credentials")
        .join("moonshot.json");
    assert!(legacy_credentials_path.exists());
    let config_path = Path::new(share_dir).join("config.toml");
    assert!(config_path.exists());
    let config = tokio::fs::read_to_string(&config_path)
        .await
        .expect("read config.toml");
    assert!(config.contains("default_model = \"kimi-code/kimi-for-coding\""));
    assert!(config.contains("key = \"oauth/kimi-code\""));
}

#[tokio::test]
async fn adding_existing_kimi_account_updates_metadata_and_config_toml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let credentials = r#"{"access_token":"token-a","refresh_token":"token-r"}"#;

    let first = add_kimi_account(
        root,
        Some("Kimi Initial".to_string()),
        Some("moonshot".to_string()),
        credentials.to_string(),
        Some("model = \"k1\"".to_string()),
        Some("initial@example.com".to_string()),
    )
    .await
    .unwrap();
    let first_id = first.active_account_id.clone().expect("active account");

    let second = add_kimi_account(
        root,
        Some("Kimi Updated".to_string()),
        Some("moonshot".to_string()),
        credentials.to_string(),
        Some("model = \"k2\"".to_string()),
        Some("updated@example.com".to_string()),
    )
    .await
    .unwrap();

    assert_eq!(second.accounts.len(), 1);
    assert_eq!(second.active_account_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(second.accounts[0].label, "Kimi Updated");
    assert_eq!(
        second.accounts[0].email.as_deref(),
        Some("updated@example.com")
    );

    let secret_ref = second.accounts[0]
        .secret_ref
        .as_deref()
        .expect("secret ref should be set");
    let secret = read_kimi_secret_for_ref(root, secret_ref).await.unwrap();
    assert_eq!(secret.config_toml.as_deref(), Some("model = \"k2\""));
}

#[tokio::test]
async fn adding_existing_kimi_account_fails_closed_on_malformed_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let secret_ref = "acct-1.json";
    save_kimi_registry(
        root,
        &KimiAccountRegistry {
            active_account_id: Some("acct-1".to_string()),
            accounts: vec![KimiAccountEntry {
                id: "acct-1".to_string(),
                label: "Existing".to_string(),
                kind: KIMI_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(secret_ref.to_string()),
            }],
        },
    )
    .await
    .unwrap();
    let secret_path = kimi_secret_path(root, secret_ref).unwrap();
    tokio::fs::create_dir_all(secret_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&secret_path, "{ invalid json")
        .await
        .unwrap();

    let err = add_kimi_account(
        root,
        Some("Kimi Updated".to_string()),
        None,
        r#"{"access_token":"token-a"}"#.to_string(),
        None,
        None,
    )
    .await
    .expect_err("malformed existing kimi secret should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid kimi secret"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1.json"),
        "expected secret path in error: {message}"
    );
}

#[tokio::test]
async fn deleting_active_kimi_account_clears_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_kimi_account(
        root,
        Some("Kimi Test".to_string()),
        None,
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let _ = remove_kimi_account(root, &active_id).await.unwrap();
    let env = kimi_env_for_active_account(root).await.unwrap();
    assert!(env.is_empty());
}

#[tokio::test]
async fn deleting_active_kimi_account_removes_runtime_root_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime_root = root
        .join("containers")
        .join("workspaces")
        .join("workspace-kimi")
        .join("data");
    tokio::fs::create_dir_all(&runtime_root).await.unwrap();

    let registry = add_kimi_account(
        root,
        Some("Kimi Test".to_string()),
        None,
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let projected_home = kimi_account_home(&runtime_root, &active_id);

    let _ = kimi_env_for_active_account_with_runtime_root(root, &runtime_root)
        .await
        .unwrap();
    assert!(projected_home.exists());

    let _ = remove_kimi_account(root, &active_id).await.unwrap();
    assert!(!projected_home.exists());
}

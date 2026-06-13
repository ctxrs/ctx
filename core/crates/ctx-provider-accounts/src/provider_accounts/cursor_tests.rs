use super::*;

fn assert_unsafe_account_id_error(err: anyhow::Error) {
    assert!(
        err.to_string().contains("single path segment"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn remove_cursor_account_rejects_unsafe_account_id() {
    let dir = tempfile::tempdir().unwrap();
    let err = remove_cursor_account(dir.path(), "..").await.unwrap_err();
    assert_unsafe_account_id_error(err);
}

#[tokio::test]
async fn cursor_active_account_projects_config_dir_and_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_cursor_account(
        root,
        Some("Cursor Test".to_string()),
        "cursor-key".to_string(),
        Some("dev@example.com".to_string()),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let env = cursor_env_for_active_account(root).await.unwrap();
    assert_eq!(env.get("CURSOR_API_KEY"), Some(&"cursor-key".to_string()));
    assert!(!env.contains_key("CURSOR_AUTH_TOKEN"));
    let config_dir = env
        .get("CURSOR_CONFIG_DIR")
        .expect("CURSOR_CONFIG_DIR should be set");
    assert!(config_dir.contains(&active_id));
    assert!(cursor_account_home(root, &active_id).exists());
}

#[tokio::test]
async fn cursor_active_oauth_account_projects_config_dir_and_auth_token() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_cursor_oauth_account(
        root,
        Some("Cursor OAuth".to_string()),
        "cursor-access-token".to_string(),
        Some("cursor-refresh-token".to_string()),
        Some("oauth@example.com".to_string()),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let env = cursor_env_for_active_account(root).await.unwrap();
    assert_eq!(
        env.get("CURSOR_AUTH_TOKEN"),
        Some(&"cursor-access-token".to_string())
    );
    assert!(!env.contains_key("CURSOR_API_KEY"));
    let config_dir = env
        .get("CURSOR_CONFIG_DIR")
        .expect("CURSOR_CONFIG_DIR should be set");
    assert!(config_dir.contains(&active_id));
    assert!(cursor_account_home(root, &active_id).exists());
}

#[tokio::test]
async fn adding_existing_cursor_account_updates_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let first = add_cursor_account(
        root,
        Some("Cursor Initial".to_string()),
        "cursor-key".to_string(),
        Some("initial@example.com".to_string()),
    )
    .await
    .unwrap();
    let first_id = first.active_account_id.clone().expect("active account");

    let second = add_cursor_account(
        root,
        Some("Cursor Updated".to_string()),
        "cursor-key".to_string(),
        Some("updated@example.com".to_string()),
    )
    .await
    .unwrap();

    assert_eq!(second.accounts.len(), 1);
    assert_eq!(second.active_account_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(second.accounts[0].label, "Cursor Updated");
    assert_eq!(
        second.accounts[0].email.as_deref(),
        Some("updated@example.com")
    );
}

#[tokio::test]
async fn adding_existing_cursor_account_fails_closed_on_malformed_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let secret_ref = "acct-1.json";
    save_cursor_registry(
        root,
        &CursorAccountRegistry {
            active_account_id: Some("acct-1".to_string()),
            accounts: vec![CursorAccountEntry {
                id: "acct-1".to_string(),
                label: "Existing".to_string(),
                kind: CURSOR_CREDENTIAL_KIND_API_KEY.to_string(),
                email: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(secret_ref.to_string()),
            }],
        },
    )
    .await
    .unwrap();
    let secret_path = cursor_secret_path(root, secret_ref).unwrap();
    tokio::fs::create_dir_all(secret_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&secret_path, "{ invalid json")
        .await
        .unwrap();

    let err = add_cursor_account(
        root,
        Some("Cursor Updated".to_string()),
        "cursor-key".to_string(),
        None,
    )
    .await
    .expect_err("malformed existing cursor secret should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid cursor secret"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1.json"),
        "expected secret path in error: {message}"
    );
}

#[tokio::test]
async fn deleting_active_cursor_account_clears_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_cursor_account(
        root,
        Some("Cursor Test".to_string()),
        "cursor-key".to_string(),
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let _ = remove_cursor_account(root, &active_id).await.unwrap();
    let env = cursor_env_for_active_account(root).await.unwrap();
    assert!(env.is_empty());
}

#[tokio::test]
async fn deleting_active_cursor_account_removes_runtime_root_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime_root = root
        .join("containers")
        .join("workspaces")
        .join("workspace-cursor")
        .join("data");
    tokio::fs::create_dir_all(&runtime_root).await.unwrap();

    let registry = add_cursor_account(
        root,
        Some("Cursor Test".to_string()),
        "cursor-key".to_string(),
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let projected_home = cursor_account_home(&runtime_root, &active_id);

    let _ = cursor_env_for_active_account_with_runtime_root(root, &runtime_root)
        .await
        .unwrap();
    assert!(projected_home.exists());

    let _ = remove_cursor_account(root, &active_id).await.unwrap();
    assert!(!projected_home.exists());
}

use super::*;

fn assert_unsafe_account_id_error(err: anyhow::Error) {
    assert!(
        err.to_string().contains("single path segment"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn remove_gemini_account_rejects_unsafe_account_id() {
    let dir = tempfile::tempdir().unwrap();
    let err = remove_gemini_account(dir.path(), "..").await.unwrap_err();
    assert_unsafe_account_id_error(err);
}

#[tokio::test]
async fn gemini_active_account_projects_home_and_auth_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let oauth_creds =
        r#"{"access_token":"token-a","refresh_token":"token-r","token_type":"Bearer"}"#;
    let google_accounts = r#"[{"email":"dev@example.com"}]"#;
    let registry = add_gemini_account(
        root,
        Some("Gemini Test".to_string()),
        oauth_creds.to_string(),
        Some(google_accounts.to_string()),
        Some("dev@example.com".to_string()),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let env = gemini_env_for_active_account(root).await.unwrap();
    let home = env
        .get("GEMINI_CLI_HOME")
        .expect("GEMINI_CLI_HOME should be set");
    assert!(home.contains(&active_id));
    assert_eq!(
        env.get(GEMINI_FORCE_FILE_STORAGE_ENV),
        Some(&"true".to_string())
    );
    let oauth_path = Path::new(home).join(".gemini").join("oauth_creds.json");
    assert!(oauth_path.exists());
    let settings_path = Path::new(home).join(".gemini").join("settings.json");
    let settings_payload = tokio::fs::read_to_string(settings_path).await.unwrap();
    assert!(settings_payload.contains(GEMINI_AUTH_SELECTED_TYPE_OAUTH_PERSONAL));
}

#[tokio::test]
async fn adding_existing_gemini_account_updates_metadata_and_google_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let oauth_creds = r#"{"access_token":"token-a","refresh_token":"token-r"}"#;

    let first = add_gemini_account(
        root,
        Some("Gemini Initial".to_string()),
        oauth_creds.to_string(),
        Some(r#"[{"email":"initial@example.com"}]"#.to_string()),
        Some("initial@example.com".to_string()),
    )
    .await
    .unwrap();
    let first_id = first.active_account_id.clone().expect("active account");

    let second = add_gemini_account(
        root,
        Some("Gemini Updated".to_string()),
        oauth_creds.to_string(),
        Some(r#"[{"email":"updated@example.com"}]"#.to_string()),
        Some("updated@example.com".to_string()),
    )
    .await
    .unwrap();

    assert_eq!(second.accounts.len(), 1);
    assert_eq!(second.active_account_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(second.accounts[0].label, "Gemini Updated");
    assert_eq!(
        second.accounts[0].email.as_deref(),
        Some("updated@example.com")
    );

    let secret_ref = second.accounts[0]
        .secret_ref
        .as_deref()
        .expect("secret ref should be set");
    let secret = read_gemini_secret_for_ref(root, secret_ref).await.unwrap();
    assert_eq!(
        secret.google_accounts,
        Some(serde_json::json!([{"email":"updated@example.com"}]))
    );
}

#[tokio::test]
async fn deleting_active_gemini_account_clears_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_gemini_account(
        root,
        Some("Gemini Test".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let _ = remove_gemini_account(root, &active_id).await.unwrap();
    let env = gemini_env_for_active_account(root).await.unwrap();
    assert!(env.is_empty());
}

#[tokio::test]
async fn deleting_active_gemini_account_removes_runtime_root_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime_root = root
        .join("containers")
        .join("workspaces")
        .join("workspace-gemini")
        .join("data");
    tokio::fs::create_dir_all(&runtime_root).await.unwrap();

    let registry = add_gemini_account(
        root,
        Some("Gemini Test".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let projected_home = gemini_account_home(&runtime_root, &active_id);

    let _ = gemini_env_for_active_account_with_runtime_root(root, &runtime_root)
        .await
        .unwrap();
    assert!(projected_home.exists());

    let _ = remove_gemini_account(root, &active_id).await.unwrap();
    assert!(!projected_home.exists());
}

#[tokio::test]
async fn adding_existing_gemini_account_fails_closed_on_malformed_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let secret_ref = "acct-1.json";
    save_gemini_registry(
        root,
        &GeminiAccountRegistry {
            active_account_id: Some("acct-1".to_string()),
            accounts: vec![GeminiAccountEntry {
                id: "acct-1".to_string(),
                label: "Existing".to_string(),
                kind: GEMINI_CREDENTIAL_KIND_OAUTH_PERSONAL.to_string(),
                email: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(secret_ref.to_string()),
            }],
        },
    )
    .await
    .unwrap();
    let secret_path = gemini_secret_path(root, secret_ref).unwrap();
    tokio::fs::create_dir_all(secret_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&secret_path, "{ invalid json")
        .await
        .unwrap();

    let err = add_gemini_account(
        root,
        Some("Gemini Updated".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .expect_err("malformed existing gemini secret should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid gemini secret"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1.json"),
        "expected secret path in error: {message}"
    );
}

use super::*;
use crate::provider_accounts::CLAUDE_CREDENTIAL_KIND_SETUP_TOKEN;

const CLAUDE_TEST_SETUP_TOKEN: &str =
    "sk-ant-oat01-abcDEF1234567890_abcdefghijklmnopqrstuvwxyz_0123456789";

fn assert_unsafe_account_id_error(err: anyhow::Error) {
    assert!(
        err.to_string().contains("single path segment"),
        "unexpected error: {err}"
    );
}

#[test]
fn normalize_claude_setup_token_accepts_wrapped_and_quoted_values() {
    let wrapped = format!("  \"{CLAUDE_TEST_SETUP_TOKEN}\"  ");
    let normalized = normalize_claude_setup_token(&wrapped).expect("normalize token");
    assert_eq!(normalized, CLAUDE_TEST_SETUP_TOKEN);

    let line_wrapped = "sk-ant-oat01-abcDEF1234567890_\nabcdefghijklmnopqrstuvwxyz_0123456789";
    let normalized = normalize_claude_setup_token(line_wrapped).expect("normalize wrapped token");
    assert_eq!(normalized, CLAUDE_TEST_SETUP_TOKEN);
}

#[test]
fn normalize_claude_setup_token_rejects_callback_codes() {
    let err = normalize_claude_setup_token("ePBMdWetJlSbZ0a#state")
        .expect_err("callback token should fail");
    assert!(err.to_string().contains("browser callback code"));
}

#[test]
fn normalize_claude_setup_token_requires_setup_token_prefix() {
    let err = normalize_claude_setup_token("token-abc").expect_err("invalid token should fail");
    assert!(err.to_string().contains("must start with sk-ant-oat"));
}

#[tokio::test]
async fn remove_claude_account_rejects_unsafe_account_id() {
    let dir = tempfile::tempdir().unwrap();
    let err = remove_claude_account(dir.path(), "..").await.unwrap_err();
    assert_unsafe_account_id_error(err);
}

#[tokio::test]
async fn claude_active_account_projects_token_to_env() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_claude_account(
        root,
        Some("Claude Test".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let env = claude_env_for_active_account(root).await.unwrap();
    assert_eq!(
        env.get("CLAUDE_CODE_OAUTH_TOKEN"),
        Some(&CLAUDE_TEST_SETUP_TOKEN.to_string())
    );
    let cfg_dir = env
        .get(CLAUDE_CONFIG_DIR_ENV_KEY)
        .expect("CLAUDE_CONFIG_DIR should be set");
    assert!(cfg_dir.contains(&active_id));
    let shim_dir = PathBuf::from(cfg_dir).join(CLAUDE_SECURITY_SHIM_DIRNAME);
    let shim_path = shim_dir.join(CLAUDE_SECURITY_SHIM_FILENAME);
    let path_env = env.get("PATH").expect("PATH should be set");
    assert!(shim_path.exists());
    assert!(
        path_env.starts_with(&shim_dir.to_string_lossy().to_string()),
        "expected PATH to start with shim dir, got {path_env}"
    );
}

#[tokio::test]
async fn adding_existing_claude_account_updates_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let first = add_claude_account(
        root,
        Some("Claude Initial".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();
    let first_id = first.active_account_id.clone().expect("active account");

    let second = add_claude_account(
        root,
        Some("Claude Updated".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();

    assert_eq!(second.accounts.len(), 1);
    assert_eq!(second.active_account_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(second.accounts[0].label, "Claude Updated");
}

#[tokio::test]
async fn adding_existing_claude_account_fails_closed_on_malformed_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let secret_ref = "acct-1.json";
    save_claude_registry(
        root,
        &ClaudeAccountRegistry {
            active_account_id: Some("acct-1".to_string()),
            accounts: vec![ClaudeAccountEntry {
                id: "acct-1".to_string(),
                label: "Existing".to_string(),
                kind: CLAUDE_CREDENTIAL_KIND_SETUP_TOKEN.to_string(),
                email: None,
                subscription_type: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(secret_ref.to_string()),
            }],
        },
    )
    .await
    .unwrap();
    let secret_path = claude_secret_path(root, secret_ref).unwrap();
    tokio::fs::create_dir_all(secret_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&secret_path, "{ invalid json")
        .await
        .unwrap();

    let err = add_claude_account(
        root,
        Some("Claude Updated".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .expect_err("malformed existing claude secret should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid claude secret"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1.json"),
        "expected secret path in error: {message}"
    );
}

#[tokio::test]
async fn deleting_active_claude_account_clears_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = add_claude_account(
        root,
        Some("Claude Test".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");

    let _ = remove_claude_account(root, &active_id).await.unwrap();
    let env = claude_env_for_active_account(root).await.unwrap();
    assert!(env.is_empty());
}

#[tokio::test]
async fn deleting_active_claude_account_removes_runtime_root_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime_root = root
        .join("containers")
        .join("workspaces")
        .join("workspace-claude")
        .join("data");
    tokio::fs::create_dir_all(&runtime_root).await.unwrap();

    let registry = add_claude_account(
        root,
        Some("Claude Test".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();
    let active_id = registry.active_account_id.clone().expect("active account");
    let projected_dir = claude_account_dir(&runtime_root, &active_id);

    let _ = claude_env_for_active_account_with_runtime_root(root, &runtime_root)
        .await
        .unwrap();
    assert!(projected_dir.exists());

    let _ = remove_claude_account(root, &active_id).await.unwrap();
    assert!(!projected_dir.exists());
}

#[tokio::test]
async fn load_claude_registry_prunes_legacy_non_setup_token_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let legacy_id = "legacy-claude-oauth";
    let setup_id = "setup-claude";
    let legacy_secret_ref = format!("{legacy_id}.json");
    let setup_secret_ref = write_claude_secret_for_account(root, setup_id, CLAUDE_TEST_SETUP_TOKEN)
        .await
        .expect("write setup secret");
    let legacy_secret_path = claude_secret_path(root, &legacy_secret_ref).unwrap();
    write_secure_file_atomic(
        &legacy_secret_path,
        br#"{"version":1,"anthropic_auth_token":"sk-ant-oat01-legacy1234567890_abcdefghijklmnopqrstuvwxyz_0123456789"}"#,
    )
    .await
    .expect("write legacy secret");
    let legacy_account_dir = claude_account_dir(root, legacy_id);
    tokio::fs::create_dir_all(&legacy_account_dir)
        .await
        .expect("create legacy account dir");

    let registry = ClaudeAccountRegistry {
        active_account_id: Some(legacy_id.to_string()),
        accounts: vec![
            ClaudeAccountEntry {
                id: legacy_id.to_string(),
                label: "Legacy OAuth".to_string(),
                kind: "claude-ai-oauth".to_string(),
                email: Some("legacy@example.com".to_string()),
                subscription_type: Some("pro".to_string()),
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(legacy_secret_ref.clone()),
            },
            ClaudeAccountEntry {
                id: setup_id.to_string(),
                label: "Setup Token".to_string(),
                kind: CLAUDE_CREDENTIAL_KIND_SETUP_TOKEN.to_string(),
                email: None,
                subscription_type: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: Some(setup_secret_ref),
            },
        ],
    };
    save_claude_registry(root, &registry)
        .await
        .expect("save registry");

    let loaded = load_claude_registry(root).await.unwrap();
    assert_eq!(loaded.accounts.len(), 1);
    assert_eq!(loaded.accounts[0].id, setup_id);
    assert_eq!(loaded.active_account_id, None);
    assert!(!legacy_secret_path.exists());
    assert!(!legacy_account_dir.exists());
}

#[tokio::test]
async fn load_claude_registry_prunes_legacy_accounts_even_with_unsafe_secret_ref() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside_secret = root.join("outside-secret.json");
    tokio::fs::write(&outside_secret, b"do-not-touch")
        .await
        .unwrap();

    let registry = ClaudeAccountRegistry {
        active_account_id: Some("legacy-claude-oauth".to_string()),
        accounts: vec![ClaudeAccountEntry {
            id: "legacy-claude-oauth".to_string(),
            label: "Legacy OAuth".to_string(),
            kind: "claude-ai-oauth".to_string(),
            email: Some("legacy@example.com".to_string()),
            subscription_type: Some("pro".to_string()),
            created_at: Utc::now(),
            last_used_at: Some(Utc::now()),
            secret_ref: Some("../outside-secret.json".to_string()),
        }],
    };
    save_claude_registry(root, &registry).await.unwrap();

    let loaded = load_claude_registry(root).await.unwrap();
    assert!(loaded.accounts.is_empty());
    assert!(loaded.active_account_id.is_none());
    assert_eq!(
        tokio::fs::read_to_string(&outside_secret).await.unwrap(),
        "do-not-touch"
    );
}

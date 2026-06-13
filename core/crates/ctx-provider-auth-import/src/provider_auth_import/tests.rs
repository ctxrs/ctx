use super::*;
use base64::Engine;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.as_deref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn import_result_mutation_filter_treats_already_imported_as_mutation() {
    let already_imported = ProviderAuthImportResult {
        candidate_id: "cand-1".to_string(),
        provider_id: "claude-crp".to_string(),
        status: "already_imported".to_string(),
        profile_id: Some("acct-1".to_string()),
        message: Some("Matching credential already imported.".to_string()),
    };

    assert!(provider_auth_import_result_mutates_effective_auth(
        &already_imported
    ));
}

#[test]
fn import_result_mutation_filter_ignores_non_mutating_statuses() {
    let unsupported = ProviderAuthImportResult {
        candidate_id: "cand-2".to_string(),
        provider_id: "cursor".to_string(),
        status: "unsupported".to_string(),
        profile_id: None,
        message: Some("Unsupported in this flow.".to_string()),
    };
    let error = ProviderAuthImportResult {
        candidate_id: "cand-3".to_string(),
        provider_id: "codex".to_string(),
        status: "error".to_string(),
        profile_id: None,
        message: Some("failed".to_string()),
    };

    assert!(!provider_auth_import_result_mutates_effective_auth(
        &unsupported
    ));
    assert!(!provider_auth_import_result_mutates_effective_auth(&error));
}

async fn write_legacy_secret_material(
    data_root: &Path,
    profile_id: &str,
    source_path: &str,
    bytes: &[u8],
) {
    let payload = StoredSecretMaterial {
        kind: "auth_file".to_string(),
        source_path: source_path.to_string(),
        content_b64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
    };
    let path = imported_secret_path(data_root, profile_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(path, serde_json::to_vec_pretty(&payload).unwrap())
        .await
        .unwrap();
}

async fn write_invalid_legacy_secret_material(data_root: &Path, profile_id: &str) {
    let path = imported_secret_path(data_root, profile_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(path, "{ invalid json").await.unwrap();
}

fn test_roots(base: &Path) -> HostRoots {
    HostRoots {
        home: base.join("home"),
        xdg_config: base.join("home").join(".config"),
        xdg_data: base.join("home").join(".local").join("share"),
        codex_home: base.join("home").join(".codex"),
    }
}

#[tokio::test]
async fn host_roots_honors_auth_import_override_envs() {
    let _env_lock = lock_env().await;
    let dir = tempfile::tempdir().unwrap();
    let override_root = dir.path().join("override");
    let _home = EnvGuard::set(
        CTX_PROVIDER_AUTH_IMPORT_HOME_ENV,
        override_root.join("home").to_string_lossy().as_ref(),
    );
    let _config = EnvGuard::set(
        CTX_PROVIDER_AUTH_IMPORT_XDG_CONFIG_HOME_ENV,
        override_root.join("config").to_string_lossy().as_ref(),
    );
    let _data = EnvGuard::set(
        CTX_PROVIDER_AUTH_IMPORT_XDG_DATA_HOME_ENV,
        override_root.join("data").to_string_lossy().as_ref(),
    );
    let _codex = EnvGuard::set(
        CTX_PROVIDER_AUTH_IMPORT_CODEX_HOME_ENV,
        override_root.join("codex").to_string_lossy().as_ref(),
    );

    let roots = host_roots().unwrap();
    assert_eq!(roots.home, override_root.join("home"));
    assert_eq!(roots.xdg_config, override_root.join("config"));
    assert_eq!(roots.xdg_data, override_root.join("data"));
    assert_eq!(roots.codex_home, override_root.join("codex"));
}

#[test]
fn env_parser_extracts_keys() {
    let parsed = parse_env_file(
        r#"
            # comment
            OPENAI_API_KEY=sk-test
            OPENAI_BASE_URL=https://api.example.com/v1
            "#,
    );
    assert_eq!(parsed.get("OPENAI_API_KEY"), Some(&"sk-test".to_string()));
    assert_eq!(
        parsed.get("OPENAI_BASE_URL"),
        Some(&"https://api.example.com/v1".to_string())
    );
}

#[test]
fn summarize_env_does_not_fill_endpoint_with_auth_type() {
    let env = BTreeMap::from([("OPENAI_API_KEY".to_string(), "sk-test".to_string())]);
    let (_summary, endpoint) = summarize_env("qwen", &env);
    assert_eq!(endpoint, None);
}

#[tokio::test]
async fn legacy_migration_keeps_unmigrated_profiles_without_marker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = Utc::now();

    let codex_profile = ProviderImportedAuthProfile {
        id: "legacy-codex".to_string(),
        provider_id: "codex".to_string(),
        provider_label: "Codex".to_string(),
        label: "Legacy Codex".to_string(),
        account_identity: None,
        endpoint: None,
        auth_type: Some("subscription".to_string()),
        source_path: "/tmp/.codex/auth.json".to_string(),
        source_kind: "auth_file".to_string(),
        secret_fingerprint: "fp-codex".to_string(),
        imported_at: now,
        updated_at: now,
    };
    let unsupported_profile = ProviderImportedAuthProfile {
        id: "legacy-cursor".to_string(),
        provider_id: "cursor".to_string(),
        provider_label: "Cursor".to_string(),
        label: "Legacy Cursor".to_string(),
        account_identity: None,
        endpoint: None,
        auth_type: Some("subscription".to_string()),
        source_path: "/tmp/.cursor/auth.json".to_string(),
        source_kind: "auth_file".to_string(),
        secret_fingerprint: "fp-cursor".to_string(),
        imported_at: now,
        updated_at: now,
    };

    save_imported_registry(
        root,
        &ProviderImportedAuthRegistry {
            profiles: vec![codex_profile.clone(), unsupported_profile.clone()],
        },
    )
    .await
    .unwrap();

    write_legacy_secret_material(
        root,
        &codex_profile.id,
        &codex_profile.source_path,
        br#"{"OPENAI_API_KEY":"sk-legacy"}"#,
    )
    .await;
    write_legacy_secret_material(
        root,
        &unsupported_profile.id,
        &unsupported_profile.source_path,
        br#"{"token":"cursor-legacy"}"#,
    )
    .await;

    migrate_legacy_imported_profiles_once(root).await.unwrap();

    let registry = load_imported_registry(root).await.unwrap();
    assert_eq!(registry.profiles.len(), 1);
    assert_eq!(registry.profiles[0].id, unsupported_profile.id);

    assert!(tokio::fs::metadata(legacy_migration_marker_path(root))
        .await
        .is_err());
    assert!(
        tokio::fs::metadata(imported_secret_path(root, &codex_profile.id))
            .await
            .is_err()
    );
    assert!(
        tokio::fs::metadata(imported_secret_path(root, &unsupported_profile.id))
            .await
            .is_ok()
    );

    let codex_registry = provider_accounts::load_codex_registry(root).await.unwrap();
    assert_eq!(codex_registry.accounts.len(), 1);
}

#[tokio::test]
async fn load_imported_registry_fails_closed_on_malformed_registry_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = root
        .join("providers")
        .join("auth_import")
        .join("profiles.json");
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&path, "{ invalid json").await.unwrap();

    let err = load_imported_registry(root)
        .await
        .expect_err("malformed imported auth registry should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("parsing imported auth registry"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("profiles.json"),
        "expected registry path in error: {message}"
    );
}

#[tokio::test]
async fn list_provider_auth_profiles_fails_closed_on_malformed_legacy_secret_material() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = Utc::now();
    let profile = ProviderImportedAuthProfile {
        id: "legacy-codex".to_string(),
        provider_id: "codex".to_string(),
        provider_label: "Codex".to_string(),
        label: "Legacy Codex".to_string(),
        account_identity: None,
        endpoint: None,
        auth_type: Some("subscription".to_string()),
        source_path: "/tmp/.codex/auth.json".to_string(),
        source_kind: "auth_file".to_string(),
        secret_fingerprint: "fp-codex".to_string(),
        imported_at: now,
        updated_at: now,
    };
    save_imported_registry(
        root,
        &ProviderImportedAuthRegistry {
            profiles: vec![profile.clone()],
        },
    )
    .await
    .unwrap();
    write_invalid_legacy_secret_material(root, &profile.id).await;

    let err = list_provider_auth_profiles(root)
        .await
        .expect_err("malformed legacy secret material should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("parsing imported auth secret material"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains(&format!("{}.json", profile.id)),
        "expected secret material path in error: {message}"
    );
    assert!(tokio::fs::metadata(legacy_migration_marker_path(root))
        .await
        .is_err());
}

#[tokio::test]
async fn codex_import_dedupes_secret_backed_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-1";
    let auth_bytes = br#"{"OPENAI_API_KEY":"sk-test"}"#;
    let account_dir = provider_accounts::ensure_codex_account_dir(root, account_id)
        .await
        .unwrap();
    tokio::fs::write(account_dir.join("auth.json"), auth_bytes)
        .await
        .unwrap();
    provider_accounts::save_codex_registry(
        root,
        &provider_accounts::CodexAccountRegistry {
            active_account_id: Some(account_id.to_string()),
            accounts: vec![provider_accounts::CodexAccountEntry {
                id: account_id.to_string(),
                label: "Test".to_string(),
                kind: provider_accounts::CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
                email: None,
                provider_account_id: None,
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: None,
                endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
            }],
        },
    )
    .await
    .unwrap();
    provider_accounts::ingest_codex_account_auth_to_secret_store(root, account_id)
        .await
        .unwrap();
    tokio::fs::remove_file(account_dir.join("auth.json"))
        .await
        .unwrap();

    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "codex-candidate".to_string(),
            provider_id: "codex".to_string(),
            provider_label: "Codex".to_string(),
            kind: "json_file".to_string(),
            path: "/tmp/.codex/auth.json".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("subscription".to_string()),
            fingerprint: Some(sha256_hex(auth_bytes)),
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(auth_bytes.to_vec()),
        label: Some("Imported Codex profile".to_string()),
    };

    let result = import_codex_candidate(root, &material).await.unwrap();
    assert_eq!(result.status, "already_imported");
    assert_eq!(result.profile_id.as_deref(), Some(account_id));
    let profiles = list_provider_auth_profiles(root).await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, account_id);
    assert_eq!(profiles[0].provider_id, "codex");
}

#[tokio::test]
async fn codex_import_persists_secret_backed_account_without_raw_source_auth() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let auth_bytes = br#"{"OPENAI_API_KEY":"sk-test"}"#;
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "codex-candidate".to_string(),
            provider_id: "codex".to_string(),
            provider_label: "Codex".to_string(),
            kind: "json_file".to_string(),
            path: "/tmp/.codex/auth.json".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("subscription".to_string()),
            fingerprint: Some(sha256_hex(auth_bytes)),
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(auth_bytes.to_vec()),
        label: Some("Imported Codex profile".to_string()),
    };

    let result = import_codex_candidate(root, &material).await.unwrap();
    assert_eq!(result.status, "imported");

    let registry = provider_accounts::load_codex_registry(root).await.unwrap();
    let account_id = result.profile_id.as_deref().expect("profile id");
    let entry = registry
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .expect("imported account");
    let secret_ref = entry.secret_ref.as_deref().expect("secret_ref");
    assert_eq!(entry.kind, provider_accounts::CODEX_CREDENTIAL_KIND_API_KEY);
    assert!(provider_accounts::codex_secrets_root(root)
        .join(secret_ref)
        .exists());
    assert!(tokio::fs::metadata(
        provider_accounts::codex_account_dir(root, account_id).join("auth.json")
    )
    .await
    .is_err());
}

#[tokio::test]
async fn codex_import_migrates_matching_raw_only_account_into_secret_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-1";
    let auth_bytes = br#"{"OPENAI_API_KEY":"sk-test"}"#;
    provider_accounts::save_codex_registry(
        root,
        &provider_accounts::CodexAccountRegistry {
            active_account_id: Some(account_id.to_string()),
            accounts: vec![provider_accounts::CodexAccountEntry {
                id: account_id.to_string(),
                label: "Existing".to_string(),
                kind: provider_accounts::CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
                email: None,
                provider_account_id: None,
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: None,
                endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
            }],
        },
    )
    .await
    .unwrap();
    let account_dir = provider_accounts::ensure_codex_account_dir(root, account_id)
        .await
        .unwrap();
    tokio::fs::write(account_dir.join("auth.json"), auth_bytes)
        .await
        .unwrap();

    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "codex-candidate".to_string(),
            provider_id: "codex".to_string(),
            provider_label: "Codex".to_string(),
            kind: "json_file".to_string(),
            path: "/tmp/.codex/auth.json".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("subscription".to_string()),
            fingerprint: Some(sha256_hex(auth_bytes)),
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(auth_bytes.to_vec()),
        label: Some("Imported Codex profile".to_string()),
    };

    let result = import_codex_candidate(root, &material).await.unwrap();
    assert_eq!(result.status, "already_imported");
    assert_eq!(result.profile_id.as_deref(), Some(account_id));

    let registry = provider_accounts::load_codex_registry(root).await.unwrap();
    let entry = registry
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .expect("existing account");
    let secret_ref = entry.secret_ref.as_deref().expect("secret_ref");
    assert!(provider_accounts::codex_secrets_root(root)
        .join(secret_ref)
        .exists());
    assert!(tokio::fs::metadata(
        provider_accounts::codex_account_dir(root, account_id).join("auth.json")
    )
    .await
    .is_err());
}

#[tokio::test]
async fn codex_import_fails_closed_on_malformed_existing_account_auth() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-1";
    provider_accounts::save_codex_registry(
        root,
        &provider_accounts::CodexAccountRegistry {
            active_account_id: Some(account_id.to_string()),
            accounts: vec![provider_accounts::CodexAccountEntry {
                id: account_id.to_string(),
                label: "Existing".to_string(),
                kind: provider_accounts::CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
                email: None,
                provider_account_id: None,
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: None,
                endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
            }],
        },
    )
    .await
    .unwrap();
    let account_dir = provider_accounts::codex_account_dir(root, account_id);
    tokio::fs::create_dir_all(&account_dir).await.unwrap();
    tokio::fs::write(account_dir.join("auth.json"), "{ invalid json")
        .await
        .unwrap();

    let auth_bytes = br#"{"OPENAI_API_KEY":"sk-test"}"#;
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "codex-candidate".to_string(),
            provider_id: "codex".to_string(),
            provider_label: "Codex".to_string(),
            kind: "json_file".to_string(),
            path: "/tmp/.codex/auth.json".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("subscription".to_string()),
            fingerprint: Some(sha256_hex(auth_bytes)),
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(auth_bytes.to_vec()),
        label: Some("Imported Codex profile".to_string()),
    };

    let err = import_codex_candidate(root, &material)
        .await
        .expect_err("malformed existing codex auth should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid codex auth JSON"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1/auth.json"),
        "expected existing auth path in error: {message}"
    );
}

#[tokio::test]
async fn codex_import_fails_closed_on_malformed_candidate_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "codex-candidate".to_string(),
            provider_id: "codex".to_string(),
            provider_label: "Codex".to_string(),
            kind: "json_file".to_string(),
            path: "/tmp/.codex/auth.json".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("subscription".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(b"{ invalid json".to_vec()),
        label: Some("Imported Codex profile".to_string()),
    };

    let err = import_codex_candidate(root, &material)
        .await
        .expect_err("malformed candidate auth should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("must be valid JSON"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("/tmp/.codex/auth.json"),
        "expected candidate path in error: {message}"
    );
}

#[test]
fn scan_detects_codex_auth_file() {
    let dir = tempfile::tempdir().unwrap();
    let roots = test_roots(dir.path());
    std::fs::create_dir_all(&roots.codex_home).unwrap();
    std::fs::write(roots.codex_home.join("auth.json"), br#"{"ok":true}"#).unwrap();

    let found = scan_with_roots(&roots)
        .into_iter()
        .find(|c| c.candidate.provider_id == "codex")
        .expect("codex candidate");
    assert_eq!(found.candidate.parse_status, "parsed");
    assert!(found.importable);
}

#[tokio::test]
async fn gemini_oauth_candidate_import_writes_canonical_account_registry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "gemini-oauth-candidate".to_string(),
            provider_id: "gemini".to_string(),
            provider_label: "Gemini".to_string(),
            kind: "auth_file".to_string(),
            path: "/tmp/.gemini/oauth_creds.json".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("subscription".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(br#"{"access_token":"a","refresh_token":"b"}"#.to_vec()),
        label: Some("Gemini import".to_string()),
    };

    let result = import_candidate_to_canonical(root, &material)
        .await
        .unwrap();
    assert_eq!(result.status, "imported");
    let registry = provider_accounts::load_gemini_registry(root).await.unwrap();
    assert_eq!(registry.accounts.len(), 1);
    assert_eq!(registry.active_account_id, result.profile_id);
    let profiles = list_provider_auth_profiles(root).await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].provider_id, "gemini");
    assert_eq!(profiles[0].id, registry.active_account_id.unwrap());
}

#[tokio::test]
async fn gemini_env_candidate_with_base_url_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let base_url = "https://generativelanguage.googleapis.com/v1beta/openai";
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "gemini-env-legacy".to_string(),
            provider_id: "gemini".to_string(),
            provider_label: "Gemini".to_string(),
            kind: "env_file".to_string(),
            path: "/tmp/.gemini/.env".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: Some(base_url.to_string()),
            auth_type: Some("api_key".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(
            format!("OPENAI_API_KEY=key-legacy\nOPENAI_BASE_URL={base_url}\n").into_bytes(),
        ),
        label: Some("Gemini legacy endpoint".to_string()),
    };

    let err = import_candidate_to_canonical(root, &material)
        .await
        .expect_err("import should fail");
    assert!(err
        .to_string()
        .contains("OpenAI-compatible endpoint imports are not supported"));
}

#[tokio::test]
async fn gemini_env_candidate_without_base_url_imports_native_key_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "gemini-env-native".to_string(),
            provider_id: "gemini".to_string(),
            provider_label: "Gemini".to_string(),
            kind: "env_file".to_string(),
            path: "/tmp/.gemini/.env".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("api_key".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(b"GEMINI_API_KEY=key-native\n".to_vec()),
        label: Some("Gemini native endpoint".to_string()),
    };

    let result = import_candidate_to_canonical(root, &material)
        .await
        .unwrap();
    assert_eq!(result.status, "imported");
    let config = harness_sources::get_provider_source_config(root, "gemini")
        .await
        .unwrap();
    assert_eq!(
        config.selected_source_kind,
        harness_sources::HarnessSourceKind::Endpoint
    );
    let selected_id = config
        .selected_endpoint_id
        .as_deref()
        .expect("selected endpoint id");
    let endpoint = config
        .endpoints
        .iter()
        .find(|endpoint| endpoint.id == selected_id)
        .expect("selected endpoint");
    assert_eq!(endpoint.auth_type, "gemini_api_key");
    assert!(endpoint.base_url.is_none());
}

#[tokio::test]
async fn gemini_env_candidate_with_google_api_key_imports_native_key_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "gemini-env-google-api-key".to_string(),
            provider_id: "gemini".to_string(),
            provider_label: "Gemini".to_string(),
            kind: "env_file".to_string(),
            path: "/tmp/.gemini/.env".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("api_key".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(b"GOOGLE_API_KEY=key-google\n".to_vec()),
        label: Some("Gemini Google API key".to_string()),
    };

    let result = import_candidate_to_canonical(root, &material)
        .await
        .unwrap();
    assert_eq!(result.status, "imported");
    let config = harness_sources::get_provider_source_config(root, "gemini")
        .await
        .unwrap();
    let selected_id = config
        .selected_endpoint_id
        .as_deref()
        .expect("selected endpoint id");
    let endpoint = config
        .endpoints
        .iter()
        .find(|endpoint| endpoint.id == selected_id)
        .expect("selected endpoint");
    assert_eq!(endpoint.auth_type, "gemini_api_key");
    assert!(endpoint.base_url.is_none());
}

#[tokio::test]
async fn gemini_env_candidate_with_google_api_key_and_vertex_markers_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "gemini-env-vertex".to_string(),
            provider_id: "gemini".to_string(),
            provider_label: "Gemini".to_string(),
            kind: "env_file".to_string(),
            path: "/tmp/.gemini/.env".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: None,
            auth_type: Some("api_key".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(b"GOOGLE_API_KEY=key-google\nGOOGLE_GENAI_USE_VERTEXAI=true\n".to_vec()),
        label: Some("Gemini Vertex AI".to_string()),
    };

    let result = import_candidate_to_canonical(root, &material).await;
    let error = result.expect_err("vertex env import should require service_account_json");
    assert!(error
        .to_string()
        .contains("Gemini Vertex env imports require service_account_json"));
    let config = harness_sources::get_provider_source_config(root, "gemini")
        .await
        .unwrap();
    assert!(config.selected_endpoint_id.is_none());
    assert!(config.endpoints.is_empty());
}

#[tokio::test]
async fn qwen_env_candidate_import_updates_endpoint_store_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "qwen-candidate".to_string(),
            provider_id: "qwen".to_string(),
            provider_label: "Qwen".to_string(),
            kind: "env_file".to_string(),
            path: "/tmp/.qwen/.env".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: Some("https://api.example.com/v1".to_string()),
            auth_type: Some("api_key".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(
            b"OPENAI_API_KEY=key-1\nOPENAI_BASE_URL=https://api.example.com/v1".to_vec(),
        ),
        label: Some("Qwen endpoint".to_string()),
    };

    let first = import_candidate_to_canonical(root, &material)
        .await
        .unwrap();
    assert_eq!(first.status, "imported");
    let config = harness_sources::get_provider_source_config(root, "qwen")
        .await
        .unwrap();
    assert_eq!(
        config.selected_source_kind,
        harness_sources::HarnessSourceKind::Endpoint
    );
    assert_eq!(config.endpoints.len(), 1);

    let second = import_candidate_to_canonical(root, &material)
        .await
        .unwrap();
    assert_eq!(second.status, "already_imported");
    let config = harness_sources::get_provider_source_config(root, "qwen")
        .await
        .unwrap();
    assert_eq!(config.endpoints.len(), 1);
    let profiles = list_provider_auth_profiles(root).await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].provider_id, "qwen");
    assert_eq!(profiles[0].id, first.profile_id.clone().unwrap());

    let updated_material = CandidateMaterial {
        secret_bytes: Some(
            b"OPENAI_API_KEY=key-2\nOPENAI_BASE_URL=https://api.example.com/v1".to_vec(),
        ),
        ..material
    };
    let updated = import_candidate_to_canonical(root, &updated_material)
        .await
        .unwrap();
    assert_eq!(updated.status, "updated");
    let config = harness_sources::get_provider_source_config(root, "qwen")
        .await
        .unwrap();
    assert_eq!(config.endpoints.len(), 1);
    let profiles = list_provider_auth_profiles(root).await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, updated.profile_id.unwrap());
}

#[tokio::test]
async fn qwen_exact_match_import_activates_existing_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let existing = harness_sources::upsert_provider_endpoint(
        root,
        "qwen",
        harness_sources::HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Existing Qwen endpoint".to_string(),
            base_url: Some("https://api.example.com/v1".to_string()),
            api_shape: harness_sources::default_shape_for_provider("qwen"),
            auth_type: None,
            model_override: None,
            api_key: Some("key-1".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .unwrap();
    let _ = harness_sources::set_provider_source_selection(
        root,
        "qwen",
        harness_sources::HarnessSourceKind::Subscription,
        None,
    )
    .await
    .unwrap();

    let material = CandidateMaterial {
        candidate: ProviderAuthImportCandidate {
            id: "qwen-candidate-exact".to_string(),
            provider_id: "qwen".to_string(),
            provider_label: "Qwen".to_string(),
            kind: "env_file".to_string(),
            path: "/tmp/.qwen/.env".to_string(),
            signal_strength: "strong".to_string(),
            confidence: "high".to_string(),
            parse_status: "parsed".to_string(),
            unsupported_reason: None,
            summary: None,
            account_identity: None,
            endpoint: Some("https://api.example.com/v1".to_string()),
            auth_type: Some("api_key".to_string()),
            fingerprint: None,
            last_modified: None,
        },
        importable: true,
        secret_bytes: Some(
            b"OPENAI_API_KEY=key-1\nOPENAI_BASE_URL=https://api.example.com/v1".to_vec(),
        ),
        label: Some("Qwen endpoint".to_string()),
    };

    let result = import_candidate_to_canonical(root, &material)
        .await
        .unwrap();
    assert_eq!(result.status, "already_imported");
    assert_eq!(result.profile_id.as_deref(), Some(existing.id.as_str()));

    let config = harness_sources::get_provider_source_config(root, "qwen")
        .await
        .unwrap();
    assert_eq!(
        config.selected_source_kind,
        harness_sources::HarnessSourceKind::Endpoint
    );
    assert_eq!(
        config.selected_endpoint_id.as_deref(),
        Some(existing.id.as_str())
    );
}

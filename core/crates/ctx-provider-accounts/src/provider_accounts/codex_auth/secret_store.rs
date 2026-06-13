use super::host::host_codex_auth_path;
use super::runtime_oauth::{clear_codex_oauth_reauth_required, codex_oauth_reauth_required};
use super::*;
use fs2::FileExt;
use std::fs::{File, OpenOptions};

const CODEX_OAUTH_AUTHORITY_LOCK_FILE: &str = ".ctx-refresh-token.lock";

pub(super) fn codex_auth_has_supported_shape(value: &serde_json::Value) -> bool {
    let has_api_key = value
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let has_token_bundle = value
        .get("tokens")
        .and_then(|v| v.as_object())
        .is_some_and(|tokens| {
            let access = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty());
            let refresh = tokens
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty());
            access && refresh
        });
    has_api_key || has_token_bundle
}

pub(super) fn codex_auth_has_runtime_supported_shape(value: &serde_json::Value) -> bool {
    let has_api_key = value
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    has_api_key || codex_auth_has_access_token(value)
}

pub(super) fn codex_auth_has_access_token(value: &serde_json::Value) -> bool {
    value
        .get("tokens")
        .and_then(|v| v.as_object())
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty())
}

pub(super) fn codex_auth_has_refresh_token(value: &serde_json::Value) -> bool {
    value
        .get("tokens")
        .and_then(|v| v.as_object())
        .and_then(|tokens| tokens.get("refresh_token"))
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty())
}

pub(super) fn codex_auth_provider_account_id(value: &serde_json::Value) -> Option<String> {
    value
        .get("tokens")
        .and_then(|v| v.as_object())
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

pub(super) fn codex_auth_kind(value: &serde_json::Value) -> Option<String> {
    let has_api_key = value
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let has_token_bundle = value
        .get("tokens")
        .and_then(|v| v.as_object())
        .is_some_and(|tokens| {
            let access = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty());
            let refresh = tokens
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty());
            access && refresh
        });
    if has_token_bundle {
        return Some(CODEX_CREDENTIAL_KIND_OAUTH.to_string());
    }
    if has_api_key {
        return Some(CODEX_CREDENTIAL_KIND_API_KEY.to_string());
    }
    None
}

async fn write_codex_secret_for_account(
    data_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<String> {
    if !codex_auth_has_supported_shape(auth) {
        anyhow::bail!(
            "codex auth has no OPENAI_API_KEY or tokens.access_token/tokens.refresh_token"
        );
    }
    let stored_auth = codex_oauth_broker_projection(auth)?;
    let secret_ref = format!("{account_id}.json");
    let secret_path = codex_secret_path(data_root, &secret_ref)?;
    let envelope = CodexSecretEnvelope {
        version: CODEX_SECRET_VERSION,
        auth: stored_auth,
    };
    let bytes = serde_json::to_vec_pretty(&envelope)?;
    write_secure_file_atomic(&secret_path, &bytes).await?;
    Ok(secret_ref)
}

async fn update_account_secret_ref(
    data_root: &Path,
    account_id: &str,
    secret_ref: String,
    kind: Option<String>,
    provider_account_id: Option<String>,
) -> Result<()> {
    let mut registry = load_codex_registry(data_root).await?;
    if let Some(entry) = registry.accounts.iter_mut().find(|a| a.id == account_id) {
        entry.secret_ref = Some(secret_ref);
        entry.kind = kind.unwrap_or_else(default_codex_credential_kind);
        if provider_account_id.is_some() {
            entry.provider_account_id = provider_account_id;
        }
        save_codex_registry(data_root, &registry).await?;
    }
    Ok(())
}

pub(super) async fn ensure_private_dir_allowing_concurrent_create(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || ensure_private_dir_allowing_concurrent_create_sync(&path))
        .await
        .context("joining private directory creation task")?
}

fn ensure_private_dir_allowing_concurrent_create_sync(path: &Path) -> Result<()> {
    let mut retries = 0;
    loop {
        match ctx_fs::permissions::ensure_private_dir_sync(path) {
            Ok(()) => return Ok(()),
            Err(err)
                if error_chain_has_kind(&err, std::io::ErrorKind::AlreadyExists) && retries < 4 =>
            {
                retries += 1;
                std::thread::yield_now();
                continue;
            }
            Err(err) => return Err(err),
        }
    }
}

fn error_chain_has_kind(error: &anyhow::Error, kind: std::io::ErrorKind) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == kind)
    })
}

fn validated_codex_broker_home(data_root: &Path, account_id: &str) -> Result<PathBuf> {
    let broker_home = codex_broker_home(data_root, account_id);
    crate::provider_accounts::paths::validate_codex_broker_home_before_broker_access(
        data_root,
        &broker_home,
    )?;
    Ok(broker_home)
}

pub(super) fn acquire_broker_oauth_authority_lock(home: &Path) -> Result<File> {
    ensure_private_dir_allowing_concurrent_create_sync(home)
        .with_context(|| format!("creating Codex broker home at {}", home.display()))?;
    let lock_path = home.join(CODEX_OAUTH_AUTHORITY_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening Codex OAuth authority lock {}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking Codex OAuth authority {}", lock_path.display()))?;
    Ok(file)
}

async fn acquire_broker_oauth_authority_lock_for_auth_async(
    data_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<Option<File>> {
    if !codex_auth_has_refresh_token(auth) {
        return Ok(None);
    }
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    let broker_home = validated_codex_broker_home(data_root, account_id)?;
    tokio::task::spawn_blocking(move || acquire_broker_oauth_authority_lock(&broker_home))
        .await
        .context("joining Codex OAuth broker lock task")?
        .map(Some)
}

async fn project_oauth_auth_to_broker_home(
    data_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<bool> {
    if !codex_auth_has_refresh_token(auth) {
        return Ok(false);
    }
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    let broker_home = validated_codex_broker_home(data_root, account_id)?;
    let projected = codex_oauth_broker_projection(auth)?;
    project_auth_value_to_home(&broker_home, &projected).await
}

pub(super) async fn project_oauth_auth_to_broker_home_with_lock(
    data_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<bool> {
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    if !codex_auth_has_refresh_token(auth) {
        return Ok(false);
    }
    let broker_home = validated_codex_broker_home(data_root, account_id)?;
    let _broker_lock =
        acquire_broker_oauth_authority_lock_for_auth_async(data_root, account_id, auth).await?;
    if let Some(existing) = read_codex_auth_value_from_home(&broker_home).await? {
        if codex_auth_has_refresh_token(&existing) {
            let projected = codex_oauth_broker_projection(&existing)?;
            if projected != existing {
                return project_auth_value_to_home(&broker_home, &projected).await;
            }
            return Ok(false);
        }
    }
    let projected = codex_oauth_broker_projection(auth)?;
    project_auth_value_to_home(&broker_home, &projected).await
}

fn codex_oauth_broker_projection(auth: &serde_json::Value) -> Result<serde_json::Value> {
    if !codex_auth_has_refresh_token(auth) {
        return Ok(auth.clone());
    }
    let mut projected = auth.clone();
    let Some(object) = projected.as_object_mut() else {
        anyhow::bail!("codex OAuth auth must be a JSON object");
    };
    object.remove("OPENAI_API_KEY");
    Ok(projected)
}

pub(super) async fn ingest_auth_value_for_account(
    data_root: &Path,
    account_id: &str,
    auth: &serde_json::Value,
) -> Result<bool> {
    if !codex_auth_has_supported_shape(auth) {
        anyhow::bail!(
            "codex auth has no OPENAI_API_KEY or tokens.access_token/tokens.refresh_token"
        );
    }
    let secret_ref = write_codex_secret_for_account(data_root, account_id, auth).await?;
    let kind = codex_auth_kind(auth);
    let provider_account_id = codex_auth_provider_account_id(auth);
    update_account_secret_ref(data_root, account_id, secret_ref, kind, provider_account_id).await?;
    Ok(true)
}

struct MatchingCodexAccount {
    entry: CodexAccountEntry,
    provider_identity_matches: bool,
}

async fn find_matching_codex_account(
    data_root: &Path,
    auth: &serde_json::Value,
) -> Result<Option<MatchingCodexAccount>> {
    let registry = load_codex_registry(data_root).await?;
    let provider_account_id = codex_auth_provider_account_id(auth);
    for existing in &registry.accounts {
        if provider_account_id.is_some()
            && existing.provider_account_id.as_deref() == provider_account_id.as_deref()
        {
            return Ok(Some(MatchingCodexAccount {
                entry: existing.clone(),
                provider_identity_matches: true,
            }));
        }
        let existing_auth = if let Some(secret_ref) = existing.secret_ref.as_deref() {
            load_codex_auth_from_secret_store(data_root, secret_ref).await?
        } else {
            let existing_auth_path = codex_account_dir(data_root, &existing.id).join("auth.json");
            match tokio::fs::read_to_string(&existing_auth_path).await {
                Ok(existing_payload) => {
                    serde_json::from_str(&existing_payload).with_context(|| {
                        format!(
                            "invalid codex auth JSON at {}",
                            existing_auth_path.display()
                        )
                    })?
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "reading existing codex auth at {}",
                            existing_auth_path.display()
                        )
                    });
                }
            }
        };
        if provider_account_id.is_some()
            && codex_auth_provider_account_id(&existing_auth).as_deref()
                == provider_account_id.as_deref()
        {
            return Ok(Some(MatchingCodexAccount {
                entry: existing.clone(),
                provider_identity_matches: true,
            }));
        }
        if existing_auth == *auth {
            return Ok(Some(MatchingCodexAccount {
                entry: existing.clone(),
                provider_identity_matches: false,
            }));
        }
    }
    Ok(None)
}

pub async fn remove_codex_account_home_auth_if_present(
    data_root: &Path,
    account_id: &str,
) -> Result<()> {
    ensure_safe_account_id(account_id)?;
    let auth_path = codex_account_dir(data_root, account_id).join("auth.json");
    match tokio::fs::remove_file(&auth_path).await {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("removing legacy codex auth at {}", auth_path.display())),
    }
}

pub async fn import_codex_auth_value_to_secret_store(
    data_root: &Path,
    label: Option<String>,
    auth: &serde_json::Value,
) -> Result<CodexAuthImportOutcome> {
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    let kind = codex_auth_kind(auth).ok_or_else(|| {
        anyhow!("codex auth has no OPENAI_API_KEY or tokens.access_token/tokens.refresh_token")
    })?;

    if let Some(existing) = find_matching_codex_account(data_root, auth).await? {
        if existing.entry.secret_ref.is_none() || existing.provider_identity_matches {
            let broker_home = validated_codex_broker_home(data_root, &existing.entry.id)?;
            let replace_broker_for_reauth = codex_auth_has_refresh_token(auth)
                && codex_oauth_reauth_required(data_root, &existing.entry.id).await?;
            let existing_broker_auth =
                if codex_auth_has_refresh_token(auth) && !replace_broker_for_reauth {
                    read_codex_auth_value_from_home(&broker_home).await?
                } else {
                    None
                };
            if let Some(broker_auth) = existing_broker_auth {
                if !codex_auth_has_supported_shape(&broker_auth) {
                    anyhow::bail!(
                        "codex broker auth at {} has unsupported auth shape",
                        broker_home.join("auth.json").display()
                    );
                }
                if !codex_auth_has_refresh_token(&broker_auth) {
                    anyhow::bail!(
                        "codex broker auth at {} is not an OAuth refresh-token credential",
                        broker_home.join("auth.json").display()
                    );
                }
                let projected = codex_oauth_broker_projection(&broker_auth)?;
                if projected != broker_auth {
                    project_oauth_auth_to_broker_home_with_lock(
                        data_root,
                        &existing.entry.id,
                        &broker_auth,
                    )
                    .await?;
                }
                ingest_auth_value_for_account(data_root, &existing.entry.id, &projected).await?;
            } else {
                let _broker_lock = acquire_broker_oauth_authority_lock_for_auth_async(
                    data_root,
                    &existing.entry.id,
                    auth,
                )
                .await?;
                ingest_auth_value_for_account(data_root, &existing.entry.id, auth).await?;
                project_oauth_auth_to_broker_home(data_root, &existing.entry.id, auth).await?;
            }
            clear_codex_oauth_reauth_required(data_root, &existing.entry.id).await?;
            remove_codex_account_home_auth_if_present(data_root, &existing.entry.id).await?;
        }
        let registry = set_active_codex_account(data_root, Some(existing.entry.id.clone())).await?;
        return Ok(CodexAuthImportOutcome {
            registry,
            account_id: existing.entry.id,
            created: false,
        });
    }

    let account_id = uuid::Uuid::new_v4().to_string();
    let _broker_lock =
        acquire_broker_oauth_authority_lock_for_auth_async(data_root, &account_id, auth).await?;
    let secret_ref = write_codex_secret_for_account(data_root, &account_id, auth).await?;
    project_oauth_auth_to_broker_home(data_root, &account_id, auth).await?;
    let entry = CodexAccountEntry {
        id: account_id.clone(),
        label: normalize_label(label, &account_id),
        kind,
        email: None,
        provider_account_id: codex_auth_provider_account_id(auth),
        plan_type: None,
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: Some(secret_ref),
        endpoint_profile: CodexEndpointProfile::default(),
    };
    let _ = upsert_codex_account(data_root, entry).await?;
    let registry = set_active_codex_account(data_root, Some(account_id.clone())).await?;
    Ok(CodexAuthImportOutcome {
        registry,
        account_id,
        created: true,
    })
}

pub(super) async fn load_codex_auth_from_secret_store(
    data_root: &Path,
    secret_ref: &str,
) -> Result<serde_json::Value> {
    let path = codex_secret_path(data_root, secret_ref)?;
    let payload = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("missing codex secret at {}", path.display()))?;
    let envelope: CodexSecretEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("invalid codex secret JSON at {}", path.display()))?;
    if envelope.version != CODEX_SECRET_VERSION {
        anyhow::bail!(
            "unsupported codex secret version {} at {}",
            envelope.version,
            path.display()
        );
    }
    if !codex_auth_has_supported_shape(&envelope.auth) {
        anyhow::bail!(
            "codex secret at {} has unsupported auth shape",
            path.display()
        );
    }
    Ok(envelope.auth)
}

pub(super) async fn project_auth_value_to_home(
    home: &Path,
    auth: &serde_json::Value,
) -> Result<bool> {
    let payload = serde_json::to_vec_pretty(auth)?;
    ensure_private_dir_allowing_concurrent_create(home).await?;
    let dest = home.join("auth.json");
    let write = match tokio::fs::read(&dest).await {
        Ok(existing) => existing != payload,
        Err(_) => true,
    };
    if write {
        write_secure_file_atomic(&dest, &payload).await?;
    }
    Ok(write)
}

async fn read_codex_auth_value_from_home(home: &Path) -> Result<Option<serde_json::Value>> {
    let auth_path = home.join("auth.json");
    let payload = match tokio::fs::read_to_string(&auth_path).await {
        Ok(payload) => payload,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading codex auth at {}", auth_path.display()));
        }
    };
    let auth: serde_json::Value = serde_json::from_str(&payload)
        .with_context(|| format!("invalid codex auth JSON at {}", auth_path.display()))?;
    Ok(Some(auth))
}

pub(super) async fn hydrate_legacy_account_auth_to_broker_home(
    data_root: &Path,
    account_id: &str,
    include_api_key: bool,
) -> Result<Option<PathBuf>> {
    ensure_safe_account_id(account_id)?;
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    let account_home = codex_account_dir(data_root, account_id);
    let Some(auth) = read_codex_auth_value_from_home(&account_home).await? else {
        return Ok(None);
    };
    if !codex_auth_has_supported_shape(&auth) {
        return Ok(None);
    }
    if !include_api_key && !codex_auth_has_refresh_token(&auth) {
        return Ok(None);
    }

    let broker_home = validated_codex_broker_home(data_root, account_id)?;
    if let Some(broker_auth) = read_codex_auth_value_from_home(&broker_home).await? {
        if !codex_auth_has_supported_shape(&broker_auth) {
            anyhow::bail!(
                "codex broker auth at {} has unsupported auth shape",
                broker_home.join("auth.json").display()
            );
        }
        if codex_auth_has_refresh_token(&auth) && !codex_auth_has_refresh_token(&broker_auth) {
            anyhow::bail!(
                "codex broker auth at {} is not an OAuth refresh-token credential",
                broker_home.join("auth.json").display()
            );
        }
        let broker_auth = if codex_auth_has_refresh_token(&broker_auth) {
            let projected = codex_oauth_broker_projection(&broker_auth)?;
            if projected != broker_auth {
                project_oauth_auth_to_broker_home_with_lock(data_root, account_id, &broker_auth)
                    .await?;
            }
            projected
        } else {
            broker_auth
        };
        ingest_auth_value_for_account(data_root, account_id, &broker_auth).await?;
        remove_codex_account_home_auth_if_present(data_root, account_id).await?;
        return Ok(Some(broker_home));
    }

    let _broker_lock =
        acquire_broker_oauth_authority_lock_for_auth_async(data_root, account_id, &auth).await?;
    ingest_auth_value_for_account(data_root, account_id, &auth).await?;
    if codex_auth_has_refresh_token(&auth) {
        project_oauth_auth_to_broker_home(data_root, account_id, &auth).await?;
        remove_codex_account_home_auth_if_present(data_root, account_id).await?;
    } else {
        project_auth_value_to_home(&broker_home, &auth).await?;
        remove_codex_account_home_auth_if_present(data_root, account_id).await?;
    }
    Ok(Some(broker_home))
}

pub async fn import_host_codex_auth_to_secret_store(
    data_root: &Path,
    label: Option<String>,
) -> Result<CodexAccountRegistry> {
    let auth_path = host_codex_auth_path()?;
    let payload = tokio::fs::read_to_string(&auth_path)
        .await
        .with_context(|| format!("missing host codex auth at {}", auth_path.display()))?;
    let auth: serde_json::Value = serde_json::from_str(&payload)
        .with_context(|| format!("invalid codex auth JSON at {}", auth_path.display()))?;
    import_codex_auth_value_to_secret_store(data_root, label, &auth)
        .await
        .map(|outcome| outcome.registry)
}

pub async fn hydrate_codex_account_home_from_secret(
    data_root: &Path,
    account_id: &str,
) -> Result<bool> {
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    let registry = load_codex_registry(data_root).await?;
    let Some(account) = registry.accounts.iter().find(|a| a.id == account_id) else {
        return Ok(false);
    };
    if codex_account_deletion_in_progress(data_root, account_id).await? {
        return Ok(false);
    }
    let include_legacy_api_key = account.kind.trim() == CODEX_CREDENTIAL_KIND_API_KEY;
    if hydrate_legacy_account_auth_to_broker_home(data_root, account_id, include_legacy_api_key)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    if account.kind.trim() != CODEX_CREDENTIAL_KIND_API_KEY
        && super::runtime::migrate_owned_runtime_oauth_projection_to_broker_if_needed(
            data_root, account_id,
        )
        .await?
    {
        return Ok(true);
    }
    let Some(secret_ref) = account.secret_ref.as_deref() else {
        if !include_legacy_api_key
            && hydrate_legacy_account_auth_to_broker_home(data_root, account_id, true)
                .await?
                .is_some()
        {
            return Ok(true);
        }
        return Ok(false);
    };
    let auth = load_codex_auth_from_secret_store(data_root, secret_ref).await?;
    let home = validated_codex_broker_home(data_root, account_id)?;
    if codex_auth_has_refresh_token(&auth) && home.join("auth.json").exists() {
        return Ok(false);
    }
    let _broker_lock =
        acquire_broker_oauth_authority_lock_for_auth_async(data_root, account_id, &auth).await?;
    project_auth_value_to_home(&home, &auth).await
}

pub async fn ingest_codex_account_auth_to_secret_store(
    data_root: &Path,
    account_id: &str,
) -> Result<bool> {
    crate::provider_accounts::paths::validate_codex_provider_root_before_broker_access(data_root)?;
    let auth_path = codex_account_dir(data_root, account_id).join("auth.json");
    let payload = match tokio::fs::read_to_string(&auth_path).await {
        Ok(payload) => payload,
        Err(_) => return Ok(false),
    };
    let auth: serde_json::Value = serde_json::from_str(&payload)
        .with_context(|| format!("invalid codex auth JSON at {}", auth_path.display()))?;
    let _broker_lock =
        acquire_broker_oauth_authority_lock_for_auth_async(data_root, account_id, &auth).await?;
    let ingested = ingest_auth_value_for_account(data_root, account_id, &auth).await?;
    project_oauth_auth_to_broker_home(data_root, account_id, &auth).await?;
    Ok(ingested)
}

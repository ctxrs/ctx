use super::runtime_oauth::clear_oauth_runtime_home_for_account;
use super::*;
use crate::provider_accounts::paths::validate_codex_runtime_home_before_broker_access;

pub(crate) async fn write_runtime_owner_marker(data_root: &Path, account_id: &str) -> Result<()> {
    validate_codex_runtime_home_before_broker_access(data_root)?;
    let marker = codex_runtime_owner_path(data_root);
    write_secure_file_atomic(&marker, account_id.as_bytes()).await
}

pub(super) async fn read_runtime_owner_marker(data_root: &Path) -> Result<Option<String>> {
    validate_codex_runtime_home_before_broker_access(data_root)?;
    let marker = codex_runtime_owner_path(data_root);
    let value = match tokio::fs::read_to_string(&marker).await {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(value.to_string()))
}

pub(crate) async fn clear_runtime_auth_projection(data_root: &Path) -> Result<()> {
    validate_codex_runtime_home_before_broker_access(data_root)?;
    let auth_path = codex_runtime_home(data_root).join("auth.json");
    match tokio::fs::remove_file(&auth_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let owner_path = codex_runtime_owner_path(data_root);
    match tokio::fs::remove_file(&owner_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

pub(crate) async fn clear_runtime_auth_projection_for_runtime_roots(
    data_root: &Path,
    account_id: &str,
) -> Result<()> {
    clear_legacy_runtime_auth_projection_for_runtime_roots(data_root, account_id).await?;
    clear_oauth_runtime_home_for_account(data_root, account_id).await?;
    for runtime_root in container_runtime_data_roots(data_root).await {
        clear_oauth_runtime_home_for_account(&runtime_root, account_id).await?;
    }
    Ok(())
}

pub(crate) async fn clear_legacy_runtime_auth_projection_for_runtime_roots(
    data_root: &Path,
    account_id: &str,
) -> Result<()> {
    clear_runtime_auth_projection_if_owned_by(data_root, account_id).await?;
    for runtime_root in container_runtime_data_roots(data_root).await {
        clear_runtime_auth_projection_if_owned_by(&runtime_root, account_id).await?;
    }
    Ok(())
}

pub(super) async fn clear_runtime_auth_projection_if_owned_by(
    data_root: &Path,
    account_id: &str,
) -> Result<()> {
    let owner = read_runtime_owner_marker(data_root).await?;
    if owner.as_deref() == Some(account_id) {
        clear_runtime_auth_projection(data_root).await?;
    }
    Ok(())
}

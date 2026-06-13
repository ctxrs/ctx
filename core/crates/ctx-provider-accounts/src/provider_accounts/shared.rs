use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ctx_fs::permissions::{ensure_private_dir, write_private_file_atomic};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub(crate) fn parse_required_json_object(raw: &str, field: &str) -> Result<serde_json::Value> {
    let parsed = parse_json_value(raw, field)?;
    if !parsed.is_object() {
        bail!("{field} must be a JSON object");
    }
    Ok(parsed)
}

pub(crate) fn parse_optional_json_value(
    raw: Option<&str>,
    field: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(parse_json_value(trimmed, field)?))
}

pub(crate) fn normalize_optional_email(email: Option<String>) -> Option<String> {
    email
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

pub(crate) fn apply_label_update(label: Option<String>, current: &mut String) {
    if let Some(raw) = label {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            *current = trimmed.to_string();
        }
    }
}

pub(crate) fn apply_email_update(email: Option<String>, current: &mut Option<String>) {
    if let Some(raw) = email {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            *current = None;
        } else {
            *current = Some(trimmed.to_string());
        }
    }
}

pub(crate) fn ensure_safe_account_id(account_id: &str) -> Result<()> {
    if account_id.trim().is_empty() {
        bail!("account_id is required");
    }

    let mut components = Path::new(account_id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail!("account_id must be a single path segment"),
    }
}

pub(crate) fn ensure_safe_secret_ref(secret_ref: &str) -> Result<()> {
    if secret_ref.trim().is_empty() {
        bail!("secret_ref is required");
    }

    let mut components = Path::new(secret_ref).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail!("secret_ref must be a single path segment"),
    }
}

pub(crate) fn ensure_account_exists(found: bool) -> Result<()> {
    if !found {
        bail!("unknown account");
    }
    Ok(())
}

pub(crate) fn collect_secret_paths<'a>(
    data_root: &Path,
    secret_refs: impl IntoIterator<Item = &'a str>,
    secret_path_for_ref: fn(&Path, &str) -> Result<PathBuf>,
) -> Vec<PathBuf> {
    secret_refs
        .into_iter()
        .filter_map(
            |secret_ref| match secret_path_for_ref(data_root, secret_ref) {
                Ok(path) => Some(path),
                Err(err) => {
                    tracing::warn!("skipping unsafe secret_ref {secret_ref:?}: {err:#}");
                    None
                }
            },
        )
        .collect()
}

pub(crate) async fn remove_projected_account_home_for_runtime_roots(
    data_root: &Path,
    account_id: &str,
    account_home_for_root: fn(&Path, &str) -> PathBuf,
    provider_id: &str,
) -> Result<()> {
    for runtime_root in container_runtime_data_roots(data_root).await {
        let projected_home = account_home_for_root(&runtime_root, account_id);
        match tokio::fs::remove_dir_all(&projected_home).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "removing projected {provider_id} account home {}",
                        projected_home.display()
                    )
                });
            }
        }
    }

    Ok(())
}

pub(crate) async fn load_json_registry<T: DeserializeOwned + Default>(
    path: &Path,
    label: &str,
) -> Result<T> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => serde_json::from_str(&contents)
            .with_context(|| format!("parsing {label} at {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err).with_context(|| format!("reading {label} at {}", path.display())),
    }
}

pub(crate) async fn save_json_registry<T: Serialize>(path: &Path, registry: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent).await?;
    }
    let payload = serde_json::to_vec_pretty(registry)?;
    write_private_file_atomic(path, &payload).await?;
    Ok(())
}

pub(crate) async fn write_secure_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let _ = path
        .parent()
        .ok_or_else(|| anyhow!("missing parent dir for {}", path.display()))?;
    write_private_file_atomic(path, bytes).await
}

pub(crate) fn home_config_cache_env(home: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("HOME".to_string(), home.to_string_lossy().to_string());
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        home.join(".config").to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_CACHE_HOME".to_string(),
        home.join(".cache").to_string_lossy().to_string(),
    );
    env
}

pub(crate) fn prepend_dir_to_path_env(dir: &Path) -> Result<String> {
    let mut path_parts = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        path_parts.extend(std::env::split_paths(&existing));
    }
    let joined = std::env::join_paths(path_parts).context("joining PATH with prepended dir")?;
    Ok(joined.to_string_lossy().to_string())
}

pub(crate) async fn ensure_home_config_cache_dirs(home: &Path) -> Result<()> {
    ensure_private_dir(home).await?;
    ensure_private_dir(&home.join(".config")).await?;
    ensure_private_dir(&home.join(".cache")).await?;
    Ok(())
}

fn parse_json_value(raw: &str, field: &str) -> Result<serde_json::Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("{field} is required");
    }
    serde_json::from_str(trimmed).with_context(|| format!("{field} must be valid JSON"))
}

fn container_workspaces_root(data_root: &Path) -> PathBuf {
    data_root.join("containers").join("workspaces")
}

pub(crate) async fn container_runtime_data_roots(data_root: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut entries = match tokio::fs::read_dir(container_workspaces_root(data_root)).await {
        Ok(entries) => entries,
        Err(_) => return roots,
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let runtime_root = entry.path().join("data");
        match tokio::fs::metadata(&runtime_root).await {
            Ok(metadata) if metadata.is_dir() => roots.push(runtime_root),
            _ => {}
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use super::{ensure_safe_account_id, ensure_safe_secret_ref};

    #[test]
    fn account_id_validation_rejects_path_traversal() {
        ensure_safe_account_id("acct-123").unwrap();
        ensure_safe_account_id("acct_123").unwrap();

        assert!(ensure_safe_account_id("").is_err());
        assert!(ensure_safe_account_id("  ").is_err());
        assert!(ensure_safe_account_id(".").is_err());
        assert!(ensure_safe_account_id("..").is_err());
        assert!(ensure_safe_account_id("../acct").is_err());
        assert!(ensure_safe_account_id("acct/../x").is_err());
        assert!(ensure_safe_account_id("acct/x").is_err());
    }

    #[test]
    fn secret_ref_validation_rejects_path_traversal() {
        ensure_safe_secret_ref("acct-123.json").unwrap();
        ensure_safe_secret_ref("acct_123").unwrap();

        assert!(ensure_safe_secret_ref("").is_err());
        assert!(ensure_safe_secret_ref("  ").is_err());
        assert!(ensure_safe_secret_ref(".").is_err());
        assert!(ensure_safe_secret_ref("..").is_err());
        assert!(ensure_safe_secret_ref("../secret").is_err());
        assert!(ensure_safe_secret_ref("/tmp/secret").is_err());
        assert!(ensure_safe_secret_ref("nested/secret").is_err());
    }
}

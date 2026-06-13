use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

pub(super) async fn apply_openhands_launch_overrides(
    workdir: &Path,
    provider_env: &mut HashMap<String, String>,
) -> Result<()> {
    let Some(data_root) = provider_env
        .get("CTX_DATA_ROOT")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let Some(session_id) = provider_env
        .get("CTX_SESSION_ID")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };

    let alias = ensure_openhands_workdir_alias(Path::new(data_root), session_id, workdir).await?;
    provider_env.insert(
        "OPENHANDS_WORK_DIR".to_string(),
        alias.to_string_lossy().to_string(),
    );
    Ok(())
}

async fn ensure_openhands_workdir_alias(
    data_root: &Path,
    session_id: &str,
    workdir: &Path,
) -> Result<PathBuf> {
    let alias_root = data_root
        .join("providers")
        .join("openhands")
        .join("workdir-aliases");
    tokio::fs::create_dir_all(&alias_root).await?;
    let alias = alias_root.join(session_id);
    reset_existing_alias(&alias, workdir).await?;
    match create_dir_symlink(workdir, &alias).await {
        Ok(()) => {}
        // Windows directory symlinks often require Developer Mode or elevated privileges.
        // Falling back to the real workdir keeps OpenHands usable when the short alias cannot
        // be created, even if the path is longer than ideal.
        Err(err) if should_fallback_to_direct_openhands_workdir(&err) => {
            return Ok(workdir.to_path_buf());
        }
        Err(err) => return Err(err),
    }
    Ok(alias)
}

pub(super) fn should_fallback_to_direct_openhands_workdir(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
            == Some(1314)
    })
}

async fn reset_existing_alias(alias: &Path, target: &Path) -> Result<()> {
    let metadata = match tokio::fs::symlink_metadata(alias).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("stat {}", alias.display())),
    };

    if metadata.file_type().is_symlink() {
        let existing_target = tokio::fs::read_link(alias)
            .await
            .with_context(|| format!("read link {}", alias.display()))?;
        if existing_target == target {
            return Ok(());
        }
        tokio::fs::remove_file(alias)
            .await
            .with_context(|| format!("remove stale symlink {}", alias.display()))?;
        return Ok(());
    }

    if metadata.is_dir() {
        tokio::fs::remove_dir_all(alias)
            .await
            .with_context(|| format!("remove stale directory {}", alias.display()))?;
    } else {
        tokio::fs::remove_file(alias)
            .await
            .with_context(|| format!("remove stale file {}", alias.display()))?;
    }
    Ok(())
}

async fn create_dir_symlink(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let source = source.to_path_buf();
        let target = target.to_path_buf();
        let source_for_link = source.clone();
        let target_for_link = target.clone();
        tokio::task::spawn_blocking(move || symlink(source_for_link, target_for_link))
            .await
            .map_err(|err| anyhow!("joining symlink task: {err}"))?
            .with_context(|| format!("symlink {} -> {}", target.display(), source.display()))?;
        return Ok(());
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_dir;
        let source = source.to_path_buf();
        let target = target.to_path_buf();
        let source_for_link = source.clone();
        let target_for_link = target.clone();
        tokio::task::spawn_blocking(move || symlink_dir(source_for_link, target_for_link))
            .await
            .map_err(|err| anyhow!("joining symlink task: {err}"))?
            .with_context(|| format!("symlink {} -> {}", target.display(), source.display()))?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(anyhow!(
        "directory symlinks are not supported on this platform"
    ))
}

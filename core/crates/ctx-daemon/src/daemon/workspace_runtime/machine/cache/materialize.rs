use super::*;

#[cfg(test)]
pub(super) fn collect_sandbox_machine_cache_file_relpaths(root: &Path) -> Result<Vec<PathBuf>> {
    fn collect_recursive(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("read_dir entry {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat {}", path.display()))?;
            if file_type.is_dir() {
                collect_recursive(root, &path, out)?;
                continue;
            }
            if file_type.is_file() || file_type.is_symlink() {
                let relpath = path.strip_prefix(root).with_context(|| {
                    format!("computing relative cache path for {}", path.display())
                })?;
                out.push(relpath.to_path_buf());
            }
        }
        Ok(())
    }

    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut relpaths = Vec::new();
    for provider_dir in
        std::fs::read_dir(root).with_context(|| format!("read_dir {}", root.display()))?
    {
        let provider_dir =
            provider_dir.with_context(|| format!("read_dir entry {}", root.display()))?;
        let provider_path = provider_dir.path();
        if !provider_path.is_dir() {
            continue;
        }
        let cache_dir = provider_path.join("cache");
        if !cache_dir.is_dir() {
            continue;
        }
        collect_recursive(root, &cache_dir, &mut relpaths)?;
    }
    relpaths.sort();
    Ok(relpaths)
}

#[cfg(test)]
#[cfg(unix)]
fn symlink_cache_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dest)
}

#[cfg(test)]
#[cfg(windows)]
fn symlink_cache_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dest)
}

#[cfg(test)]
#[cfg(not(any(unix, windows)))]
fn symlink_cache_file(_src: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "file symlinks are not supported on this platform",
    ))
}

#[cfg(test)]
fn sandbox_machine_cache_tmp_path(dest: &Path) -> PathBuf {
    let file_name = dest
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("cache-file");
    dest.with_file_name(format!(
        ".{file_name}.tmp-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

#[cfg(test)]
pub(super) async fn materialize_sandbox_machine_cache_file(
    src: &Path,
    dest: &Path,
    allow_symlink: bool,
) -> Result<()> {
    if src == dest || dest.exists() {
        return Ok(());
    }
    let Some(parent) = dest.parent() else {
        anyhow::bail!(
            "sandbox machine cache target has no parent: {}",
            dest.display()
        );
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;

    match std::fs::hard_link(src, dest) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(_) => {}
    }
    if allow_symlink {
        match symlink_cache_file(src, dest) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(_) => {}
        }
    }

    let tmp = sandbox_machine_cache_tmp_path(dest);
    let _ = fs::remove_file(&tmp).await;
    fs::copy(src, &tmp)
        .await
        .with_context(|| format!("copying {} -> {}", src.display(), tmp.display()))?;
    match fs::rename(&tmp, dest).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp).await;
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp).await;
            Err(err).with_context(|| format!("moving {} -> {}", tmp.display(), dest.display()))
        }
    }
}

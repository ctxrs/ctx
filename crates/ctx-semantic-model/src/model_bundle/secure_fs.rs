use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{anyhow, bail, Context, Result};

static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn require_real_directory(path: &Path, description: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {description} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{description} must be a real directory: {}", path.display());
    }
    Ok(())
}

pub(super) fn create_private_cache_root(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!(
                "model cache directory must be a real directory: {}",
                path.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .with_context(|| format!("create model cache directory {}", path.display()))?;
            require_real_directory(path, "model cache directory")?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect model cache directory {}", path.display()))
        }
    }
    set_private_directory_permissions(path)
}

pub(super) fn create_private_directory_tree_nofollow(base: &Path, leaf: &Path) -> Result<()> {
    let relative = leaf
        .strip_prefix(base)
        .map_err(|_| anyhow!("cache path escaped its root"))?;
    require_real_directory(base, "model cache root")?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("model cache path contains invalid component");
        };
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => {
                set_private_directory_permissions(&current)?;
                sync_parent(&current)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create model cache directory {}", current.display())
                });
            }
        }
        require_real_directory(&current, "model cache directory")?;
        set_private_directory_permissions(&current)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("secure model cache directory {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn create_private_staging_directory(path: &Path) -> Result<()> {
    reject_symlink_if_present(path)?;
    fs::create_dir(path).with_context(|| {
        format!(
            "create Core ML extraction staging directory {}",
            path.display()
        )
    })?;
    set_private_directory_permissions(path)
}

pub(super) fn reject_symlink_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlink path {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect path {}", path.display())),
    }
}

pub(super) fn unique_sibling(destination: &Path, purpose: &str) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent"))?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("destination has no UTF-8 file name"))?;
    for _ in 0..128 {
        let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.{purpose}.{}.{}.tmp", process::id(), nonce));
        if !candidate.exists() {
            reject_symlink_if_present(&candidate)?;
            return Ok(candidate);
        }
    }
    bail!("could not allocate unique staging path")
}

#[cfg(unix)]
pub(super) fn open_read_nofollow(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| {
            format!(
                "open regular file without following symlinks {}",
                path.display()
            )
        })
}

#[cfg(windows)]
pub(super) fn open_read_nofollow(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| {
            format!(
                "open regular file without following reparse points {}",
                path.display()
            )
        })
}

#[cfg(not(any(unix, windows)))]
pub(super) fn open_read_nofollow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open regular file {}", path.display()))
}

#[cfg(unix)]
pub(super) fn create_new_nofollow(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("create file without following symlinks {}", path.display()))
}

pub(super) fn create_new_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(super) fn metadata_if_present(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn ensure_target_parent_inside_cache(cache_root: &Path, target: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(cache_root).with_context(|| {
        format!(
            "resolve Core ML cache root {} before repair",
            cache_root.display()
        )
    })?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("Core ML cache repair target has no parent"))?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("resolve Core ML cache repair parent {}", parent.display()))?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!(
            "refusing to repair a content-addressed cache entry outside the configured cache root"
        );
    }
    Ok(())
}

pub(super) fn remove_real_directory_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("refusing to remove a non-directory or symlink cache path")
        }
        Ok(_) => fs::remove_dir_all(path)
            .with_context(|| format!("remove cache directory {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect cache path {}", path.display())),
    }
}

pub(super) fn remove_real_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("refusing to remove a non-file or symlink cache path")
        }
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("remove cache file {}", path.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect cache path {}", path.display())),
    }
}

#[cfg(not(unix))]
pub(super) fn create_new_nofollow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create file {}", path.display()))
}

#[cfg(unix)]
pub(super) fn same_file_metadata(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

#[cfg(not(unix))]
pub(super) fn same_file_metadata(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open directory for sync {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    sync_directory(parent)
}

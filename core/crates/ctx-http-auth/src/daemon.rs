use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_fs::permissions::{
    ensure_private_dir_sync, read_private_file_to_string_sync, reject_symlink_sync,
    write_private_file_atomic_sync,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const DAEMON_AUTH_FILENAME: &str = "daemon_auth.json";

pub fn prepare_daemon_data_root(data_root: PathBuf) -> Result<PathBuf> {
    ensure_private_dir_sync(&data_root)?;
    // Canonicalize so legacy machine mount sources resolve under shared roots on macOS
    // (e.g. /tmp -> /private/tmp). This also reduces accidental duplicate state roots.
    let data_root = std::fs::canonicalize(&data_root).unwrap_or(data_root);
    ensure_private_dir_sync(&data_root)?;
    let _ = ensure_private_dir_sync(&data_root.join("logs"));
    Ok(data_root)
}

pub fn acquire_daemon_lock(data_root: &Path) -> Result<std::fs::File> {
    let path = data_root.join("daemon.lock");
    reject_symlink_if_exists(&path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("opening daemon lockfile {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(windows)]
    let _ = ctx_fs::permissions::harden_private_file_sync(&path);

    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = file.set_len(0);
            let _ = writeln!(file, "{}", std::process::id());
            let _ = file.sync_all();
            Ok(file)
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
            anyhow::bail!("ctx daemon already running (lockfile {})", path.display())
        }
        Err(e) => Err(e).with_context(|| format!("locking daemon lockfile {}", path.display())),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonAuthFile {
    pub token: String,
    #[serde(default)]
    pub daemon_url: Option<String>,
}

pub fn daemon_auth_path(data_root: &Path) -> PathBuf {
    data_root.join(DAEMON_AUTH_FILENAME)
}

fn read_daemon_auth_file(path: &Path) -> Result<Option<DaemonAuthFile>> {
    let Some(contents) = read_private_file_to_string_sync(path)? else {
        return Ok(None);
    };
    let auth: DaemonAuthFile = serde_json::from_str(&contents)
        .with_context(|| format!("parsing daemon auth file {}", path.display()))?;
    if auth.token.trim().is_empty() {
        anyhow::bail!("daemon auth file {} contains empty token", path.display());
    }
    Ok(Some(auth))
}

fn reject_symlink_if_exists(path: &Path) -> Result<Option<()>> {
    if reject_symlink_sync(path)? {
        Ok(Some(()))
    } else {
        Ok(None)
    }
}

pub fn write_daemon_auth_file(path: &Path, auth: &DaemonAuthFile) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(auth)?;
    let _ = std::fs::remove_file(&tmp);
    write_private_file_atomic_sync(path, &bytes)?;
    Ok(())
}

pub fn load_or_init_daemon_auth(data_root: &Path) -> Result<DaemonAuthFile> {
    let path = daemon_auth_path(data_root);
    if let Some(auth) = read_daemon_auth_file(&path)? {
        return Ok(auth);
    }
    let auth = DaemonAuthFile {
        token: uuid::Uuid::new_v4().to_string(),
        daemon_url: None,
    };
    write_daemon_auth_file(&path, &auth)?;
    Ok(auth)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

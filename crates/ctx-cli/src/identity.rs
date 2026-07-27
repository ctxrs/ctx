use std::{
    env, fs,
    io::{Read as _, Seek as _, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use ctx_history_core::{
    platform_security::{
        restrict_private_directory, restrict_private_file, verify_private_directory,
        verify_private_file,
    },
    utc_now,
};
use ctx_pro_host_protocol::ProFilesystemLayout;
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

const DEVICE_FILE: &str = "device.json";
const MAX_INSTALLATION_IDENTITY_BYTES: u64 = 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallationIdentityRecord {
    schema_version: u16,
    install_id: String,
    created_at: String,
}

/// Loads or creates the one opaque identity owned by this ctx data root.
///
/// The identity is product state. Analytics may report it when enabled, but
/// analytics does not own its creation, lifetime, or format.
pub(crate) fn installation_id(data_root: &Path) -> Result<String> {
    access_installation_id(data_root, true)?.context("installation identity was not created")
}

/// Loads the root identity without creating it or the data root.
pub(crate) fn existing_installation_id(data_root: &Path) -> Result<Option<String>> {
    access_installation_id(data_root, false)
}

pub fn install_path(data_root: &Path) -> PathBuf {
    ProFilesystemLayout::new(data_root).installation_id_path()
}

fn access_installation_id(data_root: &Path, create: bool) -> Result<Option<String>> {
    validate_identity_root_path(data_root)?;
    if create {
        prepare_identity_root(data_root)?;
    } else {
        match fs::symlink_metadata(data_root) {
            Ok(_) => validate_identity_root(data_root)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("inspect installation identity root"),
        }
    }

    let path = install_path(data_root);
    let Some(mut file) = open_identity_file(&path, create)? else {
        return Ok(None);
    };
    file.lock_exclusive()
        .context("lock installation identity")?;
    let result = access_locked_identity(&path, data_root, &mut file, create);
    let unlock = fs2::FileExt::unlock(&file).context("unlock installation identity");
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(Some(value)),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn prepare_identity_root(data_root: &Path) -> Result<()> {
    validate_identity_root_path(data_root)?;
    fs::create_dir_all(data_root).context("create installation identity root")?;
    restrict_private_directory(data_root).context("protect installation identity root")?;
    validate_identity_root(data_root)
}

fn validate_identity_root(data_root: &Path) -> Result<()> {
    validate_identity_root_path(data_root)?;
    let metadata = fs::symlink_metadata(data_root).context("inspect installation identity root")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("installation identity root is not a safe directory");
    }
    verify_private_directory(data_root).context("verify installation identity root")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("installation identity root ownership is unsafe");
        }
    }
    Ok(())
}

fn validate_identity_root_path(data_root: &Path) -> Result<()> {
    if !data_root.is_absolute()
        || data_root.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        bail!("installation identity root must be a safe absolute path");
    }
    Ok(())
}

fn open_identity_file(path: &Path, create: bool) -> Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("open installation identity"),
    }
}

fn access_locked_identity(
    path: &Path,
    data_root: &Path,
    file: &mut fs::File,
    create: bool,
) -> Result<String> {
    restrict_private_file(path).context("protect installation identity")?;
    verify_private_file(path).context("verify installation identity")?;
    verify_open_identity(path, file)?;
    let length = file.metadata()?.len();
    if length > MAX_INSTALLATION_IDENTITY_BYTES {
        bail!("installation identity exceeds its size bound");
    }
    if length == 0 {
        if !create {
            bail!("installation identity is incomplete");
        }
        let record = InstallationIdentityRecord {
            schema_version: 1,
            install_id: Uuid::new_v4().to_string(),
            created_at: utc_now().to_rfc3339(),
        };
        let body = serde_json::to_vec_pretty(&record)?;
        file.write_all(&body)
            .context("write installation identity")?;
        file.sync_all().context("sync installation identity")?;
        sync_identity_root(data_root)?;
        return Ok(record.install_id);
    }

    file.rewind().context("rewind installation identity")?;
    let mut body = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    file.take(MAX_INSTALLATION_IDENTITY_BYTES.saturating_add(1))
        .read_to_end(&mut body)
        .context("read installation identity")?;
    if body.len() as u64 > MAX_INSTALLATION_IDENTITY_BYTES {
        bail!("installation identity exceeds its size bound");
    }
    let record: InstallationIdentityRecord =
        serde_json::from_slice(&body).context("parse installation identity")?;
    validate_installation_record(record)
}

fn validate_installation_record(record: InstallationIdentityRecord) -> Result<String> {
    let parsed = Uuid::parse_str(&record.install_id).context("parse opaque installation ID")?;
    if record.schema_version != 1
        || parsed.is_nil()
        || parsed.hyphenated().to_string() != record.install_id
        || record.created_at.is_empty()
        || record.created_at.len() > 128
    {
        bail!("installation identity record is invalid");
    }
    Ok(record.install_id)
}

#[cfg(unix)]
fn verify_open_identity(path: &Path, file: &fs::File) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.nlink() != 1
        || opened.uid() != unsafe { libc::geteuid() }
    {
        bail!("installation identity changed while it was opened");
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_open_identity(_path: &Path, _file: &fs::File) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn sync_identity_root(data_root: &Path) -> Result<()> {
    fs::File::open(data_root)?
        .sync_all()
        .context("sync installation identity root")
}

#[cfg(windows)]
fn sync_identity_root(data_root: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .write(true)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
        .open(data_root)?
        .sync_all()
        .context("sync installation identity root")
}

pub fn device_id(data_root: &Path) -> Result<String> {
    let path = device_path(data_root)?;
    device_id_at_path(&path)
}

fn device_id_at_path(path: &Path) -> Result<String> {
    if path.exists() {
        let mut value: serde_json::Value = serde_json::from_slice(
            &fs::read(path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        if let Some(id) = value
            .get("device_id")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        {
            if let Ok(parsed) = Uuid::parse_str(id.trim()) {
                if !parsed.is_nil() {
                    let canonical = parsed.hyphenated().to_string();
                    if id != canonical {
                        value["device_id"] = json!(&canonical);
                        let body = serde_json::to_vec_pretty(&value)?;
                        write_private_file(path, &body)
                            .with_context(|| format!("write {}", path.display()))?;
                    }
                    return Ok(canonical);
                }
            }
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let id = Uuid::new_v4().to_string();
    let body = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "device_id": id,
        "created_at": utc_now(),
    }))?;
    write_private_file(path, &body).with_context(|| format!("write {}", path.display()))?;
    Ok(id)
}

pub fn device_path(data_root: &Path) -> Result<PathBuf> {
    device_state_path(DEVICE_FILE, data_root)
}

pub(crate) fn device_state_path(file_name: &str, data_root: &Path) -> Result<PathBuf> {
    let path = device_state_dir()?.join(file_name);
    ensure_device_path_outside_data_root(&path, data_root)?;
    Ok(path)
}

pub(crate) fn ensure_device_path_outside_data_root(path: &Path, data_root: &Path) -> Result<()> {
    let normalized_path = normalize_for_prefix_check(path);
    let normalized_data_root = normalize_for_prefix_check(data_root);
    let resolved_path = resolve_for_prefix_check(&normalized_path)?;
    let resolved_data_root = resolve_for_prefix_check(&normalized_data_root)?;
    if normalized_path.starts_with(&normalized_data_root)
        || resolved_path.starts_with(&resolved_data_root)
    {
        bail!(
            "refusing to store telemetry state under ctx data root: {}",
            path.display()
        );
    }
    Ok(())
}

fn resolve_for_prefix_check(path: &Path) -> Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .context("resolve telemetry state path")?
                    .to_os_string();
                missing.push(name);
                existing = existing
                    .parent()
                    .context("resolve telemetry state parent")?
                    .to_path_buf();
            }
            Err(err) => return Err(err).with_context(|| format!("inspect {}", existing.display())),
        }
    }
    let mut resolved =
        fs::canonicalize(&existing).with_context(|| format!("resolve {}", existing.display()))?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

pub(crate) fn normalize_for_prefix_check(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn device_state_dir() -> Result<PathBuf> {
    if let Some(local_app_data) = non_empty_env_path("LOCALAPPDATA") {
        return Ok(local_app_data.join("ctx"));
    }
    Ok(home_dir()
        .context("resolve home directory")?
        .join("AppData")
        .join("Local")
        .join("ctx"))
}

#[cfg(target_os = "macos")]
pub(crate) fn device_state_dir() -> Result<PathBuf> {
    Ok(home_dir()
        .context("resolve home directory")?
        .join("Library")
        .join("Application Support")
        .join("ctx"))
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
pub(crate) fn device_state_dir() -> Result<PathBuf> {
    if let Some(xdg_state_home) = non_empty_env_path("XDG_STATE_HOME") {
        return Ok(xdg_state_home.join("ctx"));
    }
    Ok(home_dir()
        .context("resolve home directory")?
        .join(".local")
        .join("state")
        .join("ctx"))
}

pub(crate) fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolve the user home directory from `HOME`, falling back to the
/// Windows `USERPROFILE` and `HOMEDRIVE`+`HOMEPATH` conventions.
pub(crate) fn home_dir() -> Option<PathBuf> {
    non_empty_env_path("HOME")
        .or_else(|| non_empty_env_path("USERPROFILE"))
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            Some(PathBuf::from(format!(
                "{}{}",
                drive.to_string_lossy(),
                path.to_string_lossy()
            )))
        })
}

#[cfg(unix)]
pub(crate) fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    use std::{
        fs::OpenOptions,
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(body)?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    use std::{fs::OpenOptions, io::Write, os::windows::fs::OpenOptionsExt};

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_type().is_symlink() {
        bail!("refusing to write telemetry state through a symlink");
    }
    file.set_len(0)?;
    file.write_all(body)?;
    Ok(())
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("refusing to write telemetry state through a symlink");
    }
    fs::write(path, body)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn create_private_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(body)
}

#[cfg(not(unix))]
pub(crate) fn create_private_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::{fs::OpenOptions, io::Write};

    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(body)
}

#[cfg(test)]
mod installation_identity_tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn concurrent_first_use_publishes_one_stable_identity() {
        const WORKERS: usize = 12;
        let root = tempfile::tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(WORKERS));
        let mut threads = Vec::new();
        for _ in 0..WORKERS {
            let path = root.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                installation_id(&path).unwrap()
            }));
        }
        let ids = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 1);
        assert_eq!(
            existing_installation_id(root.path()).unwrap(),
            ids.into_iter().next()
        );
    }

    #[test]
    fn moving_a_complete_data_root_preserves_identity() {
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("original");
        fs::create_dir(&original).unwrap();
        let id = installation_id(&original).unwrap();
        let moved = parent.path().join("moved");
        fs::rename(&original, &moved).unwrap();
        assert_eq!(installation_id(&moved).unwrap(), id);
        assert!(!original.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_identity_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"outside").unwrap();
        symlink(outside.path(), install_path(root.path())).unwrap();
        assert!(installation_id(root.path()).is_err());
        assert_eq!(fs::read(outside.path()).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn root_aliases_and_traversal_are_rejected() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).unwrap();
        let alias = parent.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(installation_id(&alias).is_err());
        assert!(installation_id(&root.join("..").join("root")).is_err());
    }

    #[test]
    fn corrupt_or_oversize_identity_fails_closed_without_replacement() {
        for bytes in [b"not-json".to_vec(), vec![b'x'; 1025]] {
            let root = tempfile::tempdir().unwrap();
            let path = install_path(root.path());
            fs::write(&path, &bytes).unwrap();
            assert!(installation_id(root.path()).is_err());
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn device_identity_canonicalizes_parseable_legacy_text() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(DEVICE_FILE);
        fs::write(
            &path,
            br#"{
                "schema_version": 1,
                "device_id": "550E8400E29B11D4A716446655440000",
                "created_at": "preserved"
            }"#,
        )
        .unwrap();

        let id = device_id_at_path(&path).unwrap();

        assert_eq!(id, "550e8400-e29b-11d4-a716-446655440000");
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted["device_id"], id);
        assert_eq!(persisted["created_at"], "preserved");
    }

    #[test]
    fn nil_device_identity_is_replaced_with_canonical_non_nil_uuid() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(DEVICE_FILE);
        fs::write(
            &path,
            br#"{
                "schema_version": 1,
                "device_id": "00000000-0000-0000-0000-000000000000",
                "created_at": "old"
            }"#,
        )
        .unwrap();

        let id = device_id_at_path(&path).unwrap();
        let parsed = Uuid::parse_str(&id).unwrap();

        assert!(!parsed.is_nil());
        assert_eq!(id, parsed.hyphenated().to_string());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted["device_id"], id);
        assert_ne!(persisted["created_at"], "old");
    }
}

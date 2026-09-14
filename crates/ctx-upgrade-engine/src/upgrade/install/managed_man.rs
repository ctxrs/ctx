//! Best-effort refresh for man pages installed by the hosted installer.
//!
//! Only a valid installer receipt authorizes changes. Any missing, malformed,
//! stale, or modified state is left alone.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    lock::InstallationLock,
    lock_fs::{read_stable_file, StableFileKind},
    marker, ManagedInstallMarker,
};
use crate::upgrade::{platform_key, sha256_hex, state::atomic_write_json};

const RECEIPT_KEY: &str = "man_pages";
const RECEIPT_SCHEMA: u64 = 1;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MAX_MAN_PAGE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedManPage {
    /// The complete leaf file name, including `.1`.
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedManBundle {
    pub pages: Vec<ManagedManPage>,
}

#[derive(Clone, Debug)]
struct Receipt {
    directory: PathBuf,
    files: BTreeMap<String, String>,
    binary_sha256: String,
}

#[derive(Clone, Debug)]
struct DesiredPage {
    bytes: Vec<u8>,
    digest: String,
}

enum ReceiptState {
    Disabled,
    Installed(Receipt),
}

/// Refresh installer-owned man pages after an installer-managed binary changes.
/// Failures are intentionally silent because man pages are optional.
pub fn reconcile_current_man_pages<F>(generate: F)
where
    F: FnOnce() -> Result<ManagedManBundle>,
{
    #[cfg(windows)]
    let _ = generate;
    #[cfg(not(windows))]
    {
        let Ok(executable) = super::current_install_path() else {
            return;
        };
        let Ok(platform) = platform_key() else {
            return;
        };
        let _ = reconcile_at(&executable, platform, generate);
    }
}

/// Persist the hosted installer's explicit opt-out under the installation lock.
pub fn disable_current_man_pages() -> Result<()> {
    #[cfg(windows)]
    return Ok(());

    #[cfg(not(windows))]
    {
        let executable = super::current_install_path()?;
        let platform = platform_key()?;
        disable_at(&executable, platform)
    }
}

#[cfg(not(windows))]
fn disable_at(executable: &Path, platform: &str) -> Result<()> {
    let _lock = InstallationLock::acquire(executable)?;
    let ManagedInstallMarker::Valid(marker_state) =
        marker::classify_install_marker_at(executable, platform)
    else {
        bail!("ctx does not have a valid managed install marker");
    };
    let marker_path = marker::install_marker_path(&marker_state.install_path);
    let mut marker_value = read_marker_value(&marker_path)?;
    let Some(object) = marker_value.as_object_mut() else {
        bail!("ctx install marker is not an object");
    };
    if !object.contains_key(RECEIPT_KEY) {
        return Ok(());
    }
    object.insert(
        RECEIPT_KEY.to_owned(),
        json!({"schema_version": RECEIPT_SCHEMA, "status": "disabled"}),
    );
    atomic_write_json(&marker_path, &marker_value)
}

fn reconcile_at(
    executable: &Path,
    platform: &str,
    generate: impl FnOnce() -> Result<ManagedManBundle>,
) -> Result<bool> {
    if platform.starts_with("windows-") || !should_attempt(executable) {
        return Ok(false);
    }
    let ManagedInstallMarker::Valid(marker_state) =
        marker::classify_install_marker_at(executable, platform)
    else {
        return Ok(false);
    };
    let Some(_lock) = InstallationLock::try_acquire(&marker_state.install_path)? else {
        return Ok(false);
    };
    let ManagedInstallMarker::Valid(marker_state) =
        marker::classify_install_marker_at(executable, platform)
    else {
        return Ok(false);
    };

    let marker_path = marker::install_marker_path(&marker_state.install_path);
    let mut marker_value = read_marker_value(&marker_path)?;
    let Some(ReceiptState::Installed(mut receipt)) = marker_value
        .get(RECEIPT_KEY)
        .and_then(|value| parse_receipt(value).ok())
    else {
        return Ok(false);
    };
    if receipt
        .binary_sha256
        .eq_ignore_ascii_case(&marker_state.sha256)
    {
        return Ok(false);
    }

    let refreshed = match refresh_pages(&receipt, generate) {
        Ok(files) => {
            receipt.files = files;
            true
        }
        Err(_) => false,
    };
    // The existing digest doubles as the attempt watermark. Advancing it even
    // after a silent failure keeps optional man-page work off later commands.
    receipt.binary_sha256 = marker_state.sha256;
    put_receipt(&mut marker_value, &receipt)?;
    atomic_write_json(&marker_path, &marker_value)?;
    Ok(refreshed)
}

fn refresh_pages(
    receipt: &Receipt,
    generate: impl FnOnce() -> Result<ManagedManBundle>,
) -> Result<BTreeMap<String, String>> {
    preflight_recorded(receipt)?;
    let desired = normalize_bundle(generate()?)?;
    for name in desired.keys() {
        if !receipt.files.contains_key(name) && digest_at(&receipt.directory.join(name))?.is_some()
        {
            bail!("unowned man-page destination exists");
        }
    }

    for (name, page) in &desired {
        let path = receipt.directory.join(name);
        match receipt.files.get(name) {
            Some(old_digest) if old_digest.eq_ignore_ascii_case(&page.digest) => {}
            Some(old_digest) => {
                if digest_at(&path)?.as_deref() != Some(old_digest.as_str()) {
                    bail!("recorded man page changed");
                }
                write_page(&path, &page.bytes, true)?;
            }
            None => {
                if digest_at(&path)?.is_some() {
                    bail!("unowned man-page destination exists");
                }
                write_page(&path, &page.bytes, false)?;
            }
        }
    }

    for (name, old_digest) in &receipt.files {
        if desired.contains_key(name) {
            continue;
        }
        let path = receipt.directory.join(name);
        if digest_at(&path)?.as_deref() != Some(old_digest.as_str())
            || fs::remove_file(&path).is_err()
        {
            bail!("obsolete man page changed or could not be removed");
        }
    }
    Ok(desired
        .into_iter()
        .map(|(name, page)| (name, page.digest))
        .collect())
}

fn should_attempt(executable: &Path) -> bool {
    let marker_path = marker::install_marker_path(executable);
    let Ok(Some(bytes)) = read_stable_file(
        &marker_path,
        "ctx install marker",
        MAX_MARKER_BYTES,
        StableFileKind::Data,
    ) else {
        return false;
    };
    let Ok(marker) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(binary_sha256) = marker
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|value| valid_sha256(value))
    else {
        return false;
    };
    matches!(
        marker
            .get(RECEIPT_KEY)
            .and_then(|value| parse_receipt(value).ok()),
        Some(ReceiptState::Installed(receipt))
            if !receipt.binary_sha256.eq_ignore_ascii_case(binary_sha256)
    )
}

fn preflight_recorded(receipt: &Receipt) -> Result<()> {
    let directory = canonical_directory(&receipt.directory)?;
    for (name, digest) in &receipt.files {
        if digest_at(&directory.join(name))?.as_deref() != Some(digest.as_str()) {
            bail!("recorded man page changed");
        }
    }
    Ok(())
}

fn write_page(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("managed man-page target has no parent"))?;
    let temporary = parent.join(format!(".ctx-man-{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o644);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o644))
                .with_context(|| format!("set mode on {}", temporary.display()))?;
        }
        file.write_all(bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        drop(file);
        if replace {
            fs::rename(&temporary, path).with_context(|| format!("rename {}", path.display()))?;
        } else {
            fs::hard_link(&temporary, path)
                .with_context(|| format!("publish {}", path.display()))?;
            fs::remove_file(&temporary)
                .with_context(|| format!("remove {}", temporary.display()))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn normalize_bundle(bundle: ManagedManBundle) -> Result<BTreeMap<String, DesiredPage>> {
    if bundle.pages.is_empty() {
        bail!("generated managed man-page bundle has no pages");
    }
    let mut pages = BTreeMap::new();
    for page in bundle.pages {
        if !safe_name(&page.name) || page.bytes.len() as u64 > MAX_MAN_PAGE_BYTES {
            bail!("generated managed man-page bundle has an unsafe page");
        }
        let digest = sha256_hex(&page.bytes);
        if pages
            .insert(
                page.name,
                DesiredPage {
                    bytes: page.bytes,
                    digest,
                },
            )
            .is_some()
        {
            bail!("generated managed man-page bundle repeats a page name");
        }
    }
    Ok(pages)
}

fn parse_receipt(value: &Value) -> Result<ReceiptState> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("man-page receipt is not an object"))?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(RECEIPT_SCHEMA) {
        bail!("man-page receipt has an unsupported schema");
    }
    match string(object, "status")?.as_str() {
        "disabled" => Ok(ReceiptState::Disabled),
        "installed" => {
            let directory = PathBuf::from(string(object, "directory")?);
            if !directory.is_absolute() {
                bail!("man-page receipt directory is not absolute");
            }
            let mut files = BTreeMap::new();
            for file in object
                .get("files")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("man-page receipt lacks files"))?
            {
                let file = file
                    .as_object()
                    .ok_or_else(|| anyhow!("man-page receipt file is not an object"))?;
                let name = string(file, "name")?;
                let digest = string(file, "sha256")?;
                if !safe_name(&name)
                    || !valid_sha256(&digest)
                    || files.insert(name, digest.to_ascii_lowercase()).is_some()
                {
                    bail!("man-page receipt has an unsafe file entry");
                }
            }
            let binary_sha256 = string(object, "binary_sha256")?;
            if files.is_empty() || !valid_sha256(&binary_sha256) {
                bail!("man-page receipt is incomplete");
            }
            Ok(ReceiptState::Installed(Receipt {
                directory,
                files,
                binary_sha256: binary_sha256.to_ascii_lowercase(),
            }))
        }
        _ => bail!("man-page receipt has an unsupported status"),
    }
}

fn put_receipt(marker: &mut Value, receipt: &Receipt) -> Result<()> {
    let object = marker
        .as_object_mut()
        .ok_or_else(|| anyhow!("ctx install marker is not an object"))?;
    object.insert(
        RECEIPT_KEY.to_owned(),
        json!({
            "schema_version": RECEIPT_SCHEMA,
            "status": "installed",
            "directory": receipt.directory,
            "files": receipt.files.iter().map(|(name, digest)| json!({"name": name, "sha256": digest})).collect::<Vec<_>>(),
            "binary_sha256": receipt.binary_sha256,
        }),
    );
    Ok(())
}

fn read_marker_value(path: &Path) -> Result<Value> {
    let bytes = read_stable_file(
        path,
        "ctx install marker",
        MAX_MARKER_BYTES,
        StableFileKind::Data,
    )?
    .ok_or_else(|| anyhow!("ctx install marker disappeared"))?;
    serde_json::from_slice(&bytes).context("parse ctx install marker")
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("open recorded man-page directory {}", path.display()))?;
    let metadata = fs::symlink_metadata(&canonical)?;
    if canonical != path || !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("recorded man-page directory is not a real canonical directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            bail!("recorded man-page directory is not owner-safe");
        }
    }
    Ok(canonical)
}

fn digest_at(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MAN_PAGE_BYTES
    {
        bail!("{} is not a regular managed man page", path.display());
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() as u64 > MAX_MAN_PAGE_BYTES {
        bail!("{} exceeds the managed man-page size bound", path.display());
    }
    Ok(Some(sha256_hex(&bytes)))
}

fn safe_name(name: &str) -> bool {
    name.starts_with("ctx")
        && name.ends_with(".1")
        && name.len() <= 160
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn string(object: &Map<String, Value>, key: &str) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("man-page receipt lacks {key}"))
}

#[cfg(all(test, unix))]
#[path = "managed_man_tests.rs"]
mod tests;

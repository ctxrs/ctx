//! Fail-closed retirement of exact obsolete v0.25 Store leaves.

use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    path::Path,
};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

const SQLITE_SUFFIXES: &[&str] = &["", "-wal", "-shm", "-journal"];
const LOCK_DATABASE_SUFFIXES: &[&str] = &[
    ".event-search-bulk.lock.sqlite",
    ".source-inventory.lock.sqlite",
    ".migration.lock.sqlite",
];
const SEMANTIC_WORKER_FILES: &[&str] = &[
    "semantic-worker.lock",
    "semantic-worker.guard",
    "semantic-worker.json",
];

#[cfg(test)]
#[path = "old_store_retirement_tests.rs"]
mod tests;

#[cfg(any(unix, windows))]
type FileIdentity = (u64, u64, u64);
#[cfg(not(any(unix, windows)))]
type FileIdentity = ();

pub(super) fn retire(data_root: &Path) -> Result<()> {
    retire_with(data_root, || {})
}

pub(super) fn is_required(data_root: &Path) -> Result<bool> {
    for name in candidate_names(data_root)? {
        match fs::symlink_metadata(data_root.join(name)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect old Store root {}", data_root.display()))
            }
        }
    }
    Ok(false)
}

fn retire_with(data_root: &Path, after_preflight: impl FnOnce()) -> Result<()> {
    let names = candidate_names(data_root)?;

    let mut plan = Vec::new();
    for name in names {
        let path = data_root.join(&name);
        let Some(identity) = inspect_file(&path)? else {
            continue;
        };
        if probe_name_is_exact(&name) && link_count(identity) != 1 {
            continue;
        }
        if link_count(identity) != 1 {
            bail!(
                "refusing hard-linked old Store file {} with {} links",
                path.display(),
                link_count(identity)
            );
        }
        plan.push((path, identity));
    }

    after_preflight();
    for (path, identity) in &plan {
        revalidate(path, *identity)?;
    }
    let mut removed_any = false;
    for (path, identity) in &plan {
        if revalidate(path, *identity)? {
            fs::remove_file(path)
                .with_context(|| format!("retire old Store file {}", path.display()))?;
            removed_any = true;
        }
    }
    #[cfg(unix)]
    if removed_any {
        fs::File::open(data_root)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync old Store retirement root {}", data_root.display()))?;
    }
    #[cfg(not(unix))]
    let _ = removed_any;
    Ok(())
}

fn candidate_names(data_root: &Path) -> Result<BTreeSet<OsString>> {
    let mut names = fixed_file_names();
    for entry in fs::read_dir(data_root)
        .with_context(|| format!("inventory old Store root {}", data_root.display()))?
    {
        let name = entry?.file_name();
        if dynamic_file_name_is_exact(&name) {
            names.insert(name);
        }
    }
    Ok(names)
}

fn inspect_file(path: &Path) -> Result<Option<FileIdentity>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect old Store file {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("refusing non-regular old Store file {}", path.display());
    }
    Ok(Some(file_identity(path, &metadata)?))
}

fn revalidate(path: &Path, expected: FileIdentity) -> Result<bool> {
    let Some(identity) = inspect_file(path)? else {
        return Ok(false);
    };
    if identity != expected || link_count(identity) != 1 {
        bail!("refusing changed old Store file {}", path.display());
    }
    Ok(true)
}

#[cfg(unix)]
fn file_identity(_path: &Path, metadata: &fs::Metadata) -> Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    Ok((metadata.dev(), metadata.ino(), metadata.nlink()))
}

#[cfg(windows)]
fn file_identity(path: &Path, metadata: &fs::Metadata) -> Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let file = fs::File::open(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("inspect old Store file identity");
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("refusing reparse-point old Store file {}", path.display());
    }
    let _ = metadata;
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow),
        u64::from(info.nNumberOfLinks),
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_path: &Path, _metadata: &fs::Metadata) -> Result<FileIdentity> {
    bail!("old Store retirement cannot verify file identity on this platform")
}

#[cfg(any(unix, windows))]
fn link_count(identity: FileIdentity) -> u64 {
    identity.2
}

#[cfg(not(any(unix, windows)))]
fn link_count(_identity: FileIdentity) -> u64 {
    0
}

fn fixed_file_names() -> BTreeSet<OsString> {
    let mut names = BTreeSet::new();
    add_sqlite_family(&mut names, "work.sqlite");
    for suffix in LOCK_DATABASE_SUFFIXES {
        add_sqlite_family(&mut names, &format!("work.sqlite{suffix}"));
    }
    add_sqlite_family(&mut names, "vectors.sqlite");
    names.insert(OsString::from("work.sqlite.ctx-native-cold.lock"));
    names.extend(SEMANTIC_WORKER_FILES.iter().map(OsString::from));
    names
}

fn add_sqlite_family(names: &mut BTreeSet<OsString>, base: &str) {
    names.extend(
        SQLITE_SUFFIXES
            .iter()
            .map(|suffix| OsString::from(format!("{base}{suffix}"))),
    );
}

fn dynamic_file_name_is_exact(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.strip_prefix("semantic-worker.json.")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(decimal_pid)
        || cold_stage_name_is_exact(name)
        || probe_name_is_exact(OsStr::new(name))
}

fn cold_stage_name_is_exact(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("work.sqlite.ctx-native-cold-") else {
        return false;
    };
    let Some((uuid, suffix)) = rest.split_at_checked(36) else {
        return false;
    };
    canonical_uuid_v4(uuid)
        && suffix
            .strip_prefix(".sqlite")
            .is_some_and(sqlite_family_suffix_is_exact)
}

fn sqlite_family_suffix_is_exact(suffix: &str) -> bool {
    SQLITE_SUFFIXES.contains(&suffix)
        || LOCK_DATABASE_SUFFIXES.iter().any(|lock| {
            suffix
                .strip_prefix(lock)
                .is_some_and(|sidecar| SQLITE_SUFFIXES.contains(&sidecar))
        })
}

fn probe_name_is_exact(name: &OsStr) -> bool {
    let Some(rest) = name
        .to_str()
        .and_then(|name| name.strip_prefix("work.sqlite.ctx-native-cold-probe-"))
    else {
        return false;
    };
    let Some((uuid, suffix)) = rest.split_at_checked(36) else {
        return false;
    };
    canonical_uuid_v4(uuid) && matches!(suffix, ".source" | ".target")
}

fn canonical_uuid_v4(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|uuid| {
        uuid.get_version_num() == 4
            && uuid.get_variant() == uuid::Variant::RFC4122
            && uuid.hyphenated().to_string() == value
    })
}

fn decimal_pid(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

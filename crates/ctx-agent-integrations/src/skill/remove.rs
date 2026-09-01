use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    ffi::{CString, OsStr, OsString},
    io::Read as _,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::{AsRawFd as _, FromRawFd as _},
    },
    path::Component,
};

#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

use anyhow::{anyhow, Context, Result};
#[cfg(unix)]
use fs2::FileExt as _;
#[cfg(unix)]
use uuid::Uuid;

#[cfg(not(unix))]
use super::install::read_optional_regular_file;
use super::{
    install::{
        include_installed_targets, legacy_skill_dir, metadata_manages_hash,
        metadata_manages_legacy_hash, status_target, validate_directory_path,
    },
    paths::sha256_hex,
    selection::SkillAgentSelection,
    target::{resolve_targets_for_agents, SkillTarget},
    SkillInstallStatus, SkillMetadata, LEGACY_BUNDLED_SKILL_HASHES, METADATA_FILE,
};
#[cfg(not(unix))]
use crate::filesystem::atomic_remove_if_unchanged;

#[derive(Debug, Clone)]
pub struct SkillRemoveRequest {
    pub selection: SkillAgentSelection,
    pub project: bool,
    pub force: bool,
}

#[derive(Debug)]
pub struct SkillRemoveReceipt {
    pub project: bool,
    pub selection: SkillAgentSelection,
    pub results: Vec<SkillRemoveResult>,
    pub failed: usize,
    pub removed_targets: usize,
}

#[derive(Debug)]
pub struct SkillRemoveResult {
    pub target: SkillTarget,
    pub success: bool,
    pub previous_status: SkillInstallStatus,
    pub status: SkillInstallStatus,
    pub already_absent: bool,
    pub removed: bool,
    pub removed_current: bool,
    pub removed_legacy: bool,
    pub force_required: bool,
    pub error: Option<String>,
}

struct TargetPreflight {
    previous_status: SkillInstallStatus,
    current: Option<SkillCopySnapshot>,
    legacy: Option<SkillCopySnapshot>,
}

struct SkillCopySnapshot {
    path: PathBuf,
    body: Vec<u8>,
    owned: bool,
    managed_metadata: Option<MetadataSnapshot>,
    directory: DirectoryFence,
}

struct MetadataSnapshot {
    body: Vec<u8>,
}

pub fn execute_remove(
    request: SkillRemoveRequest,
    context: &super::PathContext,
) -> Result<SkillRemoveReceipt> {
    let selection = include_installed_targets(request.selection, request.project, context)?;
    let targets = resolve_targets_for_agents(&selection.agents, request.project, context)?;
    let results = targets
        .iter()
        .map(|target| {
            remove_target(target, request.force)
                .unwrap_or_else(|error| target_failure(target, format!("{error:#}")))
        })
        .collect::<Vec<_>>();
    Ok(SkillRemoveReceipt {
        project: request.project,
        failed: results.iter().filter(|result| !result.success).count(),
        removed_targets: results.iter().filter(|result| result.removed).count(),
        selection,
        results,
    })
}

pub fn remove_target(target: &SkillTarget, force: bool) -> Result<SkillRemoveResult> {
    let preflight = preflight_target(target)?;
    let already_absent = preflight.current.is_none() && preflight.legacy.is_none();
    let unowned = [
        ("current", preflight.current.as_ref()),
        ("legacy", preflight.legacy.as_ref()),
    ]
    .into_iter()
    .filter_map(|(label, snapshot)| snapshot.filter(|snapshot| !snapshot.owned).map(|_| label))
    .collect::<Vec<_>>();
    if !force && !unowned.is_empty() {
        return Ok(remove_failure(
            target,
            preflight.previous_status,
            false,
            false,
            true,
            format!(
                "preserved unowned {} skill snapshot(s); use --force to remove the exact SKILL.md file(s)",
                unowned.join(" and ")
            ),
        ));
    }

    let mut removed_current = false;
    let mut removed_legacy = false;
    let removal = (|| -> Result<()> {
        if let Some(snapshot) = &preflight.current {
            removed_current = remove_snapshot(snapshot)?;
        }
        if let Some(snapshot) = &preflight.legacy {
            removed_legacy = remove_snapshot(snapshot)?;
        }
        Ok(())
    })();
    if let Err(error) = removal {
        return Ok(remove_failure(
            target,
            preflight.previous_status,
            removed_current,
            removed_legacy,
            false,
            format!("{error:#}"),
        ));
    }

    let status = reinspect_status(target);
    if status != SkillInstallStatus::Missing {
        return Ok(remove_failure(
            target,
            preflight.previous_status,
            removed_current,
            removed_legacy,
            false,
            "skill target changed during removal".to_owned(),
        ));
    }

    Ok(SkillRemoveResult {
        target: target.clone(),
        success: true,
        previous_status: preflight.previous_status,
        status,
        already_absent,
        removed: removed_current || removed_legacy,
        removed_current,
        removed_legacy,
        force_required: false,
        error: None,
    })
}

fn preflight_target(target: &SkillTarget) -> Result<TargetPreflight> {
    let status = status_target(target)?;
    let legacy_dir = legacy_skill_dir(target)?;
    validate_directory_path(&target.authority_root, &target.skill_dir)?;
    validate_directory_path(&target.authority_root, &legacy_dir)?;
    let current = snapshot_copy(&target.authority_root, &target.skill_dir, false)?;
    let legacy = snapshot_copy(&target.authority_root, &legacy_dir, true)?;
    Ok(TargetPreflight {
        previous_status: status.status,
        current,
        legacy,
    })
}

fn snapshot_copy(
    authority_root: &Path,
    skill_dir: &Path,
    legacy: bool,
) -> Result<Option<SkillCopySnapshot>> {
    let Some(directory) = DirectoryFence::open_existing(authority_root, skill_dir)? else {
        return Ok(None);
    };
    let path = skill_dir.join("SKILL.md");
    let Some(body) = directory.read_optional_regular_file("SKILL.md", &path)? else {
        return Ok(None);
    };
    let hash = sha256_hex(&body);
    let metadata_path = skill_dir.join(METADATA_FILE);
    let metadata_body = directory
        .read_optional_regular_file(METADATA_FILE, &metadata_path)
        .ok()
        .flatten();
    let managed_metadata = metadata_body.and_then(|body| {
        let metadata = serde_json::from_slice::<SkillMetadata>(&body).ok()?;
        let managed = if legacy {
            metadata_manages_legacy_hash(Some(&metadata), &hash)
        } else {
            metadata_manages_hash(Some(&metadata), &hash)
        };
        managed.then_some(MetadataSnapshot { body })
    });
    let legacy_allowlisted = LEGACY_BUNDLED_SKILL_HASHES.contains(&hash.as_str());
    Ok(Some(SkillCopySnapshot {
        path,
        body,
        owned: managed_metadata.is_some() || legacy_allowlisted,
        managed_metadata,
        directory,
    }))
}

fn remove_snapshot(snapshot: &SkillCopySnapshot) -> Result<bool> {
    let removed = snapshot
        .directory
        .atomic_remove_if_unchanged("SKILL.md", &snapshot.path, &snapshot.body)
        .with_context(|| format!("remove {}", snapshot.path.display()))?;
    if let Some(metadata) = &snapshot.managed_metadata {
        // A malformed, foreign, or concurrently changed sidecar is no longer
        // strict ctx ownership evidence. Preserve it without weakening the
        // successful removal of the actual Agent Skill entry point.
        let metadata_path = snapshot.directory.path.join(METADATA_FILE);
        let _ = snapshot.directory.atomic_remove_if_unchanged(
            METADATA_FILE,
            &metadata_path,
            &metadata.body,
        );
    }
    Ok(removed)
}

fn remove_failure(
    target: &SkillTarget,
    previous_status: SkillInstallStatus,
    removed_current: bool,
    removed_legacy: bool,
    force_required: bool,
    error: String,
) -> SkillRemoveResult {
    SkillRemoveResult {
        target: target.clone(),
        success: false,
        previous_status,
        status: reinspect_status(target),
        already_absent: false,
        removed: removed_current || removed_legacy,
        removed_current,
        removed_legacy,
        force_required,
        error: Some(error),
    }
}

fn target_failure(target: &SkillTarget, error: String) -> SkillRemoveResult {
    let status = reinspect_status(target);
    SkillRemoveResult {
        target: target.clone(),
        success: false,
        previous_status: status,
        status,
        already_absent: false,
        removed: false,
        removed_current: false,
        removed_legacy: false,
        force_required: false,
        error: Some(error),
    }
}

fn reinspect_status(target: &SkillTarget) -> SkillInstallStatus {
    status_target(target).map_or(SkillInstallStatus::Modified, |status| status.status)
}

#[cfg(unix)]
struct DirectoryFence {
    path: PathBuf,
    authority_root: PathBuf,
    components: Vec<OsString>,
    identities: Vec<UnixDirectoryIdentity>,
    directory: File,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct UnixDirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
#[derive(PartialEq, Eq)]
struct UnixFileStamp {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    acl: Option<Vec<u8>>,
}

#[cfg(unix)]
struct UnixObservedFile {
    body: Vec<u8>,
    stamp: UnixFileStamp,
}

#[cfg(unix)]
impl DirectoryFence {
    fn open_existing(authority_root: &Path, path: &Path) -> Result<Option<Self>> {
        let relative = path
            .strip_prefix(authority_root)
            .map_err(|_| anyhow!("skill path escapes authority root"))?;
        let mut components = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(name) => components.push(name.to_os_string()),
                Component::CurDir => {}
                _ => return Err(anyhow!("skill path contains an unsafe component")),
            }
        }

        let Some(mut current) = open_unix_authority_root(authority_root)? else {
            return Ok(None);
        };
        let mut identities = vec![unix_directory_identity(&current)?];
        for component in &components {
            let Some(child) = open_unix_child_directory(&current, component)? else {
                return Ok(None);
            };
            identities.push(unix_directory_identity(&child)?);
            current = child;
        }
        let fence = Self {
            path: path.to_path_buf(),
            authority_root: authority_root.to_path_buf(),
            components,
            identities,
            directory: current,
        };
        fence.revalidate()?;
        Ok(Some(fence))
    }

    fn read_optional_regular_file(&self, name: &str, path: &Path) -> Result<Option<Vec<u8>>> {
        let Some(file) = self.open_regular_file(OsStr::new(name), path)? else {
            return Ok(None);
        };
        Ok(Some(read_unix_file(file, path)?.body))
    }

    fn atomic_remove_if_unchanged(&self, name: &str, path: &Path, expected: &[u8]) -> Result<bool> {
        ensure_relative_remove_supported()?;
        self.revalidate()?;
        let lock_name = format!(".{name}.ctx-agent-integrations.lock");
        let lock = self.open_lock_file(OsStr::new(&lock_name), path)?;
        lock.lock_exclusive()
            .with_context(|| format!("lock transaction {}", path.display()))?;
        self.revalidate()?;

        let name = OsStr::new(name);
        let Some(file) = self.open_regular_file(name, path)? else {
            return Ok(false);
        };
        let observed = read_unix_file(file, path)?;
        if observed.body != expected {
            return Err(anyhow!(
                "refusing to remove concurrently changed target {}",
                path.display()
            ));
        }

        let stage_name = OsString::from(format!(
            ".{}.ctx-stage-{}",
            name.to_string_lossy(),
            Uuid::new_v4()
        ));
        run_before_skill_remove_hook(path);
        rename_noreplace_at(&self.directory, name, &stage_name)
            .with_context(|| format!("stage removal of {}", path.display()))?;

        let verification = (|| -> Result<()> {
            let stage_path = self.path.join(&stage_name);
            let staged = self
                .open_regular_file(&stage_name, &stage_path)?
                .ok_or_else(|| anyhow!("staged removal target disappeared"))?;
            let staged = read_unix_file(staged, &stage_path)?;
            if staged.body != observed.body || staged.stamp != observed.stamp {
                return Err(anyhow!(
                    "refusing to remove concurrently changed target {}",
                    path.display()
                ));
            }
            if self.entry_exists(name)? {
                return Err(anyhow!(
                    "target was recreated during removal: {}",
                    path.display()
                ));
            }
            self.revalidate()?;
            Ok(())
        })();
        if let Err(error) = verification {
            return Err(self.rollback_staged(name, &stage_name, error));
        }

        unlink_at(&self.directory, &stage_name)
            .with_context(|| format!("remove displaced file for {}", path.display()))?;
        self.directory
            .sync_all()
            .with_context(|| format!("sync directory {}", self.path.display()))?;
        self.revalidate()?;
        Ok(true)
    }

    fn revalidate(&self) -> Result<()> {
        let Some(mut current) = open_unix_authority_root(&self.authority_root)? else {
            return Err(self.identity_changed());
        };
        if unix_directory_identity(&current)? != self.identities[0] {
            return Err(self.identity_changed());
        }
        for (index, component) in self.components.iter().enumerate() {
            let Some(child) = open_unix_child_directory(&current, component)? else {
                return Err(self.identity_changed());
            };
            if unix_directory_identity(&child)? != self.identities[index + 1] {
                return Err(self.identity_changed());
            }
            current = child;
        }
        if unix_directory_identity(&self.directory)? != *self.identities.last().unwrap() {
            return Err(self.identity_changed());
        }
        Ok(())
    }

    fn open_regular_file(&self, name: &OsStr, path: &Path) -> Result<Option<File>> {
        let name = unix_name(name)?;
        let descriptor = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).with_context(|| format!("open {}", path.display()));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        if !file.metadata()?.file_type().is_file() {
            return Err(anyhow!("target is not a regular file: {}", path.display()));
        }
        Ok(Some(file))
    }

    fn open_lock_file(&self, name: &OsStr, path: &Path) -> Result<File> {
        let name = unix_name(name)?;
        let descriptor = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("open transaction lock for {}", path.display()));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        if !file.metadata()?.file_type().is_file() {
            return Err(anyhow!(
                "transaction lock is not a regular file for {}",
                path.display()
            ));
        }
        Ok(file)
    }

    fn entry_exists(&self, name: &OsStr) -> Result<bool> {
        let name = unix_name(name)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error).context("inspect retained skill directory entry")
        }
    }

    fn rollback_staged(
        &self,
        name: &OsStr,
        stage_name: &OsStr,
        error: anyhow::Error,
    ) -> anyhow::Error {
        match rename_noreplace_at(&self.directory, stage_name, name) {
            Ok(()) => error,
            Err(rollback) => anyhow!(
                "{error:#}; failed to restore concurrently changed target in retained directory {}: {rollback}",
                self.path.display()
            ),
        }
    }

    fn identity_changed(&self) -> anyhow::Error {
        anyhow!(
            "skill directory identity changed during removal: {}",
            self.path.display()
        )
    }
}

#[cfg(unix)]
fn open_unix_authority_root(path: &Path) -> Result<Option<File>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY);
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("open authority root {}", path.display())),
    }
}

#[cfg(unix)]
fn open_unix_child_directory(parent: &File, name: &OsStr) -> Result<Option<File>> {
    let name = unix_name(name)?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    if descriptor >= 0 {
        return Ok(Some(unsafe { File::from_raw_fd(descriptor) }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error).context("open no-follow skill directory component")
    }
}

#[cfg(unix)]
fn unix_directory_identity(file: &File) -> Result<UnixDirectoryIdentity> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(anyhow!("skill path component is not a directory"));
    }
    Ok(UnixDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn read_unix_file(mut file: File, path: &Path) -> Result<UnixObservedFile> {
    let mut body = Vec::new();
    file.read_to_end(&mut body)
        .with_context(|| format!("read {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!("target is not a regular file: {}", path.display()));
    }
    Ok(UnixObservedFile {
        body,
        stamp: UnixFileStamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            acl: unix_acl_snapshot(&file)?,
        },
    })
}

#[cfg(target_os = "linux")]
fn unix_acl_snapshot(file: &File) -> io::Result<Option<Vec<u8>>> {
    let size = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"system.posix_acl_access".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    if size < 0 {
        let error = io::Error::last_os_error();
        return if matches!(
            error.raw_os_error(),
            Some(libc::ENODATA) | Some(libc::ENOTSUP)
        ) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let size = usize::try_from(size).map_err(|_| io::Error::other("ACL is too large"))?;
    let mut acl = vec![0_u8; size];
    if size != 0 {
        let read = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                c"system.posix_acl_access".as_ptr(),
                acl.as_mut_ptr().cast(),
                acl.len(),
            )
        };
        if usize::try_from(read).ok() != Some(size) {
            return Err(if read < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::other("ACL changed while it was read")
            });
        }
    }
    Ok(Some(acl))
}

#[cfg(target_os = "macos")]
fn unix_acl_snapshot(file: &File) -> io::Result<Option<Vec<u8>>> {
    use std::{ffi::c_void, slice};

    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut c_void;
        fn acl_to_text(acl: *mut c_void, length: *mut libc::ssize_t) -> *mut libc::c_char;
        fn acl_free(object: *mut c_void) -> libc::c_int;
    }
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let mut length = 0;
    let text = unsafe { acl_to_text(acl, &mut length) };
    let result = if text.is_null() || length < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(Some(unsafe {
            slice::from_raw_parts(text.cast::<u8>(), length as usize).to_vec()
        }))
    };
    let text_free = if text.is_null() {
        0
    } else {
        unsafe { acl_free(text.cast()) }
    };
    let acl_free_result = unsafe { acl_free(acl) };
    if text_free != 0 || acl_free_result != 0 {
        Err(io::Error::last_os_error())
    } else {
        result
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unix_acl_snapshot(_file: &File) -> io::Result<Option<Vec<u8>>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ACL observation is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn unix_name(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| anyhow!("skill path component contains a NUL"))
}

#[cfg(target_os = "linux")]
fn rename_noreplace_at(parent: &File, source: &OsStr, target: &OsStr) -> io::Result<()> {
    let source = unix_name(source).map_err(|error| io::Error::other(error.to_string()))?;
    let target = unix_name(target).map_err(|error| io::Error::other(error.to_string()))?;
    if unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace_at(parent: &File, source: &OsStr, target: &OsStr) -> io::Result<()> {
    let source = unix_name(source).map_err(|error| io::Error::other(error.to_string()))?;
    let target = unix_name(target).map_err(|error| io::Error::other(error.to_string()))?;
    if unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_noreplace_at(_parent: &File, _source: &OsStr, _target: &OsStr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic skill removal is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn unlink_at(parent: &File, name: &OsStr) -> io::Result<()> {
    let name = unix_name(name).map_err(|error| io::Error::other(error.to_string()))?;
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ensure_relative_remove_supported() -> Result<()> {
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn ensure_relative_remove_supported() -> Result<()> {
    Err(anyhow!(
        "atomic skill removal is unsupported on this platform"
    ))
}

#[cfg(windows)]
struct DirectoryFence {
    path: PathBuf,
    guards: Vec<WindowsDirectoryGuard>,
}

#[cfg(windows)]
struct WindowsDirectoryGuard {
    path: PathBuf,
    file: File,
}

#[cfg(windows)]
impl DirectoryFence {
    fn open_existing(_authority_root: &Path, path: &Path) -> Result<Option<Self>> {
        if !path.is_absolute() {
            return Err(anyhow!("skill directory must be absolute on Windows"));
        }
        let mut paths = path
            .ancestors()
            .filter(|ancestor| !ancestor.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        paths.reverse();
        let mut guards = Vec::with_capacity(paths.len());
        for guarded_path in paths {
            let Some(file) = open_windows_directory(&guarded_path)? else {
                return Ok(None);
            };
            guards.push(WindowsDirectoryGuard {
                path: guarded_path,
                file,
            });
        }
        let fence = Self {
            path: path.to_path_buf(),
            guards,
        };
        fence.revalidate()?;
        Ok(Some(fence))
    }

    fn read_optional_regular_file(&self, _name: &str, path: &Path) -> Result<Option<Vec<u8>>> {
        read_optional_regular_file(path)
    }

    fn atomic_remove_if_unchanged(
        &self,
        _name: &str,
        path: &Path,
        expected: &[u8],
    ) -> Result<bool> {
        self.revalidate()?;
        run_before_skill_remove_hook(path);
        self.revalidate()?;
        let removed = atomic_remove_if_unchanged(path, expected)?;
        self.revalidate()?;
        Ok(removed)
    }

    fn revalidate(&self) -> Result<()> {
        for guard in &self.guards {
            let Some(current) = open_windows_directory(&guard.path)? else {
                return Err(self.identity_changed());
            };
            if windows_file_identity(&current)? != windows_file_identity(&guard.file)? {
                return Err(self.identity_changed());
            }
        }
        Ok(())
    }

    fn identity_changed(&self) -> anyhow::Error {
        anyhow!(
            "skill directory identity changed during removal: {}",
            self.path.display()
        )
    }
}

#[cfg(windows)]
fn open_windows_directory(path: &Path) -> Result<Option<File>> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = OpenOptions::new();
    options
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open protected skill directory {}", path.display()))
        }
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(anyhow!(
            "skill path traverses a reparse point or non-directory: {}",
            path.display()
        ));
    }
    Ok(Some(file))
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<(u32, u64)> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok((
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

#[cfg(not(any(unix, windows)))]
struct DirectoryFence {
    path: PathBuf,
}

#[cfg(not(any(unix, windows)))]
impl DirectoryFence {
    fn open_existing(_authority_root: &Path, path: &Path) -> Result<Option<Self>> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_dir() => Ok(Some(Self {
                path: path.to_path_buf(),
            })),
            Ok(_) => Err(anyhow!(
                "skill path component is not a directory: {}",
                path.display()
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    }

    fn read_optional_regular_file(&self, _name: &str, path: &Path) -> Result<Option<Vec<u8>>> {
        read_optional_regular_file(path)
    }

    fn atomic_remove_if_unchanged(
        &self,
        _name: &str,
        path: &Path,
        expected: &[u8],
    ) -> Result<bool> {
        run_before_skill_remove_hook(path);
        atomic_remove_if_unchanged(path, expected)
    }
}

#[cfg(not(test))]
fn run_before_skill_remove_hook(_path: &Path) {}

#[cfg(test)]
fn run_before_skill_remove_hook(path: &Path) {
    BEFORE_SKILL_REMOVE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

#[cfg(test)]
type BeforeSkillRemoveHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static BEFORE_SKILL_REMOVE_HOOK: std::cell::RefCell<Option<BeforeSkillRemoveHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn with_before_skill_remove_hook<T>(
    hook: impl FnMut(&Path) + 'static,
    remove: impl FnOnce() -> T,
) -> T {
    BEFORE_SKILL_REMOVE_HOOK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let result = remove();
    BEFORE_SKILL_REMOVE_HOOK.with(|slot| {
        slot.borrow_mut().take();
    });
    result
}

#[cfg(test)]
mod tests;

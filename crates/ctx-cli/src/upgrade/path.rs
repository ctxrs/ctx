use std::{
    env,
    ffi::OsStr,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use super::version::CtxBinaryVersion;

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_PROBE_OUTPUT_LIMIT: usize = 4096;

#[derive(Debug, Clone)]
pub(super) struct PathDiagnostics {
    pub(super) current_exe: PathBuf,
    pub(super) entries: Vec<PathDiagnosticEntry>,
    pub(super) warnings: Vec<String>,
    resolver_status: PathResolverStatus,
}

#[derive(Debug, Clone)]
pub(super) struct PathDiagnosticEntry {
    pub(super) path: PathBuf,
    pub(super) version: Option<String>,
    pub(super) current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathResolverStatus {
    ManagedExecutableWins,
    Shadowed,
    ManagedExecutableNotOnPath,
    ManagedExecutableUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackgroundApplyBlockReason {
    PathShadowed,
    ManagedExecutableNotOnPath,
    ManagedExecutableUnavailable,
}

impl PathResolverStatus {
    fn code(self) -> &'static str {
        match self {
            Self::ManagedExecutableWins => "managed_executable_wins",
            Self::Shadowed => "shadowed",
            Self::ManagedExecutableNotOnPath => "managed_executable_not_on_path",
            Self::ManagedExecutableUnavailable => "managed_executable_unavailable",
        }
    }

    fn background_apply_block_reason(self) -> Option<BackgroundApplyBlockReason> {
        match self {
            Self::ManagedExecutableWins => None,
            Self::Shadowed => Some(BackgroundApplyBlockReason::PathShadowed),
            Self::ManagedExecutableNotOnPath => {
                Some(BackgroundApplyBlockReason::ManagedExecutableNotOnPath)
            }
            Self::ManagedExecutableUnavailable => {
                Some(BackgroundApplyBlockReason::ManagedExecutableUnavailable)
            }
        }
    }
}

impl BackgroundApplyBlockReason {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::PathShadowed => "path_shadowed",
            Self::ManagedExecutableNotOnPath => "managed_executable_not_on_path",
            Self::ManagedExecutableUnavailable => "managed_executable_unavailable",
        }
    }

    pub(super) fn action(self) -> &'static str {
        match self {
            Self::PathShadowed => {
                "move the managed ctx directory before the shadowing binary on PATH, or remove the shadowing binary"
            }
            Self::ManagedExecutableNotOnPath => {
                "add the managed ctx directory to PATH before enabling background apply"
            }
            Self::ManagedExecutableUnavailable => {
                "repair or reinstall the managed ctx executable before enabling background apply"
            }
        }
    }
}

impl PathDiagnostics {
    #[cfg(test)]
    pub(super) fn managed_executable_is_resolver_winner(&self) -> bool {
        self.resolver_status == PathResolverStatus::ManagedExecutableWins
    }

    pub(super) fn background_apply_block_reason(&self) -> Option<BackgroundApplyBlockReason> {
        self.resolver_status.background_apply_block_reason()
    }

    pub(super) fn json(&self) -> Value {
        let resolver_status = self.resolver_status;
        let block_reason = resolver_status.background_apply_block_reason();
        json!({
            "current_exe": self.current_exe.display().to_string(),
            "first_ctx": self.entries.first().map(|entry| entry.path.display().to_string()),
            "resolver_status": resolver_status.code(),
            "managed_executable_wins": resolver_status == PathResolverStatus::ManagedExecutableWins,
            "background_apply": {
                "allowed": block_reason.is_none(),
                "reason": block_reason.map(BackgroundApplyBlockReason::code),
                "action": block_reason.map(BackgroundApplyBlockReason::action),
            },
            "entries": self.entries.iter().map(|entry| {
                json!({
                    "path": entry.path.display().to_string(),
                    "version": entry.version.as_deref(),
                    "current": entry.current,
                })
            }).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

pub(super) fn path_diagnostics(current_exe: &Path, current_version: &str) -> PathDiagnostics {
    let path = env::var_os("PATH");
    path_diagnostics_with_path(current_exe, current_version, path.as_deref())
}

fn path_diagnostics_with_path(
    current_exe: &Path,
    current_version: &str,
    path: Option<&OsStr>,
) -> PathDiagnostics {
    let current_identity = file_identity(current_exe);
    let current_display = current_exe.display().to_string();
    let mut discovered = Vec::new();
    for dir in path
        .map(|path| env::split_paths(path).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let candidate = dir.join(ctx_binary_name());
        let Some(candidate_identity) = resolver_candidate_identity(&candidate) else {
            continue;
        };
        if discovered
            .iter()
            .any(|(_, identity): &(PathDiagnosticEntry, FileIdentity)| {
                identity.same_file(&candidate_identity)
            })
        {
            continue;
        }
        let current = current_identity
            .as_ref()
            .is_some_and(|identity| identity.same_file(&candidate_identity));
        discovered.push((
            PathDiagnosticEntry {
                version: current.then(|| format!("ctx {current_version}")),
                path: candidate,
                current,
            },
            candidate_identity,
        ));
    }
    let entries = discovered
        .into_iter()
        .map(|(entry, _)| entry)
        .collect::<Vec<_>>();

    let resolver_status = resolver_status(current_identity.is_some(), &entries);

    let mut warnings = Vec::new();
    match resolver_status {
        PathResolverStatus::Shadowed => warnings.push(format!(
            "PATH resolves ctx to {} before the current executable {}; your shell may keep using the earlier binary after upgrade",
            entries[0].path.display(),
            current_display
        )),
        PathResolverStatus::ManagedExecutableNotOnPath => warnings.push(format!(
            "current ctx executable {current_display} is not discoverable on PATH"
        )),
        PathResolverStatus::ManagedExecutableUnavailable => warnings.push(format!(
            "managed ctx executable {current_display} is not a resolvable regular file; background apply must remain disabled until the install is repaired"
        )),
        PathResolverStatus::ManagedExecutableWins => {}
    }
    if entries.len() > 1 {
        warnings.push(format!(
            "multiple ctx binaries are on PATH; first is {}",
            entries[0].path.display()
        ));
    }
    let expected = format!("ctx {current_version}");
    for entry in &entries {
        if let Some(version) = &entry.version {
            if version != &expected {
                warnings.push(format!(
                    "ctx on PATH at {} reports {version}; current binary reports {expected}",
                    entry.path.display()
                ));
            }
        }
    }

    PathDiagnostics {
        current_exe: current_exe.to_path_buf(),
        entries,
        warnings,
        resolver_status,
    }
}

fn resolver_status(
    current_executable_available: bool,
    entries: &[PathDiagnosticEntry],
) -> PathResolverStatus {
    if !current_executable_available {
        PathResolverStatus::ManagedExecutableUnavailable
    } else {
        match entries.first() {
            Some(first) if first.current => PathResolverStatus::ManagedExecutableWins,
            Some(_) => PathResolverStatus::Shadowed,
            None => PathResolverStatus::ManagedExecutableNotOnPath,
        }
    }
}

fn ctx_binary_name() -> &'static str {
    if cfg!(windows) {
        "ctx.exe"
    } else {
        "ctx"
    }
}

#[derive(Debug, Clone)]
struct FileIdentity {
    canonical_path: Option<PathBuf>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    windows_id: Option<(u32, u64)>,
}

impl FileIdentity {
    fn same_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        if self.device == other.device && self.inode == other.inode {
            return true;
        }
        #[cfg(windows)]
        if self
            .windows_id
            .zip(other.windows_id)
            .is_some_and(|(left, right)| left == right)
        {
            return true;
        }
        self.canonical_path
            .as_ref()
            .zip(other.canonical_path.as_ref())
            .is_some_and(|(left, right)| left == right)
    }
}

fn resolver_candidate_identity(path: &Path) -> Option<FileIdentity> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || !is_executable(path) {
        return None;
    }
    file_identity_from_metadata(path, &metadata)
}

fn file_identity(path: &Path) -> Option<FileIdentity> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    file_identity_from_metadata(path, &metadata)
}

fn file_identity_from_metadata(path: &Path, metadata: &fs::Metadata) -> Option<FileIdentity> {
    let canonical_path = fs::canonicalize(path).ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        Some(FileIdentity {
            canonical_path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        let windows_id = windows_file_identity(path);
        if canonical_path.is_none() && windows_id.is_none() {
            return None;
        }
        return Some(FileIdentity {
            canonical_path,
            windows_id,
        });
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        canonical_path.map(|canonical_path| FileIdentity {
            canonical_path: Some(canonical_path),
        })
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a live NUL-terminated path string.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

#[cfg(windows)]
fn windows_file_identity(path: &Path) -> Option<(u32, u64)> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let file = fs::File::open(path).ok()?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `file` owns a live handle and the out pointer is valid.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return None;
    }
    // SAFETY: a successful API call initialized the complete structure.
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Some((information.dwVolumeSerialNumber, index))
}

pub(super) fn ctx_binary_version(path: &Path) -> Result<CtxBinaryVersion> {
    let output = run_ctx_version_command(path)?;
    if !output.status.success() {
        return Err(anyhow!("{} --version failed", path.display()));
    }
    if output.truncated {
        return Err(anyhow!(
            "{} --version output exceeded {} bytes",
            path.display(),
            VERSION_PROBE_OUTPUT_LIMIT
        ));
    }
    CtxBinaryVersion::parse(&output.stdout)
        .with_context(|| format!("parse {} --version output", path.display()))
}

struct VersionCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    truncated: bool,
}

fn run_ctx_version_command(path: &Path) -> Result<VersionCommandOutput> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run {} --version", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("capture {} --version output", path.display()))?;
    let (output_tx, output_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = output_tx.send(read_capped_output(stdout, VERSION_PROBE_OUTPUT_LIMIT));
    });
    let started = Instant::now();
    let mut status = None;
    let mut output = None;
    loop {
        if status.is_none() {
            status = child
                .try_wait()
                .with_context(|| format!("wait for {} --version", path.display()))?;
        }
        if output.is_none() {
            match output_rx.try_recv() {
                Ok(result) => {
                    output =
                        Some(result.with_context(|| {
                            format!("read {} --version output", path.display())
                        })?);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(anyhow!(
                        "reader thread stopped for {} --version",
                        path.display()
                    ));
                }
            }
        }
        match (status.take(), output.take()) {
            (Some(status), Some((stdout, truncated))) => {
                return Ok(VersionCommandOutput {
                    status,
                    stdout,
                    truncated,
                });
            }
            (next_status, next_output) => {
                status = next_status;
                output = next_output;
            }
        }
        if started.elapsed() >= VERSION_PROBE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "{} --version timed out after {}ms",
                path.display(),
                VERSION_PROBE_TIMEOUT.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_capped_output(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 1024];
    while output.len() < limit {
        let remaining = limit - output.len();
        let max_read = remaining.min(buffer.len());
        let read = reader.read(&mut buffer[..max_read])?;
        if read == 0 {
            return Ok((output, false));
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok((output, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_candidate(directory: &Path) -> PathBuf {
        fs::create_dir_all(directory).unwrap();
        let path = directory.join(ctx_binary_name());
        fs::write(&path, b"not executed by path diagnostics").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn path_shadow_and_winner_have_machine_readable_apply_status() {
        let temp = tempfile::tempdir().unwrap();
        let managed_dir = temp.path().join("managed");
        let shadow_dir = temp.path().join("shadow");
        let managed = write_candidate(&managed_dir);
        let shadow = write_candidate(&shadow_dir);

        let shadow_first = env::join_paths([shadow_dir.as_path(), managed_dir.as_path()]).unwrap();
        let diagnostics =
            path_diagnostics_with_path(&managed, "1.2.3", Some(shadow_first.as_os_str()));
        assert_eq!(diagnostics.entries[0].path, shadow);
        assert!(!diagnostics.managed_executable_is_resolver_winner());
        assert_eq!(
            diagnostics.background_apply_block_reason(),
            Some(BackgroundApplyBlockReason::PathShadowed)
        );
        let json = diagnostics.json();
        assert_eq!(json["resolver_status"], "shadowed");
        assert_eq!(json["background_apply"]["allowed"], false);
        assert_eq!(json["background_apply"]["reason"], "path_shadowed");

        let managed_first = env::join_paths([managed_dir.as_path(), shadow_dir.as_path()]).unwrap();
        let diagnostics =
            path_diagnostics_with_path(&managed, "1.2.3", Some(managed_first.as_os_str()));
        assert_eq!(diagnostics.entries[0].path, managed);
        assert!(diagnostics.managed_executable_is_resolver_winner());
        assert_eq!(diagnostics.background_apply_block_reason(), None);
        let json = diagnostics.json();
        assert_eq!(json["resolver_status"], "managed_executable_wins");
        assert_eq!(json["background_apply"]["allowed"], true);
        assert!(json["background_apply"]["reason"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_to_managed_executable_is_the_same_resolver_winner() {
        let temp = tempfile::tempdir().unwrap();
        let managed_dir = temp.path().join("managed");
        let alias_dir = temp.path().join("alias");
        let managed = write_candidate(&managed_dir);
        fs::create_dir_all(&alias_dir).unwrap();
        let alias = alias_dir.join(ctx_binary_name());
        fs::hard_link(&managed, &alias).unwrap();

        let path = env::join_paths([alias_dir.as_path(), managed_dir.as_path()]).unwrap();
        let diagnostics = path_diagnostics_with_path(&managed, "1.2.3", Some(path.as_os_str()));
        assert_eq!(diagnostics.entries.len(), 1);
        assert_eq!(diagnostics.entries[0].path, alias);
        assert!(diagnostics.entries[0].current);
        assert!(diagnostics.managed_executable_is_resolver_winner());
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_path_entry_does_not_shadow_the_managed_winner() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let managed_dir = temp.path().join("managed");
        let shadow_dir = temp.path().join("not-executable");
        let managed = write_candidate(&managed_dir);
        let shadow = write_candidate(&shadow_dir);
        fs::set_permissions(&shadow, fs::Permissions::from_mode(0o644)).unwrap();

        let path = env::join_paths([shadow_dir.as_path(), managed_dir.as_path()]).unwrap();
        let diagnostics = path_diagnostics_with_path(&managed, "1.2.3", Some(path.as_os_str()));
        assert_eq!(diagnostics.entries.len(), 1);
        assert_eq!(diagnostics.entries[0].path, managed);
        assert!(diagnostics.managed_executable_is_resolver_winner());
    }

    #[cfg(unix)]
    #[test]
    fn path_diagnostics_never_executes_a_shadow_candidate() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let managed_dir = temp.path().join("managed");
        let shadow_dir = temp.path().join("shadow");
        let managed = write_candidate(&managed_dir);
        fs::create_dir_all(&shadow_dir).unwrap();
        let marker = temp.path().join("shadow-ran");
        let shadow = shadow_dir.join(ctx_binary_name());
        fs::write(
            &shadow,
            format!("#!/bin/sh\ntouch '{}'\nsleep 30\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&shadow, fs::Permissions::from_mode(0o755)).unwrap();

        let path = env::join_paths([shadow_dir.as_path(), managed_dir.as_path()]).unwrap();
        let diagnostics = path_diagnostics_with_path(&managed, "1.2.3", Some(path.as_os_str()));
        assert_eq!(
            diagnostics.background_apply_block_reason(),
            Some(BackgroundApplyBlockReason::PathShadowed)
        );
        assert!(!marker.exists());
    }
}

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom},
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Context, Result};
#[cfg(not(unix))]
use ctx_history_core::platform_security::verify_private_directory;
#[cfg(windows)]
use ctx_history_core::platform_security::verify_private_executable;
use sha2::{Digest as _, Sha256};

const AUTHORIZER_PATH_ENV: &str = "CTX_PRO_QUALIFICATION_AUTHORIZER_PATH";
const AUTHORIZER_SHA256_ENV: &str = "CTX_PRO_QUALIFICATION_AUTHORIZER_SHA256";
const AUTHORIZER_STATE_ROOT_ENV: &str = "CTX_PRO_QUALIFICATION_STATE_ROOT";
const MAX_AUTHORIZER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_AUTHORITY_PATH_BYTES: usize = 32 * 1024;

#[derive(Debug)]
pub(crate) struct QualificationAuthorizerCommand {
    executable: VerifiedAuthorizerExecutable,
    state_root: VerifiedStateRoot,
}

impl QualificationAuthorizerCommand {
    pub(crate) fn from_process_environment() -> Result<Option<Self>> {
        Self::from_values(
            std::env::var_os(AUTHORIZER_PATH_ENV),
            std::env::var_os(AUTHORIZER_SHA256_ENV),
            std::env::var_os(AUTHORIZER_STATE_ROOT_ENV),
        )
    }

    fn from_values(
        path: Option<OsString>,
        sha256: Option<OsString>,
        state_root: Option<OsString>,
    ) -> Result<Option<Self>> {
        if path.is_none() && sha256.is_none() && state_root.is_none() {
            return Ok(None);
        }
        let (Some(path), Some(sha256), Some(state_root)) = (path, sha256, state_root) else {
            bail!(
                "invalid_request: qualification authorizer path, SHA-256, and state root must be configured together"
            );
        };

        Ok(Some(Self {
            executable: VerifiedAuthorizerExecutable::open(
                &PathBuf::from(path),
                parse_sha256(sha256)?,
            )?,
            state_root: VerifiedStateRoot::open(&PathBuf::from(state_root))?,
        }))
    }

    pub(crate) fn prepare_execution(&self) -> Result<PreparedAuthorizerExecution> {
        let program = self.executable.prepare_program()?;
        self.state_root.verify()?;
        Ok(PreparedAuthorizerExecution {
            program,
            state_root: self.state_root.path.clone(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct PreparedAuthorizerExecution {
    program: PreparedProgram,
    state_root: PathBuf,
}

impl PreparedAuthorizerExecution {
    pub(crate) fn program(&self) -> &Path {
        self.program.path()
    }

    pub(crate) fn configure_command(&self, command: &mut std::process::Command) {
        command
            .env_clear()
            .env(AUTHORIZER_STATE_ROOT_ENV, &self.state_root);
        self.program.configure_command(command);
    }
}

#[derive(Debug)]
struct VerifiedAuthorizerExecutable {
    path: PathBuf,
    handle: File,
    sha256: String,
}

impl VerifiedAuthorizerExecutable {
    fn open(path: &Path, sha256: String) -> Result<Self> {
        let path = canonical_absolute_path(path, "qualification authorizer path")?;
        let handle = open_verified_authorizer(&path)?;
        verify_file_digest(&handle, &sha256)?;
        Ok(Self {
            path,
            handle,
            sha256,
        })
    }

    fn prepare_program(&self) -> Result<PreparedProgram> {
        self.verify()?;
        #[cfg(unix)]
        {
            let descriptor = self
                .handle
                .try_clone()
                .context("invalid_request: duplicate qualification authorizer descriptor")?;
            let path = descriptor_execution_path(&descriptor)?;
            Ok(PreparedProgram { path, descriptor })
        }
        #[cfg(not(unix))]
        {
            Ok(PreparedProgram {
                path: self.path.clone(),
            })
        }
    }

    fn verify(&self) -> Result<()> {
        canonical_absolute_path(&self.path, "qualification authorizer path")?;
        let named = open_verified_authorizer(&self.path)?;
        if !same_file_identity(&self.handle, &named)? {
            bail!("invalid_request: qualification authorizer changed during validation");
        }
        verify_file_digest(&named, &self.sha256)
    }
}

#[derive(Debug)]
struct PreparedProgram {
    path: PathBuf,
    #[cfg(unix)]
    descriptor: File,
}

impl PreparedProgram {
    fn path(&self) -> &Path {
        &self.path
    }

    fn configure_command(&self, command: &mut std::process::Command) {
        #[cfg(unix)]
        {
            use std::{os::fd::AsRawFd as _, os::unix::process::CommandExt as _};

            let descriptor = self.descriptor.as_raw_fd();
            // SAFETY: the descriptor is retained by `self` until after spawn; the closure only
            // clears its close-on-exec bit in the child between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(descriptor, libc::F_GETFD);
                    if flags == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(unix))]
        let _ = command;
    }
}

#[derive(Debug)]
struct VerifiedStateRoot {
    path: PathBuf,
    handle: File,
}

impl VerifiedStateRoot {
    fn open(path: &Path) -> Result<Self> {
        let path = canonical_absolute_path(path, "qualification authorizer state root")?;
        if path.parent().is_none() {
            bail!(
                "invalid_request: qualification authorizer state root must be a bounded private directory"
            );
        }
        let handle = open_verified_state_root(&path)?;
        Ok(Self { path, handle })
    }

    fn verify(&self) -> Result<()> {
        canonical_absolute_path(&self.path, "qualification authorizer state root")?;
        let named = open_verified_state_root(&self.path)?;
        if !same_file_identity(&self.handle, &named)? {
            bail!("invalid_request: qualification authorizer state root changed during validation");
        }
        Ok(())
    }
}

fn canonical_absolute_path(path: &Path, label: &str) -> Result<PathBuf> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > MAX_AUTHORITY_PATH_BYTES
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("invalid_request: {label} must be a bounded normalized absolute path");
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("invalid_request: canonicalize {label}"))?;
    if canonical != path {
        bail!("invalid_request: {label} must be canonical and contain no symlinks");
    }
    Ok(canonical)
}

fn parse_sha256(value: OsString) -> Result<String> {
    let value = value.into_string().map_err(|_| {
        anyhow::anyhow!("invalid_request: qualification authorizer SHA-256 must be valid UTF-8")
    })?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "invalid_request: qualification authorizer SHA-256 must be 64 lowercase hex characters"
        );
    }
    Ok(value)
}

fn open_verified_authorizer(path: &Path) -> Result<File> {
    let metadata =
        fs::symlink_metadata(path).context("invalid_request: inspect qualification authorizer")?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("invalid_request: qualification authorizer must be a regular non-symlink file");
    }

    let file = open_file_without_following_symlinks(path)
        .context("invalid_request: open qualification authorizer")?;
    let opened = file
        .metadata()
        .context("invalid_request: inspect opened qualification authorizer")?;
    let named = open_file_without_following_symlinks(path)
        .context("invalid_request: reopen qualification authorizer")?;
    if !opened.is_file() || !same_file_identity(&file, &named)? {
        bail!("invalid_request: qualification authorizer changed during validation");
    }
    verify_owner_private_executable(path, &opened)?;
    if opened.len() == 0 || opened.len() > MAX_AUTHORIZER_BYTES {
        bail!("invalid_request: qualification authorizer size is outside allowed bounds");
    }
    Ok(file)
}

fn open_verified_state_root(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)
        .context("invalid_request: inspect qualification authorizer state root")?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "invalid_request: qualification authorizer state root must be a regular non-symlink directory"
        );
    }

    let handle = open_directory_without_following_symlinks(path)
        .context("invalid_request: open qualification authorizer state root")?;
    let opened = handle
        .metadata()
        .context("invalid_request: inspect opened qualification authorizer state root")?;
    let named = open_directory_without_following_symlinks(path)
        .context("invalid_request: reopen qualification authorizer state root")?;
    if !opened.is_dir() || !same_file_identity(&handle, &named)? {
        bail!("invalid_request: qualification authorizer state root changed during validation");
    }
    verify_owner_private_directory(path, &opened)
        .context("invalid_request: qualification authorizer state root permissions are unsafe")?;
    Ok(handle)
}

fn verify_file_digest(file: &File, sha256: &str) -> Result<()> {
    let expected_size = file
        .metadata()
        .context("invalid_request: inspect qualification authorizer")?
        .len();
    let mut reader = file
        .try_clone()
        .context("invalid_request: duplicate qualification authorizer descriptor")?;
    reader
        .seek(SeekFrom::Start(0))
        .context("invalid_request: seek qualification authorizer")?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("invalid_request: hash qualification authorizer")?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .filter(|size| *size <= MAX_AUTHORIZER_BYTES)
            .context("invalid_request: qualification authorizer size is outside allowed bounds")?;
        digest.update(&buffer[..read]);
    }
    if size != expected_size {
        bail!("invalid_request: qualification authorizer changed during validation");
    }
    if format!("{:x}", digest.finalize()) != sha256 {
        bail!("invalid_request: qualification authorizer SHA-256 does not match");
    }
    Ok(())
}

fn open_file_without_following_symlinks(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn open_directory_without_following_symlinks(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(unix)]
fn verify_owner_private_executable(_path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let mode = metadata.permissions().mode();
    // SAFETY: `geteuid` has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } || mode & 0o077 != 0 || mode & 0o100 == 0 {
        bail!("invalid_request: qualification authorizer must be an owner-only executable");
    }
    Ok(())
}

#[cfg(windows)]
fn verify_owner_private_executable(path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    verify_private_executable(path)
        .context("invalid_request: qualification authorizer ACL is unsafe")
}

#[cfg(not(any(unix, windows)))]
fn verify_owner_private_executable(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    bail!("invalid_request: qualification authorizers are unsupported on this platform")
}

#[cfg(unix)]
fn verify_owner_private_directory(_path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    // SAFETY: `geteuid` has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0 {
        bail!("private state path is not owner-only");
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_owner_private_directory(path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    verify_private_directory(path).map_err(Into::into)
}

#[cfg(unix)]
fn same_file_identity(left: &File, right: &File) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn same_file_identity(left: &File, right: &File) -> Result<bool> {
    Ok(windows_file_identity(left)? == windows_file_identity(right)?)
}

#[cfg(not(any(unix, windows)))]
fn same_file_identity(_left: &File, _right: &File) -> Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<(u32, u64)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `information` is a correctly sized writable output buffer for a valid file handle.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("invalid_request: inspect qualification authority object identity");
    }
    // SAFETY: the successful call initialized the complete output structure.
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, index))
}

#[cfg(unix)]
fn descriptor_execution_path(file: &File) -> Result<PathBuf> {
    use std::os::fd::AsRawFd as _;

    #[cfg(target_os = "linux")]
    let root = Path::new("/proc/self/fd");
    #[cfg(not(target_os = "linux"))]
    let root = Path::new("/dev/fd");
    if !root.is_dir() {
        bail!("invalid_request: descriptor-bound qualification execution is unavailable");
    }
    Ok(root.join(file.as_raw_fd().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn value(value: impl Into<OsString>) -> Option<OsString> {
        Some(value.into())
    }

    #[cfg(unix)]
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let authorizer = root.path().join("authorizer");
        fs::write(
            &authorizer,
            b"#!/bin/sh\nif [ \"${CTX_AMBIENT_SECRET+x}\" = x ]; then exit 71; fi\nprintf '%s\\n' \"$CTX_PRO_QUALIFICATION_STATE_ROOT\"\n",
        )
        .unwrap();
        fs::set_permissions(&authorizer, fs::Permissions::from_mode(0o700)).unwrap();
        let state_root = root.path().join("state");
        fs::create_dir(&state_root).unwrap();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700)).unwrap();
        let digest = format!("{:x}", Sha256::digest(fs::read(&authorizer).unwrap()));
        (root, authorizer, state_root, digest)
    }

    #[cfg(unix)]
    fn configured(
        authorizer: &Path,
        digest: &str,
        state_root: &Path,
    ) -> QualificationAuthorizerCommand {
        QualificationAuthorizerCommand::from_values(
            value(authorizer.as_os_str()),
            value(digest),
            value(state_root.as_os_str()),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn absent_or_ambiguous_environment_fails_closed() {
        let _environment_loader = QualificationAuthorizerCommand::from_process_environment;
        assert!(
            QualificationAuthorizerCommand::from_values(None, None, None)
                .unwrap()
                .is_none()
        );
        for values in [
            (value("/tmp/authorizer"), None, None),
            (None, value("a".repeat(64)), None),
            (None, None, value("/tmp/state")),
            (value("/tmp/authorizer"), value("a".repeat(64)), None),
        ] {
            let error = QualificationAuthorizerCommand::from_values(values.0, values.1, values.2)
                .unwrap_err();
            assert!(error.to_string().contains("configured together"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn valid_execution_is_descriptor_bound_and_clears_the_environment() {
        let (root, authorizer, state_root, digest) = fixture();
        let command = configured(&authorizer, &digest, &state_root);
        let prepared = command.prepare_execution().unwrap();

        let retained = root.path().join("retained-authorizer");
        fs::rename(&authorizer, &retained).unwrap();
        fs::write(&authorizer, b"#!/bin/sh\nprintf 'attacker\\n'\n").unwrap();
        fs::set_permissions(&authorizer, fs::Permissions::from_mode(0o700)).unwrap();

        let mut process = std::process::Command::new(prepared.program());
        process.env("CTX_AMBIENT_SECRET", "must-not-leak");
        prepared.configure_command(&mut process);
        let output = process.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n", state_root.display())
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_noncanonical_authorizer_paths_are_rejected() {
        let (root, authorizer, state_root, digest) = fixture();
        let symlink = root.path().join("authorizer-link");
        std::os::unix::fs::symlink(&authorizer, &symlink).unwrap();
        let error = QualificationAuthorizerCommand::from_values(
            value(symlink.as_os_str()),
            value(&digest),
            value(state_root.as_os_str()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("canonical"));

        let noncanonical = root.path().join("nested").join("..").join("authorizer");
        let error = QualificationAuthorizerCommand::from_values(
            value(noncanonical.as_os_str()),
            value(&digest),
            value(state_root.as_os_str()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("normalized absolute path"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_mode_and_hash_are_rejected() {
        let (_root, authorizer, state_root, digest) = fixture();
        fs::set_permissions(&authorizer, fs::Permissions::from_mode(0o755)).unwrap();
        let error = QualificationAuthorizerCommand::from_values(
            value(authorizer.as_os_str()),
            value(&digest),
            value(state_root.as_os_str()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("owner-only executable"));

        fs::set_permissions(&authorizer, fs::Permissions::from_mode(0o700)).unwrap();
        let error = QualificationAuthorizerCommand::from_values(
            value(authorizer.as_os_str()),
            value("a".repeat(64)),
            value(state_root.as_os_str()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("SHA-256 does not match"));
    }

    #[cfg(unix)]
    #[test]
    fn state_root_must_be_canonical_private_and_bounded() {
        let (root, authorizer, state_root, digest) = fixture();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o755)).unwrap();
        let error = QualificationAuthorizerCommand::from_values(
            value(authorizer.as_os_str()),
            value(&digest),
            value(state_root.as_os_str()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("permissions are unsafe"));
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700)).unwrap();

        let symlink = root.path().join("state-link");
        std::os::unix::fs::symlink(&state_root, &symlink).unwrap();
        let error = QualificationAuthorizerCommand::from_values(
            value(authorizer.as_os_str()),
            value(&digest),
            value(symlink.as_os_str()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("canonical"));

        for path in [Path::new("relative-state"), Path::new("/")] {
            let error = QualificationAuthorizerCommand::from_values(
                value(authorizer.as_os_str()),
                value(&digest),
                value(path.as_os_str()),
            )
            .unwrap_err();
            assert!(error.to_string().contains("bounded"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn executable_and_state_root_tampering_are_rejected_before_prepare() {
        let (_root, authorizer, state_root, digest) = fixture();
        let command = configured(&authorizer, &digest, &state_root);
        fs::write(&authorizer, b"tampered").unwrap();
        let error = command.prepare_execution().unwrap_err();
        assert!(error.to_string().contains("SHA-256 does not match"));

        let (_root, authorizer, state_root, digest) = fixture();
        let command = configured(&authorizer, &digest, &state_root);
        let displaced = state_root.with_extension("original");
        fs::rename(&state_root, &displaced).unwrap();
        fs::create_dir(&state_root).unwrap();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700)).unwrap();
        let error = command.prepare_execution().unwrap_err();
        assert!(error.to_string().contains("state root changed"));
    }
}

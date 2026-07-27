use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
#[cfg(windows)]
use ctx_history_core::platform_security::restrict_private_executable;
use ctx_history_core::platform_security::{
    restrict_private_directory, verify_private_directory as verify_platform_private_directory,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::lifecycle::lifecycle_manifest::{ReleaseChannel, MAX_ARTIFACT_BYTES};

#[cfg(ctx_pro_qualification)]
const HELPER_PATH_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_PATH";
#[cfg(ctx_pro_qualification)]
const HELPER_SHA256_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_SHA256";
#[cfg(ctx_pro_qualification)]
const HELPER_CHANNEL_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_CHANNEL";

#[derive(Debug)]
pub(crate) struct QualificationHelperBundle {
    stage: QualificationStage,
    #[cfg(ctx_pro_qualification)]
    source_path: PathBuf,
    path: PathBuf,
    sha256: String,
}

#[derive(Debug)]
struct QualificationStage {
    directory: PathBuf,
    helper: PathBuf,
    helper_handle: Option<File>,
}

impl Drop for QualificationStage {
    fn drop(&mut self) {
        drop(self.helper_handle.take());
        let _ = fs::remove_file(&self.helper);
        let _ = fs::remove_dir(&self.directory);
    }
}

impl QualificationHelperBundle {
    #[cfg(ctx_pro_qualification)]
    pub(crate) fn from_process_environment(
        selected_channel: ReleaseChannel,
    ) -> Result<Option<Self>> {
        Self::from_values(
            std::env::var_os(HELPER_PATH_ENV),
            std::env::var_os(HELPER_SHA256_ENV),
            std::env::var_os(HELPER_CHANNEL_ENV),
            selected_channel,
        )
    }

    fn from_values(
        path: Option<OsString>,
        sha256: Option<OsString>,
        channel: Option<OsString>,
        selected_channel: ReleaseChannel,
    ) -> Result<Option<Self>> {
        if path.is_none() && sha256.is_none() && channel.is_none() {
            return Ok(None);
        }
        let (Some(path), Some(sha256), Some(channel)) = (path, sha256, channel) else {
            bail!(
                "invalid_request: qualification helper path, SHA-256, and channel must be configured together"
            );
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            bail!("invalid_request: qualification helper path must be absolute");
        }
        let sha256 = sha256.into_string().map_err(|_| {
            anyhow::anyhow!("invalid_request: qualification helper SHA-256 must be valid UTF-8")
        })?;
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!(
                "invalid_request: qualification helper SHA-256 must be 64 lowercase hex characters"
            );
        }
        let channel = channel.into_string().map_err(|_| {
            anyhow::anyhow!("invalid_request: qualification helper channel must be valid UTF-8")
        })?;
        if channel != selected_channel.wire_name() {
            bail!(
                "invalid_request: qualification helper channel does not match the selected commercial channel"
            );
        }
        let source = open_verified_source(&path)?;
        let stage = stage_verified_helper(source, &sha256)?;
        let staged_path = stage.helper.clone();
        let bundle = Self {
            stage,
            #[cfg(ctx_pro_qualification)]
            source_path: path,
            path: staged_path,
            sha256,
        };
        bundle.verify()?;
        Ok(Some(bundle))
    }

    pub(crate) fn verified_path(&self) -> Result<&Path> {
        self.verify()?;
        Ok(&self.path)
    }

    #[cfg(ctx_pro_qualification)]
    pub(crate) fn source_path(&self) -> &Path {
        &self.source_path
    }

    fn verify(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid_request: qualification stage is invalid"))?;
        verify_platform_private_directory(parent)
            .context("invalid_request: qualification stage permissions are unsafe")?;
        let retained = self
            .stage
            .helper_handle
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("invalid_request: qualification stage is unavailable"))?
            .metadata()
            .context("invalid_request: inspect retained qualification helper")?;
        let mut named = open_verified_helper(&self.path)?;
        let opened = named
            .metadata()
            .context("invalid_request: inspect staged qualification helper")?;
        if !same_file_identity(&retained, &opened) {
            bail!("invalid_request: qualification helper changed during validation");
        }
        verify_reader_digest(&mut named, opened.len(), &self.sha256)?;
        Ok(())
    }
}

struct VerifiedSource {
    file: File,
    size: u64,
}

fn open_verified_source(path: &Path) -> Result<VerifiedSource> {
    let metadata =
        fs::symlink_metadata(path).context("invalid_request: inspect qualification helper")?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("invalid_request: qualification helper must be a regular non-symlink file");
    }
    verify_private_executable(path, &metadata)?;
    if metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
        bail!("invalid_request: qualification helper size is outside allowed bounds");
    }
    let file = open_without_following_symlinks(path)
        .context("invalid_request: open qualification helper")?;
    let opened = file
        .metadata()
        .context("invalid_request: inspect opened qualification helper")?;
    if !same_file_identity(&metadata, &opened) {
        bail!("invalid_request: qualification helper changed during validation");
    }
    Ok(VerifiedSource {
        file,
        size: opened.len(),
    })
}

fn stage_verified_helper(mut source: VerifiedSource, sha256: &str) -> Result<QualificationStage> {
    let directory = create_stage_directory()?;
    let path = directory.join(if cfg!(windows) {
        "ctx-pro-qualification.exe"
    } else {
        "ctx-pro-qualification"
    });
    let mut stage = QualificationStage {
        directory,
        helper: path.clone(),
        helper_handle: None,
    };
    restrict_private_directory(&stage.directory)
        .context("invalid_request: secure qualification helper stage")?;
    verify_platform_private_directory(&stage.directory)
        .context("invalid_request: qualification stage permissions are unsafe")?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .context("invalid_request: create staged qualification helper")?;
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source
            .file
            .read(&mut buffer)
            .context("invalid_request: read qualification helper")?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .filter(|copied| *copied <= MAX_ARTIFACT_BYTES)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid_request: qualification helper size is outside allowed bounds"
                )
            })?;
        digest.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .context("invalid_request: write staged qualification helper")?;
    }
    if copied != source.size {
        bail!("invalid_request: qualification helper changed during validation");
    }
    if format!("{:x}", digest.finalize()) != sha256 {
        bail!("invalid_request: qualification helper SHA-256 does not match");
    }
    file.flush()
        .context("invalid_request: flush staged qualification helper")?;
    file.sync_all()
        .context("invalid_request: sync staged qualification helper")?;
    drop(file);

    make_staged_helper_read_only(&path)?;
    let mut helper_handle = open_verified_helper(&path)?;
    let staged_metadata = helper_handle
        .metadata()
        .context("invalid_request: inspect staged qualification helper")?;
    verify_reader_digest(&mut helper_handle, staged_metadata.len(), sha256)?;
    sync_directory(&stage.directory)?;
    stage.helper_handle = Some(helper_handle);
    Ok(stage)
}

#[cfg(unix)]
fn make_staged_helper_read_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o500))
        .context("invalid_request: make staged qualification helper read-only")
}

#[cfg(windows)]
fn make_staged_helper_read_only(path: &Path) -> Result<()> {
    restrict_private_executable(path).context("invalid_request: secure staged qualification helper")
}

#[cfg(not(any(unix, windows)))]
fn make_staged_helper_read_only(_path: &Path) -> Result<()> {
    bail!("invalid_request: qualification helpers are unsupported on this platform")
}

fn open_verified_helper(path: &Path) -> Result<File> {
    let metadata =
        fs::symlink_metadata(path).context("invalid_request: inspect qualification helper")?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("invalid_request: qualification helper must be a regular non-symlink file");
    }
    verify_private_executable(path, &metadata)?;
    if metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
        bail!("invalid_request: qualification helper size is outside allowed bounds");
    }
    let file = open_without_following_symlinks(path)
        .context("invalid_request: open qualification helper")?;
    let opened = file
        .metadata()
        .context("invalid_request: inspect opened qualification helper")?;
    if !same_file_identity(&metadata, &opened) {
        bail!("invalid_request: qualification helper changed during validation");
    }
    Ok(file)
}

fn verify_reader_digest(reader: &mut File, expected_size: u64, sha256: &str) -> Result<()> {
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("invalid_request: hash qualification helper")?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .filter(|size| *size <= MAX_ARTIFACT_BYTES)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid_request: qualification helper size is outside allowed bounds"
                )
            })?;
        digest.update(&buffer[..read]);
    }
    if size != expected_size {
        bail!("invalid_request: qualification helper changed during validation");
    }
    if format!("{:x}", digest.finalize()) != sha256 {
        bail!("invalid_request: qualification helper SHA-256 does not match");
    }
    Ok(())
}

fn create_stage_directory() -> Result<PathBuf> {
    for _ in 0..16 {
        let path =
            std::env::temp_dir().join(format!("ctx-pro-qualification-{}", Uuid::new_v4().simple()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).context("invalid_request: create qualification helper stage")
            }
        }
    }
    bail!("invalid_request: create unique qualification helper stage")
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .context("invalid_request: sync qualification helper stage")
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn open_without_following_symlinks(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
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

#[cfg(unix)]
fn verify_private_executable(_path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let mode = metadata.permissions().mode();
    if metadata.uid() != unsafe { libc::geteuid() } || mode & 0o077 != 0 || mode & 0o100 == 0 {
        bail!("invalid_request: qualification helper must be an owner-only executable");
    }
    Ok(())
}

#[cfg(windows)]
fn verify_private_executable(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let _ = metadata;
    ctx_history_core::platform_security::verify_private_executable(path)
        .context("invalid_request: qualification helper ACL is unsafe")
}

#[cfg(not(any(unix, windows)))]
fn verify_private_executable(_path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let _ = metadata;
    bail!("invalid_request: qualification helpers are unsupported on this platform")
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    left.volume_serial_number() == right.volume_serial_number()
        && left.file_index() == right.file_index()
}

#[cfg(not(any(unix, windows)))]
fn same_file_identity(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn value(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[cfg(unix)]
    fn helper() -> (tempfile::TempDir, PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctx-pro");
        fs::write(&path, b"#!/bin/sh\nprintf 'verified helper\\n'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let digest = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
        (root, path, digest)
    }

    #[test]
    fn absent_configuration_preserves_normal_delivery() {
        assert!(
            QualificationHelperBundle::from_values(None, None, None, ReleaseChannel::Stable)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn partial_configuration_fails_closed() {
        let error = QualificationHelperBundle::from_values(
            value("/tmp/ctx-pro"),
            None,
            value("stable"),
            ReleaseChannel::Stable,
        )
        .unwrap_err();
        assert!(error.to_string().contains("configured together"));
    }

    #[cfg(unix)]
    #[test]
    fn source_path_replacement_cannot_change_the_staged_helper() {
        let (_root, path, digest) = helper();
        let bundle = QualificationHelperBundle::from_values(
            Some(path.clone().into_os_string()),
            value(&digest),
            value("stable"),
            ReleaseChannel::Stable,
        )
        .unwrap()
        .unwrap();
        let staged = bundle.verified_path().unwrap().to_path_buf();
        assert_ne!(staged, path);
        assert_eq!(
            fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o500
        );

        let original = path.with_extension("original");
        fs::rename(&path, &original).unwrap();
        fs::write(&path, b"#!/bin/sh\nprintf 'attacker replacement\\n'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        let output = std::process::Command::new(bundle.verified_path().unwrap())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"verified helper\n");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("attacker"));
    }

    #[cfg(unix)]
    #[test]
    fn staged_helper_tampering_is_rejected() {
        let (_root, path, digest) = helper();
        let bundle = QualificationHelperBundle::from_values(
            Some(path.into_os_string()),
            value(&digest),
            value("stable"),
            ReleaseChannel::Stable,
        )
        .unwrap()
        .unwrap();
        let staged = bundle.verified_path().unwrap().to_path_buf();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&staged, b"tampered helper").unwrap();
        assert!(bundle
            .verified_path()
            .unwrap_err()
            .to_string()
            .contains("SHA-256 does not match"));
    }

    #[cfg(unix)]
    #[test]
    fn staged_path_replacement_is_rejected_against_the_retained_inode() {
        let (root, path, digest) = helper();
        let bundle = QualificationHelperBundle::from_values(
            Some(path.into_os_string()),
            value(&digest),
            value("stable"),
            ReleaseChannel::Stable,
        )
        .unwrap()
        .unwrap();
        let staged = bundle.verified_path().unwrap().to_path_buf();
        let displaced = root.path().join("displaced-staged-helper");
        fs::rename(&staged, &displaced).unwrap();
        fs::write(&staged, b"#!/bin/sh\nprintf 'replacement\\n'\n").unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o500)).unwrap();

        assert!(bundle
            .verified_path()
            .unwrap_err()
            .to_string()
            .contains("changed during validation"));
    }

    #[cfg(unix)]
    #[test]
    fn private_stage_is_removed_when_the_bundle_is_dropped() {
        let (_root, path, digest) = helper();
        let staged;
        {
            let bundle = QualificationHelperBundle::from_values(
                Some(path.into_os_string()),
                value(&digest),
                value("stable"),
                ReleaseChannel::Stable,
            )
            .unwrap()
            .unwrap();
            staged = bundle.verified_path().unwrap().to_path_buf();
            assert!(staged.exists());
        }
        assert!(!staged.exists());
        assert!(!staged.parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_source_is_rejected() {
        let (_root, path, digest) = helper();
        let link = path.with_extension("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let error = QualificationHelperBundle::from_values(
            Some(link.into_os_string()),
            value(&digest),
            value("stable"),
            ReleaseChannel::Stable,
        )
        .unwrap_err();
        assert!(error.to_string().contains("regular non-symlink file"));
    }

    #[cfg(unix)]
    #[test]
    fn selected_channel_mismatch_is_rejected() {
        let (_root, path, digest) = helper();
        let error = QualificationHelperBundle::from_values(
            Some(path.into_os_string()),
            value(&digest),
            value("staging"),
            ReleaseChannel::Stable,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }
}

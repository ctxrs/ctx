use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use super::lifecycle::lifecycle_manifest::{ReleaseChannel, MAX_ARTIFACT_BYTES};

#[cfg(ctx_pro_qualification)]
const HELPER_PATH_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_PATH";
#[cfg(ctx_pro_qualification)]
const HELPER_SHA256_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_SHA256";
#[cfg(ctx_pro_qualification)]
const HELPER_CHANNEL_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_CHANNEL";

#[derive(Debug)]
pub(crate) struct QualificationHelperBundle {
    path: PathBuf,
    sha256: String,
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
        let path = path
            .canonicalize()
            .context("invalid_request: resolve qualification helper path")?;
        let bundle = Self { path, sha256 };
        bundle.verify()?;
        Ok(Some(bundle))
    }

    pub(crate) fn verified_path(&self) -> Result<&Path> {
        self.verify()?;
        Ok(&self.path)
    }

    fn verify(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)
            .context("invalid_request: inspect qualification helper")?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!("invalid_request: qualification helper must be a regular non-symlink file");
        }
        verify_private_executable(&self.path, &metadata)?;
        if metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
            bail!("invalid_request: qualification helper size is outside allowed bounds");
        }
        let file = open_without_following_symlinks(&self.path)
            .context("invalid_request: open qualification helper")?;
        let opened = file
            .metadata()
            .context("invalid_request: inspect opened qualification helper")?;
        if !same_file_identity(&metadata, &opened) {
            bail!("invalid_request: qualification helper changed during validation");
        }
        let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
        file.take(MAX_ARTIFACT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .context("invalid_request: hash qualification helper")?;
        if bytes.len() as u64 != opened.len() || bytes.len() as u64 > MAX_ARTIFACT_BYTES {
            bail!("invalid_request: qualification helper changed during validation");
        }
        if format!("{:x}", Sha256::digest(&bytes)) != self.sha256 {
            bail!("invalid_request: qualification helper SHA-256 does not match");
        }
        Ok(())
    }
}

fn open_without_following_symlinks(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
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
        fs::write(&path, b"qualification helper").unwrap();
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
    fn exact_private_helper_is_accepted_but_tampering_is_rejected() {
        let (_root, path, digest) = helper();
        let bundle = QualificationHelperBundle::from_values(
            Some(path.into_os_string()),
            value(&digest),
            value("stable"),
            ReleaseChannel::Stable,
        )
        .unwrap()
        .unwrap();
        bundle.verified_path().unwrap();
        fs::write(bundle.verified_path().unwrap(), b"tampered helper").unwrap();
        assert!(bundle
            .verified_path()
            .unwrap_err()
            .to_string()
            .contains("SHA-256 does not match"));
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

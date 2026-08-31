use std::{
    fs, io,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ctx_history_platform::platform_security::{
    ensure_private_file_handle, restrict_private_file_handle, verify_private_file_handle,
};
use uuid::Uuid;

const CONFIG_MUTATION_LOCK_FILE: &str = ".config.mutation.lock";

pub(super) struct ConfigMutationLock {
    file: fs::File,
}

impl ConfigMutationLock {
    pub(super) fn acquire(config_path: &Path) -> Result<Self> {
        let parent = config_path.parent().ok_or_else(|| {
            anyhow::anyhow!("config path has no parent: {}", config_path.display())
        })?;
        let path = parent.join(CONFIG_MUTATION_LOCK_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(anyhow::anyhow!(
                    "config mutation lock is not a regular non-symlink file: {}",
                    path.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect config lock {}", path.display()));
            }
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
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
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
                    WRITE_DAC,
                },
            };

            // The ACL hardening call operates through this same handle and
            // therefore needs WRITE_DAC. Sharing read/write lets contenders
            // open the stable lock inode and block in fs2 while withholding
            // delete sharing prevents replacement during the transaction.
            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("open config lock {}", path.display()))?;
        restrict_private_file_handle(&file)
            .with_context(|| format!("protect config lock {}", path.display()))?;
        fs2::FileExt::lock_exclusive(&file)
            .with_context(|| format!("acquire config lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for ConfigMutationLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(super) fn read_config_text(path: &Path) -> Result<Option<String>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::{
            Foundation::GENERIC_READ,
            Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                READ_CONTROL, WRITE_DAC,
            },
        };

        options
            .access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    ensure_private_file_handle(&file)
        .with_context(|| format!("protect private config {}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(Some(text))
}

pub(super) fn write_config_durably(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
    let temp = temporary_config_path(parent);
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
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
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, READ_CONTROL, WRITE_DAC,
                },
            };

            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("create temporary config {}", temp.display()))?;
        restrict_private_file_handle(&file)
            .with_context(|| format!("protect temporary config {}", temp.display()))?;
        verify_private_file_handle(&file)
            .with_context(|| format!("verify temporary config {}", temp.display()))?;
        file.write_all(body)
            .with_context(|| format!("write temporary config {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary config {}", temp.display()))?;
        drop(file);
        replace_config_file(&temp, path)
            .with_context(|| format!("publish config {}", path.display()))?;
        sync_config_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn temporary_config_path(parent: &Path) -> PathBuf {
    parent.join(format!(".config.{}.tmp", Uuid::new_v4().simple()))
}

#[cfg(not(windows))]
fn replace_config_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_config_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_config_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open config directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync config directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_config_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_config_names_use_unpredictable_uuids() {
        let path = temporary_config_path(Path::new("private-root"));
        let name = path.file_name().unwrap().to_str().unwrap();
        let uuid = name
            .strip_prefix(".config.")
            .and_then(|name| name.strip_suffix(".tmp"))
            .unwrap();
        assert!(Uuid::parse_str(uuid).is_ok(), "{name}");
    }
}

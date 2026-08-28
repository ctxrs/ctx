use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
#[cfg(test)]
use ctx_history_platform::platform_security::restrict_private_directory;
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file, verify_private_directory,
    verify_private_file,
};
use sha2::{Digest, Sha256};

use super::ReleaseTransport;

const DOWNLOAD_DIRECTORY: &str = ".ctx-upgrade-downloads";

#[derive(Debug)]
pub(super) struct DownloadedArtifact {
    path: PathBuf,
    file: Option<fs::File>,
    identity: FileIdentity,
    byte_len: u64,
    sha256: String,
    remove_on_drop: bool,
}

impl DownloadedArtifact {
    pub(super) fn download_verified(
        transport: &dyn ReleaseTransport,
        managed_root: &Path,
        endpoint: &str,
        expected_sha256: &str,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<Self> {
        validate_sha256(expected_sha256)?;
        if max_bytes == 0 {
            return Err(anyhow!("artifact max bytes must be greater than zero"));
        }
        let downloads = prepare_download_directory(managed_root)?;
        let (path, file) = create_download_file(&downloads)?;
        let identity = file_identity(&file)?;
        let mut artifact = Self {
            path,
            file: Some(file),
            identity,
            byte_len: 0,
            sha256: expected_sha256.to_ascii_lowercase(),
            remove_on_drop: true,
        };
        artifact.byte_len =
            transport.download_artifact(endpoint, artifact.file_mut()?, max_bytes, timeout)?;
        artifact
            .file_ref()?
            .sync_all()
            .with_context(|| format!("sync downloaded artifact {}", artifact.path.display()))?;
        artifact.verify_unchanged()?;
        Ok(artifact)
    }

    /// Reuses an owner-private cached artifact only after rehashing it against
    /// current signed metadata. A cache miss downloads into a bounded
    /// temporary file and publishes a verified copy for later releases.
    pub(super) fn download_or_reuse_verified(
        transport: &dyn ReleaseTransport,
        managed_root: &Path,
        endpoint: &str,
        expected_sha256: &str,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<Self> {
        validate_sha256(expected_sha256)?;
        let downloads = prepare_download_directory(managed_root)?;
        let cache_path = downloads.join(format!(
            "runtime-{}.artifact",
            expected_sha256.to_ascii_lowercase()
        ));
        if let Some(mut cached) = Self::open_cached(&cache_path, expected_sha256)? {
            if cached.byte_len == 0 || cached.byte_len > max_bytes {
                cached.remove_on_drop = true;
                drop(cached);
            } else {
                match cached.verify_unchanged() {
                    Ok(()) => return Ok(cached),
                    Err(_) => {
                        // `open_cached` proved the pathname still identifies the
                        // owner-private file held by this process. Marking it
                        // temporary makes Drop remove only that exact bad cache.
                        cached.remove_on_drop = true;
                        drop(cached);
                    }
                }
            }
        }

        let mut downloaded = Self::download_verified(
            transport,
            managed_root,
            endpoint,
            expected_sha256,
            max_bytes,
            timeout,
        )?;
        downloaded.publish_cache_copy(&cache_path)?;
        Ok(downloaded)
    }

    #[cfg(test)]
    pub(super) fn byte_len(&self) -> u64 {
        self.byte_len
    }

    #[cfg(test)]
    pub(super) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns a path that remains bound to the held verified file.
    ///
    /// Unix consumers use the process file-descriptor namespace. Windows keeps
    /// the private file open without write/delete sharing while the returned
    /// path is in use.
    pub(super) fn stable_path(&mut self) -> Result<PathBuf> {
        self.verify_path_identity()?;
        self.file_mut()?.seek(SeekFrom::Start(0))?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            Ok(PathBuf::from(format!(
                "/proc/self/fd/{}",
                self.file_ref()?.as_raw_fd()
            )))
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            use std::os::fd::AsRawFd as _;
            Ok(PathBuf::from(format!(
                "/dev/fd/{}",
                self.file_ref()?.as_raw_fd()
            )))
        }
        #[cfg(windows)]
        {
            Ok(self.path.clone())
        }
    }

    /// Copies from the held handle and rechecks the signed digest while
    /// copying, so callers never need to reopen the temporary pathname.
    pub(super) fn copy_verified_to(&mut self, output: &mut impl Write) -> Result<u64> {
        self.verify_path_identity()?;
        let byte_len = self.byte_len;
        let sha256 = self.sha256.clone();
        copy_file_verified(
            self.file_mut()?,
            output,
            byte_len,
            &sha256,
            "verified artifact",
        )
    }

    pub(super) fn verify_unchanged(&mut self) -> Result<()> {
        self.verify_path_identity()?;
        let byte_len = self.byte_len;
        let sha256 = self.sha256.clone();
        copy_file_verified(
            self.file_mut()?,
            &mut std::io::sink(),
            byte_len,
            &sha256,
            "verified artifact",
        )
        .map(|_| ())
    }

    fn verify_path_identity(&self) -> Result<()> {
        let reopened = open_private_file(&self.path)
            .with_context(|| format!("reopen verified artifact {}", self.path.display()))?;
        if file_identity(&reopened)? != self.identity {
            return Err(anyhow!(
                "verified artifact path identity changed after download"
            ));
        }
        Ok(())
    }

    fn file_ref(&self) -> Result<&fs::File> {
        self.file
            .as_ref()
            .ok_or_else(|| anyhow!("verified artifact file is already closed"))
    }

    fn file_mut(&mut self) -> Result<&mut fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| anyhow!("verified artifact file is already closed"))
    }

    fn open_cached(path: &Path, expected_sha256: &str) -> Result<Option<Self>> {
        let file = match open_retained_private_file(path) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) if cached_artifact_has_active_writer(&error) => {
                // A concurrent publisher still owns write access. Treat the
                // cache as unavailable; the separately verified download path
                // remains authoritative and publication is create-new.
                return Ok(None);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open cached runtime artifact {}", path.display()));
            }
        };
        let byte_len = file.metadata()?.len();
        Ok(Some(Self {
            path: path.to_path_buf(),
            identity: file_identity(&file)?,
            file: Some(file),
            byte_len,
            sha256: expected_sha256.to_ascii_lowercase(),
            remove_on_drop: false,
        }))
    }

    fn publish_cache_copy(&mut self, cache_path: &Path) -> Result<()> {
        let mut cache = match create_private_file(cache_path) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                // Another process won the cache publication race. It must
                // verify before a later process can reuse it.
                return Ok(());
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create cached runtime artifact {}", cache_path.display())
                });
            }
        };
        let result = self
            .copy_verified_to(&mut cache)
            .and_then(|_| cache.sync_all().map_err(Into::into));
        if let Err(error) = result {
            drop(cache);
            let _ = fs::remove_file(cache_path);
            return Err(error).with_context(|| {
                format!("publish cached runtime artifact {}", cache_path.display())
            });
        }
        Ok(())
    }

    #[cfg(test)]
    fn temporary_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DownloadedArtifact {
    fn drop(&mut self) {
        let remove_original = self.remove_on_drop && self.verify_path_identity().is_ok();
        drop(self.file.take());
        if remove_original {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "expected artifact checksum is not a SHA-256 digest"
        ));
    }
    if value.bytes().all(|byte| byte == b'0') {
        return Err(anyhow!("expected artifact checksum is a placeholder"));
    }
    Ok(())
}

fn prepare_download_directory(managed_root: &Path) -> Result<PathBuf> {
    if managed_root
        .to_str()
        .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
    {
        return Err(anyhow!(
            "artifact download root must not be empty or whitespace-padded"
        ));
    }
    if !managed_root.is_absolute() {
        return Err(anyhow!("artifact download root must be an absolute path"));
    }
    verify_private_directory(managed_root).with_context(|| {
        format!(
            "verify private artifact download root {}",
            managed_root.display()
        )
    })?;
    let downloads = managed_root.join(DOWNLOAD_DIRECTORY);
    create_private_directory_all(&downloads).with_context(|| {
        format!(
            "create private artifact download directory {}",
            downloads.display()
        )
    })?;
    verify_private_directory(&downloads)
        .with_context(|| format!("verify artifact download directory {}", downloads.display()))?;
    Ok(downloads)
}

fn create_download_file(downloads: &Path) -> Result<(PathBuf, fs::File)> {
    for _ in 0..8 {
        let path = downloads.join(format!(
            ".ctx-upgrade-{}.part",
            uuid::Uuid::new_v4().simple()
        ));
        match create_private_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create artifact download {}", path.display()));
            }
        }
    }
    Err(anyhow!(
        "could not allocate a unique private artifact download"
    ))
}

pub(super) fn create_private_file(path: &Path) -> Result<fs::File> {
    #[cfg(windows)]
    {
        return create_private_file_windows(path);
    }
    #[cfg(not(windows))]
    create_private_file_non_windows(path)
}

#[cfg(not(windows))]
fn create_private_file_non_windows(path: &Path) -> Result<fs::File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let protected = restrict_private_file(path)
        .and_then(|()| verify_private_file(path))
        .context("protect private artifact download");
    if let Err(error) = protected {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

#[cfg(windows)]
fn create_private_file_windows(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut initial_options = fs::OpenOptions::new();
    initial_options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let initial = initial_options.open(path)?;
    let identity = match file_identity(&initial) {
        Ok(identity) => identity,
        Err(error) => {
            drop(initial);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    };
    let protected = restrict_private_file(path)
        .and_then(|()| verify_private_file(path))
        .context("protect private artifact download");
    if let Err(error) = protected {
        drop(initial);
        remove_file_if_identity(path, &identity);
        return Err(error);
    }
    drop(initial);

    let mut locked_options = fs::OpenOptions::new();
    locked_options
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = match locked_options.open(path) {
        Ok(file) => file,
        Err(error) => {
            remove_file_if_identity(path, &identity);
            return Err(error).context("lock private artifact download");
        }
    };
    let actual_identity = match file_identity(&file) {
        Ok(identity) => identity,
        Err(error) => {
            drop(file);
            remove_file_if_identity(path, &identity);
            return Err(error);
        }
    };
    if actual_identity != identity {
        return Err(anyhow!(
            "private artifact download identity changed while it was protected"
        ));
    }
    if let Err(error) = ctx_history_platform::platform_security::verify_private_file_handle(&file) {
        drop(file);
        remove_file_if_identity(path, &identity);
        return Err(error).context("verify private artifact download handle");
    }
    Ok(file)
}

#[cfg(windows)]
fn remove_file_if_identity(path: &Path, expected: &FileIdentity) {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let Ok(file) = options.open(path) else {
        return;
    };
    let matches = file_identity(&file).is_ok_and(|actual| &actual == expected);
    drop(file);
    if matches {
        let _ = fs::remove_file(path);
    }
}

fn open_private_file(path: &Path) -> Result<fs::File> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        return open_private_file_windows(path, FILE_SHARE_READ | FILE_SHARE_WRITE);
    }

    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt as _;

        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let file = options.open(path)?;
        verify_private_file(path)?;
        if !file.metadata()?.is_file() {
            return Err(anyhow!("private artifact path is not a regular file"));
        }
        Ok(file)
    }
}

fn open_retained_private_file(path: &Path) -> Result<fs::File> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        // Retained cache handles admit concurrent readers but deny writers and
        // deletion until digest-checked consumption has completed.
        return open_private_file_windows(path, FILE_SHARE_READ);
    }

    #[cfg(not(windows))]
    open_private_file(path)
}

#[cfg(windows)]
fn open_private_file_windows(path: &Path, share_mode: u32) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    verify_private_file(path)?;
    ctx_history_platform::platform_security::verify_private_file_handle(&file)?;
    if !file.metadata()?.is_file() {
        return Err(anyhow!("private artifact path is not a regular file"));
    }
    Ok(file)
}

#[cfg(windows)]
fn cached_artifact_has_active_writer(error: &anyhow::Error) -> bool {
    use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32))
}

#[cfg(not(windows))]
fn cached_artifact_has_active_writer(_error: &anyhow::Error) -> bool {
    false
}

pub(super) fn copy_file_verified(
    file: &mut fs::File,
    output: &mut impl Write,
    expected_len: u64,
    expected_sha256: &str,
    label: &str,
) -> Result<u64> {
    if file.metadata()?.len() != expected_len {
        return Err(anyhow!("{label} size does not match signed metadata"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count)?)
            .ok_or_else(|| anyhow!("{label} size overflow"))?;
        if total > expected_len {
            return Err(anyhow!("{label} grew while being consumed"));
        }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if total != expected_len {
        return Err(anyhow!("{label} changed size while being consumed"));
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(anyhow!(
            "artifact checksum mismatch: expected {expected_sha256}, got {actual}"
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(total)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    first: u64,
    second: u64,
}

fn file_identity(file: &fs::File) -> Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        Ok(FileIdentity {
            first: metadata.dev(),
            second: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
        use windows_sys::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
        };

        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        // SAFETY: `file` owns a live handle and `information` is a valid out pointer.
        if unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("inspect artifact download file identity");
        }
        // SAFETY: the successful call initialized the structure.
        let information = unsafe { information.assume_init() };
        return Ok(FileIdentity {
            first: u64::from(information.dwVolumeSerialNumber),
            second: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        });
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(anyhow!(
            "verified artifact identity is unsupported on this platform"
        ))
    }
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;

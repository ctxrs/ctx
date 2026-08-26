use super::*;

#[cfg(windows)]
use std::sync::atomic::{AtomicUsize, Ordering};

struct FileReleaseTransport;

static FILE_RELEASE_TRANSPORT: FileReleaseTransport = FileReleaseTransport;

#[cfg(windows)]
struct CountingReleaseTransport<'a> {
    inner: &'a dyn ReleaseTransport,
    download_calls: AtomicUsize,
}

#[cfg(windows)]
impl ReleaseTransport for CountingReleaseTransport<'_> {
    fn get_bytes_limited(&self, endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
        self.inner.get_bytes_limited(endpoint, max_bytes)
    }

    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut fs::File,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<u64> {
        self.download_calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .download_artifact(endpoint, destination, max_bytes, timeout)
    }
}

impl ReleaseTransport for FileReleaseTransport {
    fn get_bytes_limited(&self, endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let path = endpoint
            .strip_prefix("file://")
            .ok_or_else(|| anyhow!("test release endpoint is not a file URL"))?;
        let bytes = fs::read(path)?;
        if bytes.len() > max_bytes {
            return Err(anyhow!("release response exceeds max bytes ({max_bytes})"));
        }
        Ok(bytes)
    }

    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut fs::File,
        max_bytes: u64,
        _timeout: Duration,
    ) -> Result<u64> {
        let path = endpoint
            .strip_prefix("file://")
            .ok_or_else(|| anyhow!("test release endpoint is not a file URL"))?;
        let mut source = fs::File::open(path)?;
        let mut total = 0u64;
        let mut buffer = [0u8; 8 * 1024];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(read as u64)
                .ok_or_else(|| anyhow!("artifact size overflow"))?;
            if total > max_bytes {
                return Err(anyhow!("artifact download exceeds max bytes ({max_bytes})"));
            }
            destination.write_all(&buffer[..read])?;
        }
        Ok(total)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    root
}

#[test]
fn verified_download_is_bounded_reusable_and_ephemeral() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    let bytes = b"verified release artifact";
    fs::write(&source, bytes).unwrap();
    let mut artifact = DownloadedArtifact::download_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &format!("file://{}", source.display()),
        &digest(bytes),
        bytes.len() as u64,
        Duration::from_secs(1),
    )
    .unwrap();
    let temporary_path = artifact.temporary_path().to_path_buf();
    assert_eq!(artifact.byte_len(), bytes.len() as u64);
    assert_eq!(artifact.sha256(), digest(bytes));
    let mut copied = Vec::new();
    artifact.copy_verified_to(&mut copied).unwrap();
    assert_eq!(copied, bytes);
    let stable = artifact.stable_path().unwrap();
    assert_eq!(fs::read(stable).unwrap(), bytes);
    drop(artifact);
    assert!(!temporary_path.exists());
}

#[test]
fn invalid_checksums_are_rejected_before_download_state() {
    let root = private_tempdir();
    for checksum in ["g".repeat(64), "0".repeat(64)] {
        let error = DownloadedArtifact::download_verified(
            &FILE_RELEASE_TRANSPORT,
            root.path(),
            "transport-must-not-run",
            &checksum,
            1024,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("expected artifact checksum"));
        assert!(!root.path().join(DOWNLOAD_DIRECTORY).exists());
    }
}

#[test]
fn runtime_download_cache_is_rehashed_and_reused_across_releases() {
    let root = private_tempdir();
    let source = root.path().join("runtime.tar.gz");
    let bytes = b"same signed runtime artifact";
    fs::write(&source, bytes).unwrap();
    let endpoint = format!("file://{}", source.display());
    let expected = digest(bytes);

    let first = DownloadedArtifact::download_or_reuse_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    drop(first);
    fs::remove_file(&source).unwrap();

    let mut reused = DownloadedArtifact::download_or_reuse_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    let mut copied = Vec::new();
    reused.copy_verified_to(&mut copied).unwrap();
    assert_eq!(copied, bytes);
}

#[cfg(windows)]
#[test]
fn retained_runtime_cache_allows_extractor_readers_but_denies_writers_and_delete() {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::{
        Foundation::ERROR_SHARING_VIOLATION,
        Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
    };

    let root = private_tempdir();
    let source = root.path().join("runtime.zip");
    let bytes = b"same signed runtime artifact";
    fs::write(&source, bytes).unwrap();
    let endpoint = format!("file://{}", source.display());
    let expected = digest(bytes);

    let first = DownloadedArtifact::download_or_reuse_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    drop(first);

    let retained = DownloadedArtifact::download_or_reuse_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    let cache_path = retained.temporary_path().to_path_buf();

    let extractor_reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&cache_path)
        .unwrap();
    let writer_error = fs::OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&cache_path)
        .unwrap_err();
    assert_eq!(
        writer_error.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION as i32)
    );
    let delete_error = fs::remove_file(&cache_path).unwrap_err();
    assert_eq!(
        delete_error.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION as i32)
    );

    drop(extractor_reader);
    drop(retained);
    fs::remove_file(cache_path).unwrap();
}

#[cfg(windows)]
#[test]
fn publisher_held_runtime_cache_falls_back_to_a_separately_verified_download() {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

    let root = private_tempdir();
    let source = root.path().join("runtime.zip");
    let bytes = b"same signed runtime artifact";
    fs::write(&source, bytes).unwrap();
    let endpoint = format!("file://{}", source.display());
    let expected = digest(bytes);

    let first = DownloadedArtifact::download_or_reuse_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    drop(first);

    let cache_path = root
        .path()
        .join(DOWNLOAD_DIRECTORY)
        .join(format!("runtime-{expected}.artifact"));
    let mut publisher = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&cache_path)
        .unwrap();
    let transport = CountingReleaseTransport {
        inner: &FILE_RELEASE_TRANSPORT,
        download_calls: AtomicUsize::new(0),
    };

    let mut downloaded = DownloadedArtifact::download_or_reuse_verified(
        &transport,
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(transport.download_calls.load(Ordering::SeqCst), 1);
    assert_ne!(downloaded.temporary_path(), cache_path);
    let downloaded_path = downloaded.temporary_path().to_path_buf();
    let mut copied = Vec::new();
    downloaded.copy_verified_to(&mut copied).unwrap();
    assert_eq!(copied, bytes);

    publisher.seek(SeekFrom::Start(0)).unwrap();
    let mut cached = Vec::new();
    publisher.read_to_end(&mut cached).unwrap();
    assert_eq!(cached, bytes);
    drop(downloaded);
    assert!(!downloaded_path.exists());
    assert!(cache_path.is_file());

    drop(publisher);
    fs::remove_file(cache_path).unwrap();
}

#[test]
fn engine_hash_failure_does_not_leave_a_partial_download() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    fs::write(&source, b"wrong bytes").unwrap();
    let error = DownloadedArtifact::download_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &format!("file://{}", source.display()),
        &digest(b"expected bytes"),
        1024,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("artifact checksum mismatch"));
    assert_eq!(
        fs::read_dir(root.path().join(DOWNLOAD_DIRECTORY))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn oversized_download_does_not_leave_a_partial_download() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    fs::write(&source, b"12345").unwrap();
    let error = DownloadedArtifact::download_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &format!("file://{}", source.display()),
        &digest(b"12345"),
        4,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("exceeds max bytes (4)"));
    assert_eq!(
        fs::read_dir(root.path().join(DOWNLOAD_DIRECTORY))
            .unwrap()
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn held_artifact_detects_same_path_substitution() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    let bytes = b"original";
    fs::write(&source, bytes).unwrap();
    let mut artifact = DownloadedArtifact::download_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &format!("file://{}", source.display()),
        &digest(bytes),
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    let replacement = artifact.temporary_path().with_extension("replacement");
    fs::write(&replacement, bytes).unwrap();
    restrict_private_file(&replacement).unwrap();
    fs::rename(&replacement, artifact.temporary_path()).unwrap();
    let error = artifact.copy_verified_to(&mut Vec::new()).unwrap_err();
    assert!(error.to_string().contains("path identity changed"));
    let substituted_path = artifact.temporary_path().to_path_buf();
    drop(artifact);
    assert_eq!(fs::read(substituted_path).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn symlinked_download_directory_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;

    let root = private_tempdir();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, root.path().join(DOWNLOAD_DIRECTORY)).unwrap();
    let source = root.path().join("source.bin");
    fs::write(&source, b"bytes").unwrap();
    let error = DownloadedArtifact::download_verified(
        &FILE_RELEASE_TRANSPORT,
        root.path(),
        &format!("file://{}", source.display()),
        &digest(b"bytes"),
        1024,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("create private artifact download directory"));
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn insecure_existing_download_directory_is_rejected_without_repair() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = private_tempdir();
    let downloads = root.path().join(DOWNLOAD_DIRECTORY);
    fs::create_dir(&downloads).unwrap();
    fs::set_permissions(&downloads, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(prepare_download_directory(root.path()).is_err());
    assert_eq!(
        fs::metadata(&downloads).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

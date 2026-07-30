use std::{
    collections::BTreeSet,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    Result,
};

#[cfg(test)]
use std::{
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

const TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";
const FULL_CONTENT_FINGERPRINT_MAX_BYTES: u64 = 64 * 1024;
const SPARSE_SAMPLE_BYTES: u64 = 8 * 1024;

#[cfg(test)]
std::thread_local! {
    static CONTENT_SAMPLE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static FORBIDDEN_CONTENT_OPENS: LazyLock<Mutex<BTreeSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

#[cfg(test)]
pub(crate) struct ForbiddenOrdinaryFileContentOpen {
    path: PathBuf,
}

#[cfg(test)]
impl Drop for ForbiddenOrdinaryFileContentOpen {
    fn drop(&mut self) {
        let mut paths = FORBIDDEN_CONTENT_OPENS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        paths.remove(&self.path);
    }
}

#[cfg(test)]
pub(crate) fn forbid_ordinary_file_content_open(path: &Path) -> ForbiddenOrdinaryFileContentOpen {
    let path = path.to_path_buf();
    let mut paths = FORBIDDEN_CONTENT_OPENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    paths.insert(path.clone());
    ForbiddenOrdinaryFileContentOpen { path }
}

#[cfg(test)]
fn reject_forbidden_content_open(path: &Path) -> Result<()> {
    let paths = FORBIDDEN_CONTENT_OPENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if paths.contains(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test forbids opening this provider transcript",
        )
        .into());
    }
    Ok(())
}

/// A bounded observation of an ordinary provider file.
///
/// Length and mtime retain the inexpensive append/no-op checks used by callers.
/// The token binds strong platform change identity where available, and falls
/// back to bounded content sampling only on targets without reliable change
/// identity. A same-size rewrite with a restored mtime therefore cannot
/// masquerade as an unchanged file on supported platforms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryFileObservation {
    len: u64,
    modified_at: SystemTime,
    token: [u8; 32],
}

impl OrdinaryFileObservation {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub fn token_hex(&self) -> String {
        self.token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

pub fn observe_ordinary_file(path: impl AsRef<Path>) -> Result<OrdinaryFileObservation> {
    observe_ordinary_file_inner(path.as_ref(), || {})
}

fn observe_ordinary_file_inner(
    path: &Path,
    before_open: impl FnOnce(),
) -> Result<OrdinaryFileObservation> {
    before_open();
    #[cfg(test)]
    reject_forbidden_content_open(path)?;
    let opened = open_provider_source_file(path)?;
    observe_opened_ordinary_file(path, &opened)
}

pub(crate) fn observe_opened_ordinary_file(
    path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<OrdinaryFileObservation> {
    let metadata = opened.metadata().clone();
    let platform_before = platform_token(path, opened.file(), &metadata)?;
    // Supported platforms expose a stable file identity plus a change-time
    // value that cannot be restored through ordinary file timestamp APIs. That
    // is both stronger and substantially cheaper than reopening and sampling
    // every provider file during an unchanged inventory pass. Targets without
    // reliable change identity retain the bounded content fallback.
    let content_fingerprint = if platform_before.is_some() {
        None
    } else {
        let mut file = opened.file().try_clone()?;
        Some(content_fingerprint(&mut file, &metadata)?)
    };
    let current = opened.file().metadata()?;
    let platform_after = platform_token(path, opened.file(), &current)?;
    if current.len() != metadata.len()
        || current.modified().ok() != metadata.modified().ok()
        || platform_after != platform_before
    {
        return Err(file_changed_during_observation().into());
    }
    opened.revalidate()?;

    Ok(OrdinaryFileObservation {
        len: metadata.len(),
        modified_at: metadata.modified().unwrap_or(UNIX_EPOCH),
        token: combined_token(platform_before, content_fingerprint),
    })
}

pub(crate) fn open_ordinary_file_without_following(path: &Path) -> Result<File> {
    #[cfg(test)]
    reject_forbidden_content_open(path)?;
    open_provider_source_file(path)?
        .file()
        .try_clone()
        .map_err(Into::into)
}

#[cfg(unix)]
fn platform_token(_path: &Path, _file: &File, metadata: &Metadata) -> Result<Option<[u8; 32]>> {
    Ok(Some(unix_platform_token(metadata)))
}

#[cfg(unix)]
fn unix_platform_token(metadata: &Metadata) -> [u8; 32] {
    use std::os::unix::fs::MetadataExt;

    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(b"unix\0");
    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    hasher.update(metadata.ctime().to_le_bytes());
    hasher.update(metadata.ctime_nsec().to_le_bytes());
    hasher.finalize().into()
}

#[cfg(target_os = "windows")]
fn platform_token(path: &Path, file: &File, metadata: &Metadata) -> Result<Option<[u8; 32]>> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic_info = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            &mut basic_info as *mut FILE_BASIC_INFO as *mut std::ffi::c_void,
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if basic_info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(crate::CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "reparse-point provider transcript files are rejected",
        });
    }

    let mut id_info = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut id_info as *mut FILE_ID_INFO as *mut std::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(b"windows\0");
    hasher.update(id_info.VolumeSerialNumber.to_le_bytes());
    hasher.update(id_info.FileId.Identifier);
    hasher.update(basic_info.ChangeTime.to_le_bytes());
    hasher.update(basic_info.LastWriteTime.to_le_bytes());
    hasher.update(metadata.len().to_le_bytes());
    Ok(Some(hasher.finalize().into()))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn platform_token(_path: &Path, _file: &File, _metadata: &Metadata) -> Result<Option<[u8; 32]>> {
    Ok(None)
}

fn combined_token(
    platform_token: Option<[u8; 32]>,
    content_fingerprint: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    if let Some(platform_token) = platform_token {
        hasher.update(b"platform\0");
        hasher.update(platform_token);
    } else {
        hasher.update(b"portable\0");
        match content_fingerprint {
            Some(content_fingerprint) => hasher.update(content_fingerprint),
            None => hasher.update(b"missing-content-fingerprint\0"),
        }
    }
    hasher.finalize().into()
}

fn content_fingerprint(file: &mut File, metadata: &Metadata) -> Result<[u8; 32]> {
    let len = metadata.len();
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(len.to_le_bytes());
    if len <= FULL_CONTENT_FINGERPRINT_MAX_BYTES {
        hasher.update(b"full\0");
        hash_range(file, 0, len, &mut hasher)?;
    } else {
        hasher.update(b"sparse\0");
        for offset in sparse_sample_offsets(len) {
            let sample_len = SPARSE_SAMPLE_BYTES.min(len.saturating_sub(offset));
            hasher.update(offset.to_le_bytes());
            hasher.update(sample_len.to_le_bytes());
            hash_range(file, offset, sample_len, &mut hasher)?;
        }
    }
    Ok(hasher.finalize().into())
}

fn sparse_sample_offsets(len: u64) -> BTreeSet<u64> {
    let last = len.saturating_sub(SPARSE_SAMPLE_BYTES);
    [0, len / 4, len / 2, len.saturating_mul(3) / 4, last]
        .into_iter()
        .map(|offset| offset.min(last))
        .collect()
}

fn hash_range(file: &mut File, offset: u64, len: u64, hasher: &mut Sha256) -> std::io::Result<()> {
    #[cfg(test)]
    CONTENT_SAMPLE_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = len;
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(file_changed_during_observation());
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(())
}

fn file_changed_during_observation() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "ordinary file changed while it was being observed",
    )
}

#[cfg(test)]
mod tests {
    use std::{io::Write, time::Duration};

    use super::*;

    fn count_content_sample_reads<T>(operation: impl FnOnce() -> T) -> (T, usize) {
        CONTENT_SAMPLE_READS.with(|reads| reads.set(0));
        let output = operation();
        let reads = CONTENT_SAMPLE_READS.with(|reads| reads.replace(0));
        (output, reads)
    }

    #[test]
    fn bounded_content_fallback_hashes_the_complete_small_file() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"alpha-middle-omega").unwrap();
        let mut file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let first = content_fingerprint(&mut file, &metadata).unwrap();

        std::fs::write(&path, b"alpha-switch-omega").unwrap();
        let mut file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let second = content_fingerprint(&mut file, &metadata).unwrap();

        assert_ne!(first, second);
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn supported_platform_observation_does_not_sample_file_content() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, vec![b'x'; 256 * 1024]).unwrap();

        let (observation, content_reads) =
            count_content_sample_reads(|| observe_ordinary_file(&path).unwrap());

        assert_eq!(observation.len(), 256 * 1024);
        assert_eq!(content_reads, 0);
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn platform_token_detects_unsampled_same_size_rewrite_with_restored_mtime() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let source = vec![b'a'; 128 * 1024];
        std::fs::write(&path, source).unwrap();
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let first = observe_ordinary_file(&path).unwrap();

        std::thread::sleep(Duration::from_millis(2));
        let mut file = File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(16 * 1024)).unwrap();
        file.write_all(b"b").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        drop(file);
        let second = observe_ordinary_file(&path).unwrap();

        assert_eq!(first.len(), second.len());
        assert_eq!(first.modified_at(), second.modified_at());
        assert_ne!(first.token(), second.token());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_final_component() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target = temp.path().join("target.jsonl");
        let link = temp.path().join("link.jsonl");
        std::fs::write(&target, b"content\n").unwrap();
        symlink(&target, &link).unwrap();

        assert!(observe_ordinary_file(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_parent_component() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target_parent = temp.path().join("target-parent");
        let link_parent = temp.path().join("link-parent");
        std::fs::create_dir(&target_parent).unwrap();
        std::fs::write(target_parent.join("source.jsonl"), b"content\n").unwrap();
        symlink(&target_parent, &link_parent).unwrap();

        assert!(observe_ordinary_file(link_parent.join("source.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_final_component_symlink_swapped_before_open() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        let target = temp.path().join("target.jsonl");
        std::fs::write(&path, b"original\n").unwrap();
        std::fs::write(&target, b"replacement\n").unwrap();

        let result = observe_ordinary_file_inner(&path, || {
            std::fs::rename(&path, &moved).unwrap();
            symlink(&target, &path).unwrap();
        });

        assert!(result.is_err());
    }
}

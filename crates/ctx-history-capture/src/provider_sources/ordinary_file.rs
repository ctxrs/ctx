use std::{
    collections::BTreeSet,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{common::io::open_regular_provider_transcript_file, pace_current_disk_io, Result};

const TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v1\0";
const FULL_CONTENT_FINGERPRINT_MAX_BYTES: u64 = 64 * 1024;
const SPARSE_SAMPLE_BYTES: u64 = 8 * 1024;

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
    let (_, observation) = open_observed_ordinary_file(path.as_ref())?;
    Ok(observation)
}

pub(crate) fn open_observed_ordinary_file(path: &Path) -> Result<(File, OrdinaryFileObservation)> {
    let mut file = open_regular_provider_transcript_file(path)?;
    let metadata = file.metadata()?;
    let platform_before = platform_token(&file, &metadata);
    let content_fingerprint = content_fingerprint(&mut file, &metadata)?;
    let current = file.metadata()?;
    let platform_after = platform_token(&file, &current);
    if current.len() != metadata.len()
        || current.modified().ok() != metadata.modified().ok()
        || platform_after != platform_before
    {
        return Err(file_changed_during_observation().into());
    }
    let token = combined_token(platform_before, content_fingerprint);
    file.seek(SeekFrom::Start(0))?;
    Ok((
        file,
        OrdinaryFileObservation {
            len: metadata.len(),
            modified_at: metadata.modified().unwrap_or(UNIX_EPOCH),
            token,
        },
    ))
}

#[cfg(unix)]
fn platform_token(_file: &File, metadata: &Metadata) -> Option<[u8; 32]> {
    use std::os::unix::fs::MetadataExt;

    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(b"unix\0");
    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    hasher.update(metadata.ctime().to_le_bytes());
    hasher.update(metadata.ctime_nsec().to_le_bytes());
    Some(hasher.finalize().into())
}

#[cfg(windows)]
fn platform_token(file: &File, _metadata: &Metadata) -> Option<[u8; 32]> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut identity = MaybeUninit::<FILE_ID_INFO>::zeroed();
    let identity_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            identity.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::zeroed();
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if identity_ok == 0 || basic_ok == 0 {
        return None;
    }
    let identity = unsafe { identity.assume_init() };
    let basic = unsafe { basic.assume_init() };
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(b"windows\0");
    hasher.update(identity.VolumeSerialNumber.to_le_bytes());
    hasher.update(identity.FileId.Identifier);
    hasher.update(basic.ChangeTime.to_le_bytes());
    Some(hasher.finalize().into())
}

#[cfg(not(any(unix, windows)))]
fn platform_token(_file: &File, _metadata: &Metadata) -> Option<[u8; 32]> {
    None
}

fn combined_token(platform_token: Option<[u8; 32]>, content_fingerprint: [u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    if let Some(platform_token) = platform_token {
        hasher.update(b"platform\0");
        hasher.update(platform_token);
    } else {
        hasher.update(b"portable\0");
    }
    hasher.update(content_fingerprint);
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
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = len;
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        pace_current_disk_io(limit as u64);
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
    use super::*;

    #[test]
    fn content_fallback_hashes_the_complete_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"alpha-middle-omega").unwrap();
        let mut file = open_regular_provider_transcript_file(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let first = content_fingerprint(&mut file, &metadata).unwrap();

        std::fs::write(&path, b"alpha-switch-omega").unwrap();
        let mut file = open_regular_provider_transcript_file(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let second = content_fingerprint(&mut file, &metadata).unwrap();

        assert_ne!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn unix_token_detects_same_size_rewrite_with_restored_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"alpha\n").unwrap();
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let first = observe_ordinary_file(&path).unwrap();

        std::fs::write(&path, b"omega\n").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        let second = observe_ordinary_file(&path).unwrap();

        assert_eq!(first.len(), second.len());
        assert_eq!(first.modified_at(), second.modified_at());
        assert_ne!(first.token(), second.token());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_final_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.jsonl");
        let link = temp.path().join("link.jsonl");
        std::fs::write(&target, b"content\n").unwrap();
        symlink(&target, &link).unwrap();

        assert!(observe_ordinary_file(&link).is_err());
    }
}

use super::*;
use std::{fs, time::SystemTime};

pub(super) fn hash_file_metadata(digest: &mut Sha256, metadata: &fs::Metadata) {
    digest.update(metadata.len().to_be_bytes());
    hash_system_time(digest, metadata.modified().ok());
    hash_system_time(digest, metadata.created().ok());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mode().to_be_bytes());
        digest.update(metadata.nlink().to_be_bytes());
        digest.update(metadata.mtime().to_be_bytes());
        digest.update(metadata.mtime_nsec().to_be_bytes());
        digest.update(metadata.ctime().to_be_bytes());
        digest.update(metadata.ctime_nsec().to_be_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        digest.update(metadata.file_attributes().to_be_bytes());
        digest.update(metadata.creation_time().to_be_bytes());
        digest.update(metadata.last_write_time().to_be_bytes());
        digest.update(metadata.file_size().to_be_bytes());
    }
}

fn hash_system_time(digest: &mut Sha256, value: Option<SystemTime>) {
    match value.and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok()) {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_secs().to_be_bytes());
            digest.update(value.subsec_nanos().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

pub(super) fn hash_os_str(digest: &mut Sha256, value: &std::ffi::OsStr) {
    let bytes = value.as_encoded_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

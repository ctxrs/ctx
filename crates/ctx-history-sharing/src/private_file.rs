use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

use ctx_history_platform::platform_security::{
    create_private_file_new, verify_private_directory, verify_private_file_handle,
};
use serde::{de::DeserializeOwned, Serialize};

use crate::{Error, Result};

pub(crate) fn open(path: &Path) -> Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| Error::State)?
    };
    #[cfg(windows)]
    let file = ctx_history_platform::platform_security::open_verified_private_file(path)
        .map_err(|_| Error::State)?;
    #[cfg(not(any(unix, windows)))]
    let file = File::open(path).map_err(|_| Error::State)?;
    verify_private_file_handle(&file).map_err(|_| Error::State)?;
    Ok(file)
}

pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(open(path)?).map_err(|_| Error::State)
}

pub(crate) fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => read(path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::State),
    }
}

/// Same-directory private temp + atomic replacement. No partially written
/// config/checkpoint can enable sharing or discard pending bytes.
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().ok_or(Error::State)?;
    verify_private_directory(parent).map_err(|_| Error::State)?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = create_private_file_new(&temporary).map_err(|_| Error::State)?;
        serde_json::to_writer(&mut file, value).map_err(|_| Error::State)?;
        file.flush().map_err(|_| Error::State)?;
        file.sync_all().map_err(|_| Error::State)?;
        drop(file);
        // TempPath::persist uses native replacement semantics on Windows too.
        tempfile::TempPath::try_from_path(&temporary)
            .map_err(|_| Error::State)?
            .persist(path)
            .map_err(|_| Error::State)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::State)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn credential(path: &Path) -> Result<String> {
    let mut token = String::new();
    open(path)
        .map_err(|_| Error::Credentials)?
        .take(16_385)
        .read_to_string(&mut token)
        .map_err(|_| Error::Credentials)?;
    if token.len() > 16_384 {
        return Err(Error::Credentials);
    }
    Ok(token.trim_end_matches(['\r', '\n']).to_owned())
}

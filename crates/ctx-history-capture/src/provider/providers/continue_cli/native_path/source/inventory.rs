use super::*;

pub(super) struct InventoryObservation {
    pub(super) entries: usize,
    pub(super) digest: String,
    pub(super) before_token: [u8; 32],
    pub(super) after_token: [u8; 32],
    #[cfg(test)]
    pub(super) maximum_directory_sort_entries: usize,
    #[cfg(test)]
    pub(super) maximum_directory_sort_key_bytes: usize,
}

#[derive(Default)]
pub(super) struct InventoryScratch {
    #[cfg(test)]
    maximum_directory_sort_entries: usize,
    #[cfg(test)]
    maximum_directory_sort_key_bytes: usize,
}

pub(super) struct DirectoryChild {
    order_key: Vec<u8>,
    path: PathBuf,
}

pub(super) fn observe_inventory(
    root: &Path,
    mut spool: Option<&mut ContinuePathSpool>,
    mutation_watch: Option<&RootMutationWatch>,
) -> Result<InventoryObservation, ContinueNativePathError> {
    let before_token = metadata_token(root)?;
    let mut hasher = Sha256::new();
    hasher.update(INVENTORY_DIGEST_DOMAIN);
    let mut entries = 0_usize;
    let mut scratch = InventoryScratch::default();
    visit_inventory(
        root,
        root,
        0,
        &mut entries,
        &mut hasher,
        &mut spool,
        mutation_watch,
        &mut scratch,
    )?;
    let after_token = metadata_token(root)?;
    Ok(InventoryObservation {
        entries,
        digest: digest_to_hex(hasher.finalize()),
        before_token,
        after_token,
        #[cfg(test)]
        maximum_directory_sort_entries: scratch.maximum_directory_sort_entries,
        #[cfg(test)]
        maximum_directory_sort_key_bytes: scratch.maximum_directory_sort_key_bytes,
    })
}

// Recursive inventory traversal carries one shared bounded-state bundle; the
// root and current path remain separate to make containment checks explicit.
#[allow(clippy::only_used_in_recursion, clippy::too_many_arguments)]
pub(super) fn visit_inventory(
    root: &Path,
    path: &Path,
    depth: usize,
    entries: &mut usize,
    hasher: &mut Sha256,
    spool: &mut Option<&mut ContinuePathSpool>,
    mutation_watch: Option<&RootMutationWatch>,
    scratch: &mut InventoryScratch,
) -> Result<(), ContinueNativePathError> {
    if depth > MAX_CONTINUE_DIRECTORY_DEPTH {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue session tree exceeds the supported depth".to_owned(),
        });
    }
    if *entries >= MAX_CONTINUE_INVENTORY_ENTRIES {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue session tree exceeds the supported inventory limit".to_owned(),
        });
    }
    *entries = entries.saturating_add(1);
    let metadata = fs::symlink_metadata(path).map_err(|error| source_access(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "symlinked Continue inventory entries are rejected".to_owned(),
        });
    }
    if metadata.file_type().is_dir() {
        if let Some(watch) = mutation_watch {
            watch.add(path)?;
        }
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    hash_inventory_entry(hasher, relative, &metadata, path)?;

    if metadata.file_type().is_file() {
        if super::super::super::continue_session_json_path(path) {
            if let Some(spool) = spool.as_deref_mut() {
                spool.push(path)?;
            }
        }
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        return Ok(());
    }

    let mut children = Vec::new();
    #[cfg(test)]
    let mut sort_key_bytes = 0_usize;
    for child in fs::read_dir(path).map_err(|error| source_access(path, error))? {
        let child = child.map_err(|error| source_access(path, error))?;
        if *entries + children.len() >= MAX_CONTINUE_INVENTORY_ENTRIES {
            return Err(ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue session tree exceeds the supported inventory limit".to_owned(),
            });
        }
        let candidate = DirectoryChild {
            order_key: os_order_key(&child.file_name()),
            path: child.path(),
        };
        #[cfg(test)]
        {
            sort_key_bytes = sort_key_bytes.saturating_add(candidate.order_key.len());
        }
        children.push(candidate);
    }
    children.sort_by(|left, right| left.order_key.cmp(&right.order_key));
    #[cfg(test)]
    {
        scratch.maximum_directory_sort_entries =
            scratch.maximum_directory_sort_entries.max(children.len());
        scratch.maximum_directory_sort_key_bytes =
            scratch.maximum_directory_sort_key_bytes.max(sort_key_bytes);
    }
    for child in children {
        visit_inventory(
            root,
            &child.path,
            depth.saturating_add(1),
            entries,
            hasher,
            spool,
            mutation_watch,
            scratch,
        )?;
    }
    Ok(())
}

pub(super) fn hash_inventory_entry(
    hasher: &mut Sha256,
    relative: &Path,
    metadata: &Metadata,
    path: &Path,
) -> Result<(), ContinueNativePathError> {
    let encoded = encode_path(relative).ok_or_else(|| ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "Continue inventory path cannot be encoded".to_owned(),
    })?;
    hasher.update((encoded.len() as u64).to_le_bytes());
    hasher.update(encoded);
    let kind = if metadata.file_type().is_file() {
        b'f'
    } else if metadata.file_type().is_dir() {
        b'd'
    } else {
        b'o'
    };
    hasher.update([kind]);
    if metadata.file_type().is_file() {
        let observation =
            observe_ordinary_file(path).map_err(|error| source_access(path, error))?;
        hasher.update(observation.len().to_le_bytes());
        hasher.update(observation.token());
    } else {
        hasher.update(metadata_identity(metadata, path)?);
    }
    Ok(())
}

pub(super) fn metadata_token(path: &Path) -> Result<[u8; 32], ContinueNativePathError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| source_access(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "symlinked Continue inventory roots are rejected".to_owned(),
        });
    }
    if metadata.file_type().is_file() {
        return observe_ordinary_file(path)
            .map(|observation| *observation.token())
            .map_err(|error| source_access(path, error));
    }
    let mut hasher = Sha256::new();
    hasher.update(METADATA_TOKEN_DOMAIN);
    hasher.update(metadata_identity(&metadata, path)?);
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
pub(in super::super) fn metadata_identity(
    metadata: &Metadata,
    _path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    use std::os::unix::fs::MetadataExt;

    let mut bytes = Vec::with_capacity(13 * 8);
    for value in [
        metadata.dev(),
        metadata.ino(),
        u64::from(metadata.mode()),
        metadata.nlink(),
        u64::from(metadata.uid()),
        u64::from(metadata.gid()),
        metadata.size(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(windows)]
pub(in super::super) fn metadata_identity(
    metadata: &Metadata,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    windows_metadata_identity(path, metadata).map_err(|error| source_access(path, error))
}

#[cfg(windows)]
pub(super) fn windows_metadata_identity(
    path: &Path,
    metadata: &Metadata,
) -> std::io::Result<Vec<u8>> {
    use std::{
        fs::OpenOptions,
        mem::size_of,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if metadata.file_type().is_dir() {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)?;
    let current = file.metadata()?;
    if current.file_type().is_file() != metadata.file_type().is_file()
        || current.file_type().is_dir() != metadata.file_type().is_dir()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Continue inventory entry changed kind during identity observation",
        ));
    }

    let handle = file.as_raw_handle();
    let mut basic = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "reparse-point Continue inventory entries are rejected",
        ));
    }
    let mut id = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut bytes = Vec::with_capacity(68);
    bytes.extend_from_slice(&id.VolumeSerialNumber.to_le_bytes());
    bytes.extend_from_slice(&id.FileId.Identifier);
    bytes.extend_from_slice(&basic.ChangeTime.to_le_bytes());
    bytes.extend_from_slice(&basic.LastWriteTime.to_le_bytes());
    bytes.extend_from_slice(&u64::from(basic.FileAttributes).to_le_bytes());
    bytes.extend_from_slice(&current.len().to_le_bytes());
    Ok(bytes)
}

#[cfg(not(any(unix, windows)))]
pub(in super::super) fn metadata_identity(
    _metadata: &Metadata,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    Err(ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "exact Continue root authority is unavailable without stable file identity"
            .to_owned(),
    })
}

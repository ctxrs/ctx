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
    name: OsString,
}

pub(super) fn observe_inventory(
    authority: &ProviderSourceRoot,
    selected_relative: &Path,
    selected_file: bool,
    mut spool: Option<&mut ContinuePathSpool>,
    mutation_watch: Option<&RootMutationWatch>,
) -> Result<InventoryObservation, ContinueNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(INVENTORY_DIGEST_DOMAIN);
    let mut entries = 0_usize;
    let mut scratch = InventoryScratch::default();
    visit_inventory(
        authority,
        selected_relative,
        selected_file,
        0,
        &mut entries,
        &mut hasher,
        &mut spool,
        mutation_watch,
        &mut scratch,
    )?;
    authority.revalidate().map_err(|error| {
        capture_source_error(authority.named_path(), "revalidate Continue root", error)
    })?;
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(InventoryObservation {
        entries,
        digest: digest_to_hex(digest),
        before_token: digest,
        after_token: digest,
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
    authority: &ProviderSourceRoot,
    relative: &Path,
    selected_file: bool,
    depth: usize,
    entries: &mut usize,
    hasher: &mut Sha256,
    spool: &mut Option<&mut ContinuePathSpool>,
    mutation_watch: Option<&RootMutationWatch>,
    scratch: &mut InventoryScratch,
) -> Result<(), ContinueNativePathError> {
    let path = authority.named_path().join(relative);
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
    let opened = authority
        .open_path(relative)
        .map_err(|error| capture_source_error(&path, "open Continue inventory entry", error))?;
    match opened {
        OpenedProviderSourcePath::File(file) => {
            hash_inventory_file(hasher, relative, &file, &path)?;
            if super::super::super::continue_session_json_path(&path) {
                if let Some(spool) = spool.as_deref_mut() {
                    spool.push(&path)?;
                }
            }
            file.revalidate().map_err(|error| {
                capture_source_error(&path, "revalidate Continue inventory file", error)
            })?;
            return Ok(());
        }
        OpenedProviderSourcePath::Directory(directory) => {
            if selected_file {
                return Err(ContinueNativePathError::SourceChanged { path });
            }
            hash_inventory_directory(hasher, relative, &path)?;
            if let Some(watch) = mutation_watch {
                watch.add(&path)?;
            }
            let remaining = MAX_CONTINUE_INVENTORY_ENTRIES.saturating_sub(*entries);
            let names = directory
                .entries(remaining.saturating_add(1))
                .map_err(|error| {
                    capture_source_error(&path, "enumerate Continue inventory directory", error)
                })?;
            if names.len() > remaining {
                return Err(ContinueNativePathError::SourceAccess {
                    path,
                    message: "Continue session tree exceeds the supported inventory limit"
                        .to_owned(),
                });
            }
            let mut children = Vec::with_capacity(names.len());
            #[cfg(test)]
            let mut sort_key_bytes = 0_usize;
            for name in names {
                let candidate = DirectoryChild {
                    order_key: os_order_key(&name),
                    name,
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
                    authority,
                    &relative.join(child.name),
                    false,
                    depth.saturating_add(1),
                    entries,
                    hasher,
                    spool,
                    mutation_watch,
                    scratch,
                )?;
            }
            directory.revalidate().map_err(|error| {
                capture_source_error(&path, "revalidate Continue inventory directory", error)
            })?;
        }
    }
    Ok(())
}

fn hash_inventory_file(
    hasher: &mut Sha256,
    relative: &Path,
    file: &OpenedProviderSourceFile,
    path: &Path,
) -> Result<(), ContinueNativePathError> {
    hash_inventory_path(hasher, relative, path)?;
    hasher.update(*b"f");
    hasher.update(file.len().to_le_bytes());
    hasher.update(opened_metadata_identity(file, path)?);
    Ok(())
}

fn hash_inventory_directory(
    hasher: &mut Sha256,
    relative: &Path,
    path: &Path,
) -> Result<(), ContinueNativePathError> {
    hash_inventory_path(hasher, relative, path)?;
    hasher.update(*b"d");
    Ok(())
}

fn hash_inventory_path(
    hasher: &mut Sha256,
    relative: &Path,
    path: &Path,
) -> Result<(), ContinueNativePathError> {
    let encoded = encode_path(relative).ok_or_else(|| ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "Continue inventory path cannot be encoded".to_owned(),
    })?;
    hasher.update((encoded.len() as u64).to_le_bytes());
    hasher.update(encoded);
    Ok(())
}

pub(super) fn opened_file_token(
    file: &OpenedProviderSourceFile,
    path: &Path,
) -> Result<[u8; 32], ContinueNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(METADATA_TOKEN_DOMAIN);
    hasher.update(file.len().to_le_bytes());
    hasher.update(opened_metadata_identity(file, path)?);
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn opened_metadata_identity(
    file: &OpenedProviderSourceFile,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    metadata_identity(file.metadata(), path)
}

#[cfg(windows)]
fn opened_metadata_identity(
    file: &OpenedProviderSourceFile,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    windows_file_metadata_identity(file.file(), file.metadata())
        .map_err(|error| source_access(path, error))
}

#[cfg(not(any(unix, windows)))]
fn opened_metadata_identity(
    _file: &OpenedProviderSourceFile,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    Err(ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "exact Continue root authority is unavailable without stable file identity"
            .to_owned(),
    })
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

#[cfg(all(test, windows))]
pub(in super::super) fn metadata_identity(
    metadata: &Metadata,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    windows_metadata_identity(path, metadata).map_err(|error| source_access(path, error))
}

#[cfg(all(test, windows))]
pub(super) fn windows_metadata_identity(
    path: &Path,
    metadata: &Metadata,
) -> std::io::Result<Vec<u8>> {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
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
    windows_file_metadata_identity(&file, metadata)
}

#[cfg(windows)]
fn windows_file_metadata_identity(file: &File, metadata: &Metadata) -> std::io::Result<Vec<u8>> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_ID_INFO,
    };
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
    bytes.extend_from_slice(&metadata.len().to_le_bytes());
    Ok(bytes)
}

#[cfg(all(test, not(any(unix, windows))))]
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

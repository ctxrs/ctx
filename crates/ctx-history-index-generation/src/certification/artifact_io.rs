use super::*;

pub(crate) fn open_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<(File, ArtifactIdentity)> {
    open_artifact_with_alias_authority(
        root,
        generation_path,
        relative_path,
        ManagedAliasAuthority::Publication(pointer),
    )
}

pub(super) fn open_artifact_with_alias_authority(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    alias_authority: ManagedAliasAuthority<'_>,
) -> Result<(File, ArtifactIdentity)> {
    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = relative_path
        .to_str()
        .ok_or(IndexError::ChecksumMismatch)?
        .to_owned();
    let artifact_path = generation_path.join(relative_path);
    let mut unaccounted_observation: Option<(FileIdentity, u64)> = None;
    let mut stable_unaccounted_attempts = 0_usize;
    for _ in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        let Some((file, identity)) = open_artifact_file_snapshot(&artifact_path)? else {
            unaccounted_observation = None;
            stable_unaccounted_attempts = 0;
            std::thread::yield_now();
            continue;
        };
        match stable_artifact_link_snapshot_with_alias_authority(
            root,
            &artifact_path,
            relative_path,
            &file,
            &identity,
            alias_authority,
        )? {
            ArtifactLinkSnapshot::Stable(identity) => {
                return Ok((file, ArtifactIdentity { path, identity }));
            }
            ArtifactLinkSnapshot::Retry => {
                unaccounted_observation = None;
                stable_unaccounted_attempts = 0;
            }
            ArtifactLinkSnapshot::Unaccounted { identity, aliases } => {
                let observation = (identity, aliases);
                if unaccounted_observation.as_ref() == Some(&observation) {
                    stable_unaccounted_attempts = stable_unaccounted_attempts
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                } else {
                    unaccounted_observation = Some(observation);
                    stable_unaccounted_attempts = 1;
                }
                if stable_unaccounted_attempts == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(IndexError::ChecksumMismatch);
                }
            }
        }
        std::thread::yield_now();
    }
    Err(IndexError::ConcurrentGenerationChange)
}

pub(crate) fn open_authenticated_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<(File, ArtifactIdentity)> {
    open_artifact(root, generation_path, relative_path, pointer)
}

pub(crate) fn recapture_authenticated_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    file: &File,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = relative_path
        .to_str()
        .ok_or(IndexError::ChecksumMismatch)?
        .to_owned();
    let artifact_path = generation_path.join(relative_path);
    let mut unaccounted_observation: Option<(FileIdentity, u64)> = None;
    let mut stable_unaccounted_attempts = 0_usize;
    for _ in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        let current = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
        match stable_artifact_link_snapshot(
            root,
            &artifact_path,
            relative_path,
            file,
            &current,
            pointer,
        )? {
            ArtifactLinkSnapshot::Stable(identity) => {
                return Ok(ArtifactIdentity { path, identity });
            }
            ArtifactLinkSnapshot::Retry => {
                unaccounted_observation = None;
                stable_unaccounted_attempts = 0;
            }
            ArtifactLinkSnapshot::Unaccounted { identity, aliases } => {
                let observation = (identity, aliases);
                if unaccounted_observation.as_ref() == Some(&observation) {
                    stable_unaccounted_attempts = stable_unaccounted_attempts
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                } else {
                    unaccounted_observation = Some(observation);
                    stable_unaccounted_attempts = 1;
                }
                if stable_unaccounted_attempts == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(IndexError::ChecksumMismatch);
                }
            }
        }
        std::thread::yield_now();
    }
    Err(IndexError::ConcurrentGenerationChange)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArtifactLinkSnapshot {
    Stable(FileIdentity),
    Retry,
    Unaccounted {
        identity: FileIdentity,
        aliases: u64,
    },
}

/// Opens a named artifact while distinguishing hard-link topology churn from
/// replacement or payload mutation. `None` asks the bounded caller to retry a
/// snapshot changed only by a link/unlink operation.
pub(super) fn open_artifact_file_snapshot(path: &Path) -> Result<Option<(File, FileIdentity)>> {
    validate_named_regular_file(path)?;
    let file = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let opened = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    drop(named);
    let held = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    if opened == named_identity && named_identity == held {
        return Ok(Some((file, held)));
    }
    if opened.same_payload_identity(&named_identity) && named_identity.same_payload_identity(&held)
    {
        return Ok(None);
    }
    Err(IndexError::ChecksumMismatch)
}

/// Proves one stable managed-alias snapshot for an already-bound artifact.
/// Stable unaccounted hardlinks remain corruption; only an observation that
/// changed during the bounded snapshot is retryable.
pub(super) fn stable_artifact_link_snapshot(
    root: &Path,
    artifact_path: &Path,
    relative_path: &Path,
    file: &File,
    identity: &FileIdentity,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactLinkSnapshot> {
    stable_artifact_link_snapshot_with_alias_authority(
        root,
        artifact_path,
        relative_path,
        file,
        identity,
        ManagedAliasAuthority::Publication(pointer),
    )
}

pub(super) fn stable_artifact_link_snapshot_with_alias_authority(
    root: &Path,
    artifact_path: &Path,
    relative_path: &Path,
    file: &File,
    identity: &FileIdentity,
    alias_authority: ManagedAliasAuthority<'_>,
) -> Result<ArtifactLinkSnapshot> {
    let before = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
    if before != *identity {
        return if before.same_payload_identity(identity) {
            Ok(ArtifactLinkSnapshot::Retry)
        } else {
            Err(IndexError::ChecksumMismatch)
        };
    }
    let Some(alias_snapshot) =
        managed_artifact_alias_count(root, relative_path, &before, alias_authority)?
    else {
        return Ok(ArtifactLinkSnapshot::Retry);
    };
    let after_scan = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(artifact_path)?;
    let named = open_nofollow(artifact_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    drop(named);
    let final_identity = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;

    if before == after_scan && after_scan == named_identity && named_identity == final_identity {
        if alias_authority.requires_accounted_aliases() && alias_snapshot.unaccounted_aliases != 0 {
            return Ok(ArtifactLinkSnapshot::Unaccounted {
                identity: final_identity,
                aliases: alias_snapshot
                    .aliases
                    .saturating_sub(alias_snapshot.unaccounted_aliases),
            });
        }
        if alias_snapshot.aliases == 0 || alias_snapshot.aliases != final_identity.link_count() {
            if alias_snapshot.saw_unpublished_generation {
                return Ok(ArtifactLinkSnapshot::Retry);
            }
            return Ok(ArtifactLinkSnapshot::Unaccounted {
                identity: final_identity,
                aliases: alias_snapshot.aliases,
            });
        }
        return Ok(ArtifactLinkSnapshot::Stable(final_identity));
    }
    if before.same_payload_identity(&after_scan)
        && after_scan.same_payload_identity(&named_identity)
        && named_identity.same_payload_identity(&final_identity)
    {
        return Ok(ArtifactLinkSnapshot::Retry);
    }
    Err(IndexError::ChecksumMismatch)
}

pub(crate) fn recapture_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    capture_artifact(root, generation_path, relative_path, pointer)
}

pub(crate) fn capture_artifact_identity(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    capture_artifact(root, generation_path, relative_path, pointer)
}

pub(super) fn capture_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    let (file, artifact) = open_artifact(root, generation_path, relative_path, pointer)?;
    drop(file);
    Ok(artifact)
}

pub(super) fn capture_artifact_with_retained_aliases(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    retained_alias_directories: &HashSet<String>,
) -> Result<ArtifactIdentity> {
    let (file, artifact) = open_artifact_with_alias_authority(
        root,
        generation_path,
        relative_path,
        ManagedAliasAuthority::Retained(retained_alias_directories),
    )?;
    drop(file);
    Ok(artifact)
}

pub(super) fn capture_single_link_control(path: &Path) -> Result<FileIdentity> {
    let (file, identity) = open_regular_file(path)?;
    drop(file);
    if identity.link_count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(identity)
}

#[cfg(test)]
pub(super) fn capture_pointer_bound_single_link_control(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    path: &Path,
) -> Result<FileIdentity> {
    for attempt in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        match capture_single_link_control(path) {
            Ok(identity) => {
                if load_current_pointer(root)? != *pointer {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                return Ok(identity);
            }
            Err(error) => {
                if load_current_pointer(root)? != *pointer {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                if attempt + 1 == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(error);
                }
                std::thread::yield_now();
            }
        }
    }
    Err(IndexError::ConcurrentGenerationChange)
}

pub(super) fn open_regular_file(path: &Path) -> Result<(File, FileIdentity)> {
    validate_named_regular_file(path)?;
    let file = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let identity = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    #[cfg(test)]
    run_regular_file_identity_test_hook(path);
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    if identity != named_identity {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok((file, identity))
}

#[cfg(test)]
pub(super) type PathTestHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static REGULAR_FILE_IDENTITY_TEST_HOOK: std::cell::RefCell<Option<PathTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) struct RegularFileIdentityTestHookGuard(Option<PathTestHook>);

#[cfg(test)]
impl RegularFileIdentityTestHookGuard {
    pub(super) fn install(hook: impl FnMut(&Path) + 'static) -> Self {
        let previous =
            REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for RegularFileIdentityTestHookGuard {
    fn drop(&mut self) {
        REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
pub(super) fn run_regular_file_identity_test_hook(path: &Path) {
    REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| {
        if let Some(hook) = active.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

pub(super) fn open_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

pub(super) fn validate_named_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(&metadata)
        || !metadata.file_type().is_file()
    {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

pub(super) fn ensure_real_directory(path: &Path) -> Result<()> {
    if let Some(opened) = crate::read_root::registered_read_directory(path)
        .map_err(|_| IndexError::ChecksumMismatch)?
    {
        return opened
            .verify_private()
            .map_err(|_| IndexError::ChecksumMismatch);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(&metadata)
        || !metadata.file_type().is_dir()
    {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::other(
            "index artifact is not a regular file",
        ));
    }
    Ok(FileIdentity {
        length: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        links: metadata.nlink(),
    })
}

#[cfg(windows)]
pub(super) fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::{mem::size_of, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FileBasicInfo, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_ID_INFO,
        },
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FileIdentity {
        length: file.metadata()?.len(),
        volume_serial_number: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        creation_time: basic.CreationTime,
        last_write_time: basic.LastWriteTime,
        change_time: basic.ChangeTime,
        attributes: basic.FileAttributes,
        links: information.nNumberOfLinks,
    })
}

#[cfg(not(any(unix, windows)))]
pub(super) fn file_identity(_file: &File) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "strong index artifact identity is unsupported on this platform",
    ))
}

#[cfg(windows)]
pub(super) fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(super) fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

#[derive(Clone, Copy)]
pub(super) enum ManagedAliasAuthority<'a> {
    Publication(Option<&'a ActiveGenerationPointer>),
    Retained(&'a HashSet<String>),
}

impl ManagedAliasAuthority<'_> {
    fn accounts_directory(&self, directory_name: &str) -> bool {
        match self {
            Self::Publication(None) => true,
            Self::Publication(Some(pointer)) => {
                pointer.active().directory() == directory_name
                    || pointer
                        .previous()
                        .is_some_and(|slot| slot.directory() == directory_name)
            }
            Self::Retained(directories) => directories.contains(directory_name),
        }
    }

    fn tracks_unpublished_generations(&self) -> bool {
        matches!(self, Self::Publication(Some(_)))
    }

    fn requires_accounted_aliases(&self) -> bool {
        matches!(self, Self::Retained(_))
    }
}

pub(super) fn managed_artifact_alias_count(
    root: &Path,
    relative_path: &Path,
    identity: &FileIdentity,
    alias_authority: ManagedAliasAuthority<'_>,
) -> Result<Option<ManagedAliasSnapshot>> {
    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    let mut aliases = 0_u64;
    let mut unaccounted_aliases = 0_u64;
    let mut saw_unpublished_generation = false;
    for entry in fs::read_dir(generations).map_err(|_| IndexError::ChecksumMismatch)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if retryable_alias_snapshot_error(&error) => return Ok(None),
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        #[cfg(test)]
        run_alias_entry_test_hook(&entry.path());
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if retryable_alias_snapshot_error(&error) => return Ok(None),
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        let Some(directory_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !file_type.is_dir() || !is_generation_directory_name(&directory_name) {
            continue;
        }
        let accounted_directory = alias_authority.accounts_directory(&directory_name);
        if alias_authority.tracks_unpublished_generations() && !accounted_directory {
            saw_unpublished_generation = true;
        }
        let candidate = entry.path().join(relative_path);
        let (file, candidate_identity) = match open_regular_file(&candidate) {
            Ok(opened) => opened,
            Err(_) => continue,
        };
        drop(file);
        if candidate_identity.same_native_file(identity) {
            aliases = aliases.checked_add(1).ok_or(IndexError::CountOverflow)?;
            if !accounted_directory {
                unaccounted_aliases = unaccounted_aliases
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
            }
        }
    }
    Ok(Some(ManagedAliasSnapshot {
        aliases,
        unaccounted_aliases,
        saw_unpublished_generation,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ManagedAliasSnapshot {
    aliases: u64,
    unaccounted_aliases: u64,
    saw_unpublished_generation: bool,
}

pub(super) fn retryable_alias_snapshot_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ESTALE)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
thread_local! {
    static ALIAS_ENTRY_TEST_HOOK: std::cell::RefCell<Option<PathTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) struct AliasEntryTestHookGuard(Option<PathTestHook>);

#[cfg(test)]
impl AliasEntryTestHookGuard {
    pub(super) fn install(hook: impl FnMut(&Path) + 'static) -> Self {
        let previous = ALIAS_ENTRY_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for AliasEntryTestHookGuard {
    fn drop(&mut self) {
        ALIAS_ENTRY_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
pub(super) fn run_alias_entry_test_hook(path: &Path) {
    ALIAS_ENTRY_TEST_HOOK.with(|active| {
        if let Some(hook) = active.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

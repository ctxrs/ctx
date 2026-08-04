use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    GitObjectFormat, CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN,
};
use sha2::{Digest, Sha256};

use super::{
    canonical_symbolic_branch, lexical_absolute, metadata_is_link_like, object_format_name,
    utf8_lines, CandidateKind, ProbeFailure, MAX_GIT_OUTPUT_BYTES, MAX_PARENT_COMPONENTS,
};

// Mutable cache-fence files such as packed-refs routinely exceed the bounded
// stdout accepted from Git. They are read directly and hashed, so retain a
// separate finite cap without rejecting ordinary repositories.
const MAX_MUTABLE_GIT_EVIDENCE_BYTES: u64 = 8 * 1024 * 1024;

/// Cheap, non-authoritative state used only to decide whether a prior negative
/// probe may be reused. Any route or Git-geometry change invalidates the hit.
pub(in super::super) fn negative_route_geometry_state(
    path: &Path,
    kind: CandidateKind,
) -> Option<[u8; 32]> {
    if !path.is_absolute() || path.components().count() > MAX_PARENT_COMPONENTS {
        return None;
    }
    let geometry_path = match kind {
        CandidateKind::Directory => path,
        CandidateKind::File => path.parent()?,
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.negative-route-geometry.v1\0");
    digest.update([match kind {
        CandidateKind::Directory => 1,
        CandidateKind::File => 2,
    }]);
    digest.update(path.as_os_str().as_encoded_bytes());
    let mut components = geometry_path.ancestors().collect::<Vec<_>>();
    components.reverse();
    for component in components {
        let metadata = match fs::symlink_metadata(component) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                digest.update([0]);
                continue;
            }
            Err(_) => return None,
        };
        if metadata_is_link_like(&metadata) || !metadata.is_dir() {
            return None;
        }
        update_negative_route_component(&mut digest, component)?;
        for entry in [
            ".git",
            "HEAD",
            "config",
            "objects",
            "refs",
            "commondir",
            "gitdir",
        ] {
            update_negative_optional_entry(&mut digest, &component.join(entry))?;
        }
        let dot_git = component.join(".git");
        if fs::symlink_metadata(&dot_git)
            .ok()
            .is_some_and(|metadata| metadata.is_dir())
        {
            for entry in ["HEAD", "config", "objects", "refs", "commondir", "gitdir"] {
                update_negative_optional_entry(&mut digest, &dot_git.join(entry))?;
            }
        }
    }
    Some(digest.finalize().into())
}

fn update_negative_optional_entry(digest: &mut Sha256, path: &Path) -> Option<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => update_negative_entry(digest, path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update([0]);
            Some(())
        }
        Err(_) => None,
    }
}

fn update_negative_route_component(digest: &mut Sha256, path: &Path) -> Option<()> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return None;
    }
    digest.update([1]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mode().to_be_bytes());
    }
    #[cfg(windows)]
    {
        let state = windows_path_state(path).ok()?;
        digest.update(state.volume_serial_number.to_be_bytes());
        digest.update(state.file_id);
        digest.update(state.file_attributes.to_be_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        return None;
    }
    Some(())
}

fn update_negative_entry(digest: &mut Sha256, path: &Path) -> Option<()> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata_is_link_like(&metadata) {
        return None;
    }
    digest.update([1]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mode().to_be_bytes());
        digest.update(metadata.len().to_be_bytes());
        digest.update(metadata.mtime().to_be_bytes());
        digest.update(metadata.mtime_nsec().to_be_bytes());
        digest.update(metadata.ctime().to_be_bytes());
        digest.update(metadata.ctime_nsec().to_be_bytes());
    }
    #[cfg(windows)]
    {
        let state = windows_path_state(path).ok()?;
        digest.update(state.volume_serial_number.to_be_bytes());
        digest.update(state.file_id);
        digest.update(state.file_attributes.to_be_bytes());
        digest.update(state.length.to_be_bytes());
        digest.update(state.last_write_time.to_be_bytes());
        digest.update(state.change_time.to_be_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        return None;
    }
    Some(())
}

pub(in super::super) fn validate_candidate_route(
    path: &Path,
    kind: CandidateKind,
) -> Result<PathBuf, ProbeFailure> {
    if !path.is_absolute() || path.components().count() > MAX_PARENT_COMPONENTS {
        return Err(ProbeFailure::Unsafe("unbounded_or_relative_candidate"));
    }
    let probe = match kind {
        CandidateKind::Directory => path.to_path_buf(),
        CandidateKind::File => {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata_is_link_like(&metadata) => {
                    return Err(ProbeFailure::Unsafe("file_candidate_is_symlink"));
                }
                Ok(metadata) if !metadata.is_file() => {
                    return Err(ProbeFailure::Unsafe("file_candidate_is_not_file"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(ProbeFailure::Unsafe("file_candidate_metadata_failed"));
                }
            }
            let mut parent = path
                .parent()
                .ok_or(ProbeFailure::Unsafe("file_candidate_has_no_parent"))?;
            loop {
                match fs::symlink_metadata(parent) {
                    Ok(metadata) if metadata_is_link_like(&metadata) => {
                        return Err(ProbeFailure::Unsafe("candidate_contains_symlink"));
                    }
                    Ok(metadata) if metadata.is_dir() => break parent.to_path_buf(),
                    Ok(_) => {
                        return Err(ProbeFailure::Unsafe(
                            "file_candidate_parent_is_not_directory",
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        parent = parent.parent().ok_or(ProbeFailure::Missing)?;
                    }
                    Err(_) => {
                        return Err(ProbeFailure::Unsafe("candidate_metadata_failed"));
                    }
                }
            }
        }
    };
    let mut components = probe.ancestors().collect::<Vec<_>>();
    components.reverse();
    for component in components {
        let metadata = fs::symlink_metadata(component).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProbeFailure::Missing
            } else {
                ProbeFailure::Unsafe("candidate_metadata_failed")
            }
        })?;
        if metadata_is_link_like(&metadata) {
            return Err(ProbeFailure::Unsafe("candidate_contains_symlink"));
        }
    }
    let metadata = fs::metadata(&probe).map_err(|_| ProbeFailure::Missing)?;
    if !metadata.is_dir() {
        return Err(ProbeFailure::Unsafe(
            "candidate_probe_base_is_not_directory",
        ));
    }
    Ok(probe)
}

pub(super) fn route_fingerprint(path: &Path) -> Result<[u8; 32], ProbeFailure> {
    let mut digest = Sha256::new();
    let mut components = path.ancestors().collect::<Vec<_>>();
    components.reverse();
    if components.len() > MAX_PARENT_COMPONENTS {
        return Err(ProbeFailure::Unsafe("parent_route_limit_exceeded"));
    }
    for component in components {
        digest.update(component.as_os_str().as_encoded_bytes());
        // A sibling created under a shared ancestor (for example `/tmp`) must
        // not look like drift in this candidate's route. Stable filesystem
        // identity still detects replacement of any component we traversed.
        digest.update(path_identity_fingerprint(component)?);
    }
    Ok(digest.finalize().into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RepositoryGeometryState {
    pub(super) git_dir: PathBuf,
    pub(super) common_dir: PathBuf,
    pub(super) fingerprint: [u8; 32],
}

/// Resolves the root's current Git marker without consulting cached Git paths.
/// Marker identity/content and linked-worktree indirection are retained only as
/// a local cache fence; they do not become logical repository identity.
pub(super) fn repository_geometry_state(
    root: &Path,
) -> Result<RepositoryGeometryState, ProbeFailure> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = root;
        Err(ProbeFailure::PlatformUnsupported)
    }
    #[cfg(any(unix, windows))]
    {
        let marker = root.join(".git");
        let mut digest = Sha256::new();
        digest.update(b"ctx.repository.cache-geometry.v1\0");
        let git_dir =
            match update_repository_geometry_entry(&mut digest, b"root_git_marker", &marker)? {
                RepositoryGeometryEntry::Directory => marker.clone(),
                RepositoryGeometryEntry::File(value) => {
                    let line =
                        parse_required_geometry_line(&value, "repository_git_pointer_invalid")?;
                    let path = line
                        .strip_prefix("gitdir: ")
                        .ok_or(ProbeFailure::Unsafe("repository_git_pointer_invalid"))?;
                    lexical_absolute(path, Some(root))
                        .ok_or(ProbeFailure::Unsafe("repository_git_pointer_invalid"))?
                }
                RepositoryGeometryEntry::Missing => {
                    return Err(ProbeFailure::Unsafe("repository_git_marker_missing"));
                }
            };
        validate_candidate_route(&git_dir, CandidateKind::Directory)?;
        update_repository_geometry_path(&mut digest, b"resolved_git_dir", &git_dir);

        let commondir_marker = git_dir.join("commondir");
        let common_dir = match update_repository_geometry_entry(
            &mut digest,
            b"commondir_marker",
            &commondir_marker,
        )? {
            RepositoryGeometryEntry::Missing => git_dir.clone(),
            RepositoryGeometryEntry::File(value) => {
                let path =
                    parse_required_geometry_line(&value, "repository_commondir_pointer_invalid")?;
                lexical_absolute(path, Some(&git_dir))
                    .ok_or(ProbeFailure::Unsafe("repository_commondir_pointer_invalid"))?
            }
            RepositoryGeometryEntry::Directory => {
                return Err(ProbeFailure::Unsafe(
                    "repository_commondir_marker_is_not_file",
                ));
            }
        };
        validate_candidate_route(&common_dir, CandidateKind::Directory)?;
        update_repository_geometry_path(&mut digest, b"resolved_common_dir", &common_dir);

        let gitdir_marker = git_dir.join("gitdir");
        match update_repository_geometry_entry(
            &mut digest,
            b"worktree_gitdir_marker",
            &gitdir_marker,
        )? {
            RepositoryGeometryEntry::Missing if common_dir == git_dir => {}
            RepositoryGeometryEntry::Missing => {
                return Err(ProbeFailure::Unsafe("repository_worktree_backlink_missing"));
            }
            RepositoryGeometryEntry::File(value) => {
                let path =
                    parse_required_geometry_line(&value, "repository_worktree_backlink_invalid")?;
                let backlink = lexical_absolute(path, Some(&git_dir))
                    .ok_or(ProbeFailure::Unsafe("repository_worktree_backlink_invalid"))?;
                if backlink != marker {
                    return Err(ProbeFailure::Unsafe(
                        "repository_worktree_backlink_mismatch",
                    ));
                }
            }
            RepositoryGeometryEntry::Directory => {
                return Err(ProbeFailure::Unsafe(
                    "repository_worktree_backlink_is_not_file",
                ));
            }
        }

        Ok(RepositoryGeometryState {
            git_dir,
            common_dir,
            fingerprint: digest.finalize().into(),
        })
    }
}

#[cfg(any(unix, windows))]
enum RepositoryGeometryEntry {
    Missing,
    File(Vec<u8>),
    Directory,
}

#[cfg(unix)]
fn update_repository_geometry_entry(
    digest: &mut Sha256,
    label: &[u8],
    path: &Path,
) -> Result<RepositoryGeometryEntry, ProbeFailure> {
    use std::os::unix::fs::MetadataExt;

    digest.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(label);
    let opening = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update([0]);
            return Ok(RepositoryGeometryEntry::Missing);
        }
        Err(_) => {
            return Err(ProbeFailure::Failed("repository_geometry_metadata_failed"));
        }
    };
    if metadata_is_link_like(&opening) {
        return Err(ProbeFailure::Unsafe(
            "repository_geometry_marker_is_symlink",
        ));
    }
    let identity = [opening.dev(), opening.ino(), u64::from(opening.mode())];
    if opening.is_dir() {
        digest.update([1]);
        for part in identity {
            digest.update(part.to_be_bytes());
        }
        return Ok(RepositoryGeometryEntry::Directory);
    }
    if !opening.is_file() {
        return Err(ProbeFailure::Unsafe(
            "repository_geometry_marker_is_not_file_or_directory",
        ));
    }
    if opening.len() > MAX_GIT_OUTPUT_BYTES as u64 {
        return Err(ProbeFailure::Failed(
            "repository_geometry_marker_limit_exceeded",
        ));
    }
    let value = fs::read(path)
        .map_err(|_| ProbeFailure::Failed("repository_geometry_marker_read_failed"))?;
    let closing = fs::symlink_metadata(path).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    if !closing.is_file()
        || metadata_is_link_like(&closing)
        || opening.dev() != closing.dev()
        || opening.ino() != closing.ino()
        || opening.mode() != closing.mode()
        || opening.len() != closing.len()
        || opening.mtime() != closing.mtime()
        || opening.mtime_nsec() != closing.mtime_nsec()
        || opening.ctime() != closing.ctime()
        || opening.ctime_nsec() != closing.ctime_nsec()
    {
        return Err(ProbeFailure::ConcurrentDrift);
    }
    digest.update([2]);
    for part in identity {
        digest.update(part.to_be_bytes());
    }
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(&value);
    Ok(RepositoryGeometryEntry::File(value))
}

#[cfg(windows)]
fn update_repository_geometry_entry(
    digest: &mut Sha256,
    label: &[u8],
    path: &Path,
) -> Result<RepositoryGeometryEntry, ProbeFailure> {
    digest.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(label);
    let named_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update([0]);
            return Ok(RepositoryGeometryEntry::Missing);
        }
        Err(_) => {
            return Err(ProbeFailure::Failed("repository_geometry_metadata_failed"));
        }
    };
    if metadata_is_link_like(&named_metadata) {
        return Err(ProbeFailure::Unsafe(
            "repository_geometry_marker_is_symlink",
        ));
    }

    let mut file = open_windows_identity_path(path)
        .map_err(|_| ProbeFailure::Failed("repository_geometry_metadata_failed"))?;
    let opening = windows_file_state(&file)
        .map_err(|_| ProbeFailure::Failed("repository_geometry_metadata_failed"))?;
    if opening.is_reparse_point() {
        return Err(ProbeFailure::Unsafe(
            "repository_geometry_marker_is_symlink",
        ));
    }
    if opening.is_directory() {
        if !named_metadata.is_dir() {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        let closing = windows_file_state(&file).map_err(|_| ProbeFailure::ConcurrentDrift)?;
        let named = windows_path_state(path).map_err(|_| ProbeFailure::ConcurrentDrift)?;
        if closing != opening || named != opening {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        digest.update([1]);
        opening.update_stable_identity(digest);
        digest.update(opening.file_attributes.to_be_bytes());
        return Ok(RepositoryGeometryEntry::Directory);
    }
    if !named_metadata.is_file() {
        return Err(ProbeFailure::Unsafe(
            "repository_geometry_marker_is_not_file_or_directory",
        ));
    }
    if opening.length > MAX_GIT_OUTPUT_BYTES as u64 {
        return Err(ProbeFailure::Failed(
            "repository_geometry_marker_limit_exceeded",
        ));
    }
    let mut value = Vec::with_capacity(opening.length as usize);
    file.by_ref()
        .take(MAX_GIT_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut value)
        .map_err(|_| ProbeFailure::Failed("repository_geometry_marker_read_failed"))?;
    if value.len() > MAX_GIT_OUTPUT_BYTES {
        return Err(ProbeFailure::Failed(
            "repository_geometry_marker_limit_exceeded",
        ));
    }
    let closing = windows_file_state(&file).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    let named = windows_path_state(path).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    if closing != opening || named != opening {
        return Err(ProbeFailure::ConcurrentDrift);
    }
    digest.update([2]);
    opening.update_stable_identity(digest);
    digest.update(opening.file_attributes.to_be_bytes());
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(&value);
    Ok(RepositoryGeometryEntry::File(value))
}

#[cfg(any(unix, windows))]
fn update_repository_geometry_path(digest: &mut Sha256, label: &[u8], path: &Path) {
    digest.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(label);
    let value = path.as_os_str().as_encoded_bytes();
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

#[cfg(any(unix, windows))]
fn parse_required_geometry_line<'a>(
    value: &'a [u8],
    failure: &'static str,
) -> Result<&'a str, ProbeFailure> {
    let lines = utf8_lines(value).map_err(|_| ProbeFailure::Unsafe(failure))?;
    match lines.as_slice() {
        [line] if !line.is_empty() => Ok(line),
        _ => Err(ProbeFailure::Unsafe(failure)),
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowsFileState {
    volume_serial_number: u64,
    file_id: [u8; 16],
    creation_time: i64,
    last_write_time: i64,
    change_time: i64,
    file_attributes: u32,
    length: u64,
}

#[cfg(windows)]
impl WindowsFileState {
    fn is_directory(self) -> bool {
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;

        self.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }

    fn is_reparse_point(self) -> bool {
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        self.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    fn update_stable_identity(self, digest: &mut Sha256) {
        digest.update(self.volume_serial_number.to_be_bytes());
        digest.update(self.file_id);
    }
}

#[cfg(windows)]
fn open_windows_identity_path(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn windows_path_state(path: &Path) -> std::io::Result<WindowsFileState> {
    windows_file_state(&open_windows_identity_path(path)?)
}

#[cfg(windows)]
fn windows_file_state(file: &fs::File) -> std::io::Result<WindowsFileState> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
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
    Ok(WindowsFileState {
        volume_serial_number: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        creation_time: basic.CreationTime,
        last_write_time: basic.LastWriteTime,
        change_time: basic.ChangeTime,
        file_attributes: basic.FileAttributes,
        length: file.metadata()?.len(),
    })
}

pub(super) fn repository_mutable_evidence_state(
    git_dir: &Path,
    common_dir: &Path,
    branch: Option<&str>,
) -> Result<[u8; 32], ProbeFailure> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.mutable-binding-evidence.v1\0");
    for (label, path) in [
        ("git_head", git_dir.join("HEAD")),
        ("git_commondir", git_dir.join("commondir")),
        ("git_gitdir", git_dir.join("gitdir")),
        ("worktree_config", git_dir.join("config.worktree")),
        ("common_config", common_dir.join("config")),
        ("packed_refs", common_dir.join("packed-refs")),
    ] {
        update_mutable_evidence_entry(&mut digest, label.as_bytes(), &path)?;
    }
    if let Some(branch) = branch {
        if !canonical_symbolic_branch(branch) {
            return Err(ProbeFailure::Unsafe("git_branch_is_not_canonical"));
        }
        update_mutable_evidence_entry(&mut digest, b"symbolic_branch", &common_dir.join(branch))?;
    }
    Ok(digest.finalize().into())
}

fn update_mutable_evidence_entry(
    digest: &mut Sha256,
    label: &[u8],
    path: &Path,
) -> Result<(), ProbeFailure> {
    digest.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(label);
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata_is_link_like(&metadata) => {
            Err(ProbeFailure::Unsafe("mutable_git_evidence_is_symlink"))
        }
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_MUTABLE_GIT_EVIDENCE_BYTES => {
            let value = read_mutable_evidence_file(path, &metadata)?;
            digest.update([1]);
            digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            digest.update(value);
            Ok(())
        }
        Ok(metadata) if metadata.is_file() => {
            Err(ProbeFailure::Failed("mutable_git_evidence_limit_exceeded"))
        }
        Ok(metadata) if metadata.is_dir() => {
            digest.update([2]);
            digest.update(path_identity_fingerprint(path)?);
            Ok(())
        }
        Ok(_) => Err(ProbeFailure::Unsafe("mutable_git_evidence_is_not_file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update([0]);
            Ok(())
        }
        Err(_) => Err(ProbeFailure::Failed("mutable_git_evidence_metadata_failed")),
    }
}

fn read_mutable_evidence_bounded(
    file: &mut fs::File,
    opening_length: u64,
) -> Result<Vec<u8>, ProbeFailure> {
    let capacity =
        usize::try_from(opening_length.min(MAX_MUTABLE_GIT_EVIDENCE_BYTES)).unwrap_or_default();
    let mut value = Vec::with_capacity(capacity);
    file.by_ref()
        .take(MAX_MUTABLE_GIT_EVIDENCE_BYTES + 1)
        .read_to_end(&mut value)
        .map_err(|_| ProbeFailure::Failed("mutable_git_evidence_read_failed"))?;
    if u64::try_from(value.len()).unwrap_or(u64::MAX) > MAX_MUTABLE_GIT_EVIDENCE_BYTES {
        return Err(ProbeFailure::Failed("mutable_git_evidence_limit_exceeded"));
    }
    Ok(value)
}

#[cfg(unix)]
fn read_mutable_evidence_file(
    path: &Path,
    named_opening: &fs::Metadata,
) -> Result<Vec<u8>, ProbeFailure> {
    read_mutable_evidence_file_with_after_read(path, named_opening, || {})
}

#[cfg(unix)]
fn read_mutable_evidence_file_with_after_read(
    path: &Path,
    named_opening: &fs::Metadata,
    after_read: impl FnOnce(),
) -> Result<Vec<u8>, ProbeFailure> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ProbeFailure::Failed("mutable_git_evidence_read_failed"))?;
    let descriptor_opening = file
        .metadata()
        .map_err(|_| ProbeFailure::Failed("mutable_git_evidence_metadata_failed"))?;
    if !descriptor_opening.is_file()
        || metadata_is_link_like(&descriptor_opening)
        || !unix_file_state_matches(named_opening, &descriptor_opening)
    {
        return Err(ProbeFailure::ConcurrentDrift);
    }
    if descriptor_opening.len() > MAX_MUTABLE_GIT_EVIDENCE_BYTES {
        return Err(ProbeFailure::Failed("mutable_git_evidence_limit_exceeded"));
    }
    let value = read_mutable_evidence_bounded(&mut file, descriptor_opening.len())?;
    after_read();
    let descriptor_closing = file.metadata().map_err(|_| ProbeFailure::ConcurrentDrift)?;
    let named_closing = fs::symlink_metadata(path).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    if !descriptor_closing.is_file()
        || !named_closing.is_file()
        || metadata_is_link_like(&named_closing)
        || !unix_file_state_matches(&descriptor_opening, &descriptor_closing)
        || !unix_file_state_matches(&descriptor_opening, &named_closing)
    {
        return Err(ProbeFailure::ConcurrentDrift);
    }
    Ok(value)
}

#[cfg(unix)]
fn unix_file_state_matches(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    first.dev() == second.dev()
        && first.ino() == second.ino()
        && first.mode() == second.mode()
        && first.len() == second.len()
        && first.mtime() == second.mtime()
        && first.mtime_nsec() == second.mtime_nsec()
        && first.ctime() == second.ctime()
        && first.ctime_nsec() == second.ctime_nsec()
}

#[cfg(windows)]
fn read_mutable_evidence_file(
    path: &Path,
    _named_opening: &fs::Metadata,
) -> Result<Vec<u8>, ProbeFailure> {
    let mut file = open_windows_identity_path(path)
        .map_err(|_| ProbeFailure::Failed("mutable_git_evidence_read_failed"))?;
    let descriptor_opening = windows_file_state(&file)
        .map_err(|_| ProbeFailure::Failed("mutable_git_evidence_metadata_failed"))?;
    let named_opening = windows_path_state(path).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    if descriptor_opening.is_reparse_point() {
        return Err(ProbeFailure::Unsafe("mutable_git_evidence_is_symlink"));
    }
    if descriptor_opening.is_directory() || descriptor_opening != named_opening {
        return Err(ProbeFailure::ConcurrentDrift);
    }
    if descriptor_opening.length > MAX_MUTABLE_GIT_EVIDENCE_BYTES {
        return Err(ProbeFailure::Failed("mutable_git_evidence_limit_exceeded"));
    }
    let value = read_mutable_evidence_bounded(&mut file, descriptor_opening.length)?;
    let descriptor_closing =
        windows_file_state(&file).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    let named_closing = windows_path_state(path).map_err(|_| ProbeFailure::ConcurrentDrift)?;
    if descriptor_closing != descriptor_opening || named_closing != descriptor_opening {
        return Err(ProbeFailure::ConcurrentDrift);
    }
    Ok(value)
}

#[cfg(not(any(unix, windows)))]
fn read_mutable_evidence_file(
    _path: &Path,
    _named_opening: &fs::Metadata,
) -> Result<Vec<u8>, ProbeFailure> {
    Err(ProbeFailure::PlatformUnsupported)
}

#[cfg(test)]
mod mutable_evidence_tests {
    use super::*;

    #[test]
    fn bounded_descriptor_read_rejects_post_snapshot_growth() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("grown-packed-refs");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_MUTABLE_GIT_EVIDENCE_BYTES + 1).unwrap();
        drop(file);
        let mut file = fs::File::open(path).unwrap();

        let result = read_mutable_evidence_bounded(&mut file, MAX_MUTABLE_GIT_EVIDENCE_BYTES);

        assert!(matches!(
            result,
            Err(ProbeFailure::Failed("mutable_git_evidence_limit_exceeded"))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_rejects_post_read_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mutated-packed-refs");
        fs::write(&path, b"opening\n").unwrap();
        let opening = fs::symlink_metadata(&path).unwrap();

        let result = read_mutable_evidence_file_with_after_read(&path, &opening, || {
            fs::write(&path, b"changed-and-grown\n").unwrap();
        });

        assert!(matches!(result, Err(ProbeFailure::ConcurrentDrift)));
    }
}

/// Revision 1 local-root authorization fingerprint.
///
/// SHA-256 input is `CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN`, big-endian
/// u16 version 1, then `certified_root`, `git_dir`, and `common_dir`. Unix
/// identities are `[tag=1][u64 label length][label][u64 dev][u64 ino]`;
/// Windows identities are `[tag=2][u64 label length][label][u64 volume]
/// [16-byte file id]`. Object format is `[tag=4][u64 value length]
/// [sha1|sha256]`. Paths and mutable Git state are excluded.
pub(super) fn repository_local_root_authorization_fingerprint(
    root: &Path,
    git_dir: &Path,
    common_dir: &Path,
    object_format: GitObjectFormat,
) -> Result<[u8; 32], ProbeFailure> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (root, git_dir, common_dir, object_format);
        return Err(ProbeFailure::PlatformUnsupported);
    }
    #[cfg(any(unix, windows))]
    {
        let mut digest = Sha256::new();
        digest.update(CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN);
        digest.update(1_u16.to_be_bytes());
        for (label, path) in [
            (b"certified_root".as_slice(), root),
            (b"git_dir".as_slice(), git_dir),
            (b"common_dir".as_slice(), common_dir),
        ] {
            update_repository_local_root_authorization_fingerprint_entry(&mut digest, label, path)?;
        }
        let object_format = object_format_name(object_format);
        digest.update([4]);
        digest.update(
            u64::try_from(object_format.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(object_format);
        Ok(digest.finalize().into())
    }
}

#[cfg(unix)]
fn update_repository_local_root_authorization_fingerprint_entry(
    digest: &mut Sha256,
    label: &[u8],
    path: &Path,
) -> Result<(), ProbeFailure> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ProbeFailure::Unsafe("repository_identity_metadata_failed"))?;
    if metadata_is_link_like(&metadata) {
        return Err(ProbeFailure::Unsafe("repository_identity_path_is_symlink"));
    }
    digest.update([1]);
    digest.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(label);
    digest.update(metadata.dev().to_be_bytes());
    digest.update(metadata.ino().to_be_bytes());
    Ok(())
}

#[cfg(windows)]
fn update_repository_local_root_authorization_fingerprint_entry(
    digest: &mut Sha256,
    label: &[u8],
    path: &Path,
) -> Result<(), ProbeFailure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ProbeFailure::Unsafe("repository_identity_metadata_failed"))?;
    if metadata_is_link_like(&metadata) {
        return Err(ProbeFailure::Unsafe("repository_identity_path_is_symlink"));
    }
    let state = windows_path_state(path)
        .map_err(|_| ProbeFailure::Unsafe("repository_identity_metadata_failed"))?;
    if state.is_reparse_point() {
        return Err(ProbeFailure::Unsafe("repository_identity_path_is_symlink"));
    }
    digest.update([2]);
    digest.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(label);
    state.update_stable_identity(digest);
    Ok(())
}

pub(super) fn path_identity_fingerprint(path: &Path) -> Result<[u8; 32], ProbeFailure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ProbeFailure::Unsafe("repository_identity_metadata_failed"))?;
    if metadata_is_link_like(&metadata) {
        return Err(ProbeFailure::Unsafe("repository_identity_path_is_symlink"));
    }
    let mut digest = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
    }
    #[cfg(windows)]
    {
        let state = windows_path_state(path)
            .map_err(|_| ProbeFailure::Unsafe("repository_identity_metadata_failed"))?;
        if state.is_reparse_point() {
            return Err(ProbeFailure::Unsafe("repository_identity_path_is_symlink"));
        }
        state.update_stable_identity(&mut digest);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, metadata, digest);
        return Err(ProbeFailure::PlatformUnsupported);
    }
    Ok(digest.finalize().into())
}

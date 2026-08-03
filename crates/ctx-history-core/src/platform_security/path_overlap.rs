use std::{
    fs::{self, Metadata},
    io,
    path::{Component, Path, PathBuf},
};
#[cfg(windows)]
use std::{
    mem::MaybeUninit,
    os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _},
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPath {
    canonical: PathBuf,
    existing_identities: Vec<FileIdentity>,
    terminal_identity: Option<FileIdentity>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume: u32,
    index: u64,
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity;

pub(super) fn validate_provider_source_outside_data_root(
    data_root: &Path,
    source_root: &Path,
) -> io::Result<()> {
    validate_provider_source_outside_data_root_with(data_root, source_root, || {})
}

fn validate_provider_source_outside_data_root_with(
    data_root: &Path,
    source_root: &Path,
    after_source_observation: impl FnOnce(),
) -> io::Result<()> {
    let data_root = absolute_data_root_path(data_root)?;
    validate_absolute_path(source_root, "provider source root")?;
    let source_root = normalize_platform_namespace_alias(source_root);

    let source_before = inspect_named_endpoint(&source_root, false)?;
    after_source_observation();
    let data = resolve_path(&data_root, true)?;
    let source = resolve_path(&source_root, false)?;
    let source_after = inspect_named_endpoint(&source_root, false)?;
    if source_before != source_after {
        return Err(overlap_error(
            "provider source root changed during overlap validation",
        ));
    }

    let lexical_overlap = data.canonical == source.canonical
        || data.canonical.starts_with(&source.canonical)
        || source.canonical.starts_with(&data.canonical);
    let identity_overlap = source
        .terminal_identity
        .is_some_and(|identity| data.existing_identities.contains(&identity))
        || data
            .terminal_identity
            .is_some_and(|identity| source.existing_identities.contains(&identity));
    if lexical_overlap || identity_overlap {
        return Err(overlap_error(
            "provider source root overlaps or contains the ctx data root",
        ));
    }
    Ok(())
}

pub(super) fn absolute_data_root_path(path: &Path) -> io::Result<PathBuf> {
    let label = "ctx data root";
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} must not be empty"),
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{label} escapes its filesystem root"),
                    ));
                }
            }
        }
    }
    validate_absolute_path(&normalized, label)?;
    Ok(normalize_platform_namespace_alias(&normalized))
}

pub(super) fn normalize_platform_namespace_alias(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        rewrite_macos_var_alias(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.to_path_buf()
    }
}

#[cfg(any(target_os = "macos", test))]
fn rewrite_macos_var_alias(path: &Path) -> PathBuf {
    let Ok(suffix) = path.strip_prefix("/var") else {
        return path.to_path_buf();
    };
    Path::new("/private/var").join(suffix)
}

fn validate_absolute_path(path: &Path, label: &str) -> io::Result<()> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} must be absolute and traversal-free"),
        ));
    }
    Ok(())
}

fn resolve_path(path: &Path, require_directory: bool) -> io::Result<ResolvedPath> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    let endpoint = loop {
        match fs::symlink_metadata(&existing) {
            Ok(metadata) => break Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "path has no existing canonical ancestor",
                    )
                })?;
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    };
    let endpoint = endpoint.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "path has no existing canonical ancestor",
        )
    })?;
    reject_reparse_or_symlink(&endpoint)?;
    reject_intermediate_links(&existing)?;
    if missing.is_empty() && require_directory && !endpoint.is_dir() {
        return Err(overlap_error("ctx data root is not a directory"));
    }

    let mut canonical = fs::canonicalize(&existing)?;
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    let existing_path = if missing.is_empty() {
        canonical.clone()
    } else {
        fs::canonicalize(&existing)?
    };
    let existing_identities = identity_chain(&existing_path)?;
    let terminal_identity = if missing.is_empty() {
        file_identity(&existing_path, &endpoint)?
    } else {
        None
    };
    Ok(ResolvedPath {
        canonical,
        existing_identities,
        terminal_identity,
    })
}

fn reject_intermediate_links(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor)?;
        reject_reparse_or_symlink(&metadata)?;
    }
    Ok(())
}

fn inspect_named_endpoint(
    path: &Path,
    require_directory: bool,
) -> io::Result<Option<FileIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            reject_reparse_or_symlink(&metadata)?;
            if require_directory && !metadata.is_dir() {
                return Err(overlap_error("ctx data root is not a directory"));
            }
            file_identity(path, &metadata)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn identity_chain(path: &Path) -> io::Result<Vec<FileIdentity>> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    let mut identities = Vec::with_capacity(ancestors.len());
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor)?;
        reject_reparse_or_symlink(&metadata)?;
        if let Some(identity) = file_identity(ancestor, &metadata)? {
            identities.push(identity);
        }
    }
    Ok(identities)
}

#[cfg(unix)]
fn file_identity(_path: &Path, metadata: &Metadata) -> io::Result<Option<FileIdentity>> {
    use std::os::unix::fs::MetadataExt as _;

    Ok(Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }))
}

#[cfg(windows)]
fn file_identity(path: &Path, metadata: &Metadata) -> io::Result<Option<FileIdentity>> {
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if metadata.is_dir() {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)?;
    let opened = file.metadata()?;
    reject_reparse_or_symlink(&opened)?;
    if opened.file_type() != metadata.file_type() {
        return Err(overlap_error(
            "provider/data root endpoint changed during identity inspection",
        ));
    }

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `file` owns a live handle and `information` is a valid out pointer.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized the structure.
    let information = unsafe { information.assume_init() };
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(overlap_error(
            "provider/data root endpoint is a symlink or reparse point",
        ));
    }
    Ok(Some(FileIdentity {
        volume: information.dwVolumeSerialNumber,
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    }))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_path: &Path, _metadata: &Metadata) -> io::Result<Option<FileIdentity>> {
    Ok(None)
}

fn reject_reparse_or_symlink(metadata: &Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || is_windows_reparse_point(metadata) {
        Err(overlap_error(
            "provider/data root endpoint is a symlink or reparse point",
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn overlap_error(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_var_alias_rewrite_is_component_bounded() {
        assert_eq!(
            rewrite_macos_var_alias(Path::new("/var")),
            Path::new("/private/var")
        );
        assert_eq!(
            rewrite_macos_var_alias(Path::new("/var/folders/user/data")),
            Path::new("/private/var/folders/user/data")
        );
        assert_eq!(
            rewrite_macos_var_alias(Path::new("/var/folders/user/data")),
            rewrite_macos_var_alias(Path::new("/private/var/folders/user/data"))
        );
        assert_eq!(
            rewrite_macos_var_alias(Path::new("/variant/data")),
            Path::new("/variant/data")
        );
        assert_eq!(
            rewrite_macos_var_alias(Path::new("var/data")),
            Path::new("var/data")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_platform_normalization_leaves_var_unchanged() {
        assert_eq!(
            normalize_platform_namespace_alias(Path::new("/var/folders/user/data")),
            Path::new("/var/folders/user/data")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_var_alias_cannot_bypass_overlap_validation() {
        let temp = tempfile::tempdir_in("/private/var/tmp").unwrap();
        let data = temp.path().join("data");
        let source = data.join("provider");
        fs::create_dir_all(&source).unwrap();

        let alias_data = Path::new("/var").join(data.strip_prefix("/private/var").unwrap());
        let alias_source = Path::new("/var").join(source.strip_prefix("/private/var").unwrap());

        assert!(validate_provider_source_outside_data_root(&alias_data, &source).is_err());
        assert!(validate_provider_source_outside_data_root(&data, &alias_source).is_err());
    }

    #[test]
    fn equal_ancestor_and_descendant_roots_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("provider");
        fs::create_dir_all(data.join("nested")).unwrap();
        fs::create_dir_all(source.join("nested")).unwrap();

        assert!(validate_provider_source_outside_data_root(&data, &data).is_err());
        assert!(validate_provider_source_outside_data_root(&data, &data.join("nested")).is_err());
        assert!(
            validate_provider_source_outside_data_root(&source.join("nested"), &source).is_err()
        );
        validate_provider_source_outside_data_root(&data, &source).unwrap();
    }

    #[test]
    fn relative_data_root_preserves_disjoint_and_overlap_results() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(&cwd).unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("provider");
        fs::create_dir(&data).unwrap();
        fs::create_dir(&source).unwrap();
        let relative_data = data.strip_prefix(&cwd).unwrap();

        validate_provider_source_outside_data_root(relative_data, &source).unwrap();
        assert!(validate_provider_source_outside_data_root(relative_data, &data).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_is_rejected_even_when_target_is_disjoint() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let target = temp.path().join("provider");
        let source = temp.path().join("provider-link");
        fs::create_dir(&data).unwrap();
        fs::create_dir(&target).unwrap();
        symlink(&target, &source).unwrap();

        assert!(validate_provider_source_outside_data_root(&data, &source).is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn source_swap_during_validation_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("provider");
        let replacement = temp.path().join("replacement");
        fs::create_dir(&data).unwrap();
        fs::create_dir(&source).unwrap();
        fs::create_dir(&replacement).unwrap();

        let result = validate_provider_source_outside_data_root_with(&data, &source, || {
            fs::rename(&source, temp.path().join("moved")).unwrap();
            fs::rename(&replacement, &source).unwrap();
        });

        assert!(result.is_err());
    }
}

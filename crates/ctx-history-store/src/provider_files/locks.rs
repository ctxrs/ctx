fn provider_file_owner_lock_name(
    store_identity: &str,
    provider: CaptureProvider,
    material_source_format: &str,
    material_source_root: &str,
    source_path: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-provider-owner-lock-v2");
    for field in [
        store_identity,
        provider.as_str(),
        material_source_format,
        material_source_root,
        source_path,
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn opaque_provider_file_owner_id(
    provider: CaptureProvider,
    material_source_format: &str,
    material_source_root: &str,
    source_path: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-provider-owner-v1");
    for field in [
        provider.as_str(),
        material_source_format,
        material_source_root,
        source_path,
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn provider_file_staging_name(store_identity: &str, owner_id: &str, scope_id: Uuid) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-provider-staging-v2");
    for field in [store_identity, owner_id, &scope_id.to_string()] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn create_or_validate_private_lock_dir(path: &Path) -> std::io::Result<()> {
    match create_private_staging_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
                validate_existing_private_lock_dir(path, &metadata)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "provider owner lock path is not a private directory",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn validate_existing_private_lock_dir(
    _path: &Path,
    metadata: &fs::Metadata,
) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.permissions().mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() } {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "provider owner lock directory is not private to the current user",
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn validate_existing_private_lock_dir(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn validate_existing_private_lock_dir(
    path: &Path,
    _metadata: &fs::Metadata,
) -> std::io::Result<()> {
    validate_existing_private_windows_path(path, true)
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn open_private_owner_lock_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)
        .and_then(|file| {
            validate_open_private_owner_lock_file(&file, path)?;
            Ok(file)
        })
}

#[cfg(unix)]
fn validate_open_private_owner_lock_file(file: &File, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let opened = file.metadata()?;
    let linked = fs::symlink_metadata(path)?;
    if !opened.is_file()
        || !linked.file_type().is_file()
        || linked.file_type().is_symlink()
        || opened.uid() != unsafe { libc::geteuid() }
        || opened.permissions().mode() & 0o077 != 0
        || opened.nlink() != 1
        || opened.dev() != linked.dev()
        || opened.ino() != linked.ino()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe provider owner lock file",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn open_private_owner_lock_file(path: &Path) -> std::io::Result<File> {
    use std::mem;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{LocalFree, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
    };

    let descriptor = private_windows_security_descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    validate_open_private_owner_lock_file(&file, path)?;
    Ok(file)
}

#[cfg(windows)]
fn validate_open_private_owner_lock_file(_file: &File, path: &Path) -> std::io::Result<()> {
    validate_existing_private_windows_path(path, false)
}

#[cfg(not(any(unix, windows)))]
fn validate_open_private_owner_lock_file(_file: &File, _path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn open_private_owner_lock_file(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private provider owner locks are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn create_private_staging_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(windows)]
fn create_private_staging_dir(path: &Path) -> std::io::Result<()> {
    use std::{mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::{
        Foundation::LocalFree, Security::SECURITY_ATTRIBUTES, Storage::FileSystem::CreateDirectoryW,
    };

    let descriptor = private_windows_security_descriptor(true)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let created = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
    let error = (created == 0).then(std::io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    match error {
        Some(error) => Err(error),
        None => validate_existing_private_windows_path(path, true),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_private_staging_dir(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private replacement staging is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn create_private_staging_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(unix)]
fn open_existing_private_staging_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe provider staging file",
        ));
    }
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(unix)]
fn validate_existing_private_staging_file_for_removal(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.permissions().mode() & 0o077 == 0
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.nlink() == 1
    {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe provider staging file",
        ))
    }
}

#[cfg(windows)]
fn validate_existing_private_staging_file_for_removal(path: &Path) -> std::io::Result<()> {
    validate_existing_private_windows_path(path, false)
}

#[cfg(not(any(unix, windows)))]
fn validate_existing_private_staging_file_for_removal(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn open_existing_private_staging_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe provider staging file",
        ));
    }
    open_private_owner_lock_file(path)
}

#[cfg(not(any(unix, windows)))]
fn open_existing_private_staging_file(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private provider staging is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn create_private_staging_file(path: &Path) -> std::io::Result<File> {
    use std::{mem, os::windows::ffi::OsStrExt, os::windows::io::FromRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{LocalFree, INVALID_HANDLE_VALUE},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        },
    };

    let descriptor = private_windows_security_descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    let error = (handle == INVALID_HANDLE_VALUE).then(std::io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(error) = error {
        return Err(error);
    }
    let file = unsafe { File::from_raw_handle(handle) };
    validate_existing_private_windows_path(path, false)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn create_private_staging_file(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private replacement staging is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn validate_existing_private_windows_path(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
    use windows_sys::Win32::{
        Foundation::{LocalFree, ERROR_SUCCESS},
        Security::{
            AclSizeInformation,
            Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
            GetAclInformation, GetSecurityDescriptorDacl, ACL, ACL_SIZE_INFORMATION,
            DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
        Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let metadata = fs::symlink_metadata(path)?;
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe provider private path type",
        ));
    }

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut actual_dacl: *mut ACL = std::ptr::null_mut();
    let mut actual_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut actual_dacl,
            std::ptr::null_mut(),
            &mut actual_descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }

    let expected_descriptor = private_windows_security_descriptor(directory)?;
    let comparison = (|| {
        let mut expected_present = 0;
        let mut expected_defaulted = 0;
        let mut expected_dacl: *mut ACL = std::ptr::null_mut();
        let valid = unsafe {
            GetSecurityDescriptorDacl(
                expected_descriptor,
                &mut expected_present,
                &mut expected_dacl,
                &mut expected_defaulted,
            )
        };
        if valid == 0 || expected_present == 0 || actual_dacl.is_null() || expected_dacl.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "provider private path has no protected DACL",
            ));
        }

        fn acl_bytes(acl: *const ACL) -> std::io::Result<Vec<u8>> {
            let mut info = ACL_SIZE_INFORMATION::default();
            let valid = unsafe {
                GetAclInformation(
                    acl,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            };
            if valid == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(unsafe {
                std::slice::from_raw_parts(acl.cast::<u8>(), info.AclBytesInUse as usize).to_vec()
            })
        }

        if acl_bytes(actual_dacl)? != acl_bytes(expected_dacl)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "provider private path DACL is not owner/System-only",
            ));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(expected_descriptor);
        LocalFree(actual_descriptor);
    }
    comparison
}

#[cfg(windows)]
fn private_windows_security_descriptor(
    directory: bool,
) -> std::io::Result<windows_sys::Win32::Security::PSECURITY_DESCRIPTOR> {
    use std::ptr;
    use windows_sys::Win32::Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
    };

    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let sddl = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(descriptor)
}

#[cfg(all(test, unix))]
fn staging_directory_mode(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    Ok(Some(fs::metadata(path)?.permissions().mode() & 0o777))
}

#[cfg(all(test, not(unix)))]
fn staging_directory_mode(_path: &Path) -> Result<Option<u32>> {
    Ok(None)
}

#[cfg(all(test, unix))]
fn staging_file_mode(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    Ok(Some(fs::metadata(path)?.permissions().mode() & 0o777))
}

#[cfg(all(test, not(unix)))]
fn staging_file_mode(_path: &Path) -> Result<Option<u32>> {
    Ok(None)
}

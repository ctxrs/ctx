//! Owner-only filesystem primitives for local ctx state.

use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::fs;

mod path_overlap;
#[cfg(unix)]
mod unix_private_directory;
#[cfg(windows)]
mod windows_acl;

/// Rejects equal, ancestor, descendant, symlink/reparse, and native-identity
/// aliases between one provider source root and the selected ctx data root.
///
/// This inspection is read-only and does not create either path.
pub fn validate_provider_source_outside_data_root(
    data_root: &Path,
    source_root: &Path,
) -> io::Result<()> {
    path_overlap::validate_provider_source_outside_data_root(data_root, source_root)
}

/// Rewrites only platform-defined absolute namespace aliases to the native
/// path used for no-follow component traversal.
pub fn normalize_platform_namespace_alias(path: &Path) -> PathBuf {
    path_overlap::normalize_platform_namespace_alias(path)
}

/// Establishes the selected ctx data root as an owner-private directory before
/// the first persistent write.
///
/// Missing components are private at creation. An existing final directory is
/// repaired through a no-follow handle, while unsafe ownership, symlinks, and
/// Windows reparse points fail closed.
pub fn establish_private_data_root(path: &Path) -> io::Result<()> {
    let path = path_overlap::absolute_data_root_path(path)?;
    #[cfg(unix)]
    {
        unix_private_directory::establish_private_data_root(&path)
    }
    #[cfg(windows)]
    {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => windows_acl::restrict_private_directory(&path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                windows_acl::create_private_directory_all(&path)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private data-root establishment is unavailable on this platform",
        ))
    }
}

/// Creates missing pathname components with an owner-only directory policy,
/// then verifies the final directory without repairing pre-existing objects.
pub fn create_private_directory_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix_private_directory::create_private_directory_all(path)
    }
    #[cfg(windows)]
    {
        windows_acl::create_private_directory_all(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private directory creation is unavailable on this platform",
        ))
    }
}

/// Creates missing directories with an owner-only policy and repairs an
/// existing final directory when it is owned by the current user but is not
/// private yet.
///
/// Existing owner-private directories are left unchanged, including more
/// restrictive modes such as read/execute-only directories. Unsafe ownership,
/// symlinks, and Windows reparse points fail closed.
pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix_private_directory::ensure_private_directory(path)
    }
    #[cfg(windows)]
    {
        create_private_directory_all(path).or_else(|_| establish_private_data_root(path))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private directory establishment is unavailable on this platform",
        ))
    }
}

/// Applies and verifies an owner-only directory policy.
pub fn restrict_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        windows_acl::restrict_private_directory(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private directory policy is unavailable on this platform",
        ))
    }
}

/// Applies and verifies an owner-only regular-file policy.
pub fn restrict_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let file = open_regular_file_nofollow(path)?;
        restrict_private_file_handle(&file)
    }
    #[cfg(windows)]
    {
        windows_acl::restrict_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file policy is unavailable on this platform",
        ))
    }
}

/// Verifies an existing regular file and repairs its owner-only policy when
/// the current user owns it. Symlinks, reparse points, unsafe ownership, and
/// non-regular files fail closed.
pub fn ensure_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let file = open_regular_file_nofollow(path)?;
        ensure_private_file_handle(&file)
    }
    #[cfg(windows)]
    {
        windows_acl::ensure_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file establishment is unavailable on this platform",
        ))
    }
}

/// Verifies an already-open regular file and repairs its owner-only policy
/// through that same handle when the current user owns it.
///
/// Callers opening by pathname must use platform no-follow semantics. Unsafe
/// ownership and non-regular handles fail closed, as do unsuccessful repairs.
pub fn ensure_private_file_handle(handle: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        match verify_private_file_handle(handle) {
            Ok(()) => Ok(()),
            Err(_) => restrict_private_file_handle(handle),
        }
    }
    #[cfg(windows)]
    {
        windows_acl::ensure_private_file_handle(handle)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = handle;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file establishment is unavailable on this platform",
        ))
    }
}

/// Applies and verifies an owner-only regular-file policy through an already
/// open handle.
pub fn restrict_private_file_handle(handle: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = handle.metadata()?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(private_policy_error());
        }
        handle.set_permissions(fs::Permissions::from_mode(0o600))?;
        unix_private_directory::clear_extended_acl(handle)?;
        verify_private_file_handle(handle)
    }
    #[cfg(windows)]
    {
        windows_acl::restrict_private_file_handle(handle)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = handle;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file policy is unavailable on this platform",
        ))
    }
}

/// Applies and verifies owner-only executable-file permissions.
pub fn restrict_private_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        windows_acl::restrict_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private executable policy is unavailable on this platform",
        ))
    }
}

/// Verifies the exact owner-only directory policy without mutating it.
pub fn verify_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0
        {
            Ok(())
        } else {
            Err(private_policy_error())
        }
    }
    #[cfg(windows)]
    {
        windows_acl::verify_private_directory(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private directory verification is unavailable on this platform",
        ))
    }
}

/// Verifies the exact owner-only regular-file policy without mutating it.
pub fn verify_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let file = open_regular_file_nofollow(path)?;
        verify_private_file_handle(&file)
    }
    #[cfg(windows)]
    {
        windows_acl::verify_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file verification is unavailable on this platform",
        ))
    }
}

/// Verifies owner-only executable-file permissions.
pub fn verify_private_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::symlink_metadata(path)?;
        let mode = metadata.permissions().mode();
        if metadata.is_file()
            && !metadata.file_type().is_symlink()
            && mode & 0o077 == 0
            && mode & 0o100 != 0
        {
            Ok(())
        } else {
            Err(private_policy_error())
        }
    }
    #[cfg(windows)]
    {
        windows_acl::verify_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private executable verification is unavailable on this platform",
        ))
    }
}

/// Verifies an already-open Windows directory handle without reopening its
/// pathname. Callers may retain the handle to keep the verified object stable.
#[cfg(windows)]
pub fn verify_private_directory_handle(handle: &std::fs::File) -> io::Result<()> {
    windows_acl::verify_private_directory_handle(handle)
}

/// Verifies an already-open regular-file handle without reopening its
/// pathname. Executables use the same exact protected DACL on Windows.
pub fn verify_private_file_handle(handle: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = handle.metadata()?;
        if metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o177 == 0
        {
            unix_private_directory::verify_no_extended_acl(handle)
        } else {
            Err(private_policy_error())
        }
    }
    #[cfg(windows)]
    {
        windows_acl::verify_private_file_handle(handle)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = handle;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private file verification is unavailable on this platform",
        ))
    }
}

/// Opens a Windows private file while rejecting reparse points in every path
/// component, then verifies owner and DACL on the retained final handle.
#[cfg(windows)]
pub fn open_verified_private_file(path: &Path) -> io::Result<std::fs::File> {
    windows_acl::open_verified_private_file(path)
}

#[cfg(unix)]
fn private_policy_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private state path is not owner-only",
    )
}

#[cfg(unix)]
fn open_regular_file_nofollow(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let open = |read: bool, write: bool| {
        let mut options = fs::OpenOptions::new();
        options
            .read(read)
            .write(write)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
    };
    match open(true, false) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => open(false, true),
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn ensure_private_directory_repairs_permissive_existing_target() -> io::Result<()> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("state");
        fs::create_dir(&target)?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o775))?;

        ensure_private_directory(&target)?;

        assert_eq!(fs::metadata(target)?.permissions().mode() & 0o777, 0o700);
        Ok(())
    }

    #[test]
    fn ensure_private_directory_preserves_more_restrictive_existing_target() -> io::Result<()> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("state");
        fs::create_dir(&target)?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o500))?;

        ensure_private_directory(&target)?;

        assert_eq!(fs::metadata(target)?.permissions().mode() & 0o777, 0o500);
        Ok(())
    }

    #[test]
    fn ensure_private_file_handle_repairs_legacy_owner_owned_modes() -> io::Result<()> {
        let parent = tempfile::tempdir()?;
        for (mode, expected) in [
            (0o400, 0o400),
            (0o444, 0o600),
            (0o644, 0o600),
            (0o664, 0o600),
        ] {
            let target = parent.path().join(format!("private-state-{mode:o}"));
            fs::write(&target, b"state")?;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
            let file = fs::OpenOptions::new().read(true).open(&target)?;

            ensure_private_file_handle(&file)?;

            assert_eq!(file.metadata()?.permissions().mode() & 0o777, expected);
        }
        Ok(())
    }

    #[test]
    fn ensure_private_file_rejects_links_and_non_regular_files() -> io::Result<()> {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir()?;
        let target = parent.path().join("target");
        let link = parent.path().join("link");
        fs::write(&target, b"state")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644))?;
        symlink(&target, &link)?;

        assert!(ensure_private_file(&link).is_err());
        assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o777, 0o644);

        let directory = parent.path().join("directory");
        fs::create_dir(&directory)?;
        let handle = fs::File::open(directory)?;
        assert!(ensure_private_file_handle(&handle).is_err());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_file_without_extended_acl_is_valid() -> io::Result<()> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("private-state");
        fs::write(&target, b"state")?;

        restrict_private_file(&target)?;
        verify_private_file(&target)
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::{fs, process::Command};

    use super::*;

    #[test]
    fn inherited_permissive_acl_is_replaced_with_exact_private_acl(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let status = Command::new("icacls.exe")
            .arg(parent.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .status()?;
        if !status.success() {
            return Err("failed to make inherited ACL fixture permissive".into());
        }
        let directory = parent.path().join("private");
        fs::create_dir(&directory)?;
        let file = directory.join("ctx.db");
        fs::write(&file, b"private")?;

        restrict_private_directory(&directory)?;
        restrict_private_file(&file)?;
        verify_private_directory(&directory)?;
        verify_private_file(&file)?;
        Ok(())
    }

    #[test]
    fn recursive_creation_is_private_and_user_owned_under_a_permissive_parent(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let status = Command::new("icacls.exe")
            .arg(parent.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .status()?;
        if !status.success() {
            return Err("failed to make inherited ACL fixture permissive".into());
        }
        let first = parent.path().join("private");
        let nested = first.join("state");

        create_private_directory_all(&nested)?;

        verify_private_directory(&first)?;
        verify_private_directory(&nested)?;
        fs::write(nested.join("usable"), b"ok")?;
        Ok(())
    }

    #[test]
    fn created_file_is_private_and_user_owned_under_a_permissive_parent(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC,
        };

        let parent = tempfile::tempdir()?;
        let status = Command::new("icacls.exe")
            .arg(parent.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .status()?;
        if !status.success() {
            return Err("failed to make inherited ACL fixture permissive".into());
        }
        let path = parent.path().join("created-private-file");
        let mut options = fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .access_mode(FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(&path)?;

        restrict_private_file_handle(&file)?;

        verify_private_file_handle(&file)?;
        verify_private_file(&path)?;
        Ok(())
    }

    #[test]
    fn recursive_creation_rejects_insecure_existing_target_without_repair(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let status = Command::new("icacls.exe")
            .arg(parent.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .status()?;
        if !status.success() {
            return Err("failed to make inherited ACL fixture permissive".into());
        }
        let target = parent.path().join("insecure");
        fs::create_dir(&target)?;
        assert!(verify_private_directory(&target).is_err());

        assert!(create_private_directory_all(&target).is_err());
        assert!(verify_private_directory(&target).is_err());
        Ok(())
    }

    #[test]
    fn data_root_establishment_replaces_inherited_acl_before_first_write(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let status = Command::new("icacls.exe")
            .arg(parent.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .status()?;
        if !status.success() {
            return Err("failed to make inherited ACL fixture permissive".into());
        }
        let target = parent.path().join("data");
        fs::create_dir(&target)?;
        assert!(verify_private_directory(&target).is_err());

        establish_private_data_root(&target)?;

        verify_private_directory(&target)?;
        fs::write(target.join("first-write"), b"private")?;
        Ok(())
    }

    #[test]
    fn directory_reparse_points_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("target");
        let junction = parent.path().join("junction");
        fs::create_dir(&target)?;
        let status = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()?;
        if !status.success() {
            return Err("failed to create junction fixture".into());
        }
        assert!(restrict_private_directory(&junction).is_err());
        assert!(verify_private_directory(&junction).is_err());
        let nested = target.join("nested");
        fs::create_dir(&nested)?;
        assert!(restrict_private_directory(&junction.join("nested")).is_err());
        assert!(create_private_directory_all(&junction.join("created")).is_err());
        assert!(!target.join("created").exists());
        Ok(())
    }

    #[test]
    fn unrelated_local_principal_cannot_read_or_replace_private_file(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("CTX_TEST_WINDOWS_OTHER_PRINCIPAL").as_deref() != Ok("1") {
            return Ok(());
        }
        let username = format!("ctxacl{}", std::process::id());
        let password = format!("Cx!{}Aa7", std::process::id());
        let created = Command::new("net.exe")
            .args(["user", &username, &password, "/add"])
            .status()?;
        if !created.success() {
            return Err("native ACL test requires permission to create a disposable user".into());
        }
        struct UserCleanup(String);
        impl Drop for UserCleanup {
            fn drop(&mut self) {
                let _ = Command::new("net.exe")
                    .args(["user", &self.0, "/delete"])
                    .status();
            }
        }
        let _cleanup = UserCleanup(username.clone());

        let parent = tempfile::tempdir()?;
        let permissive = Command::new("icacls.exe")
            .arg(parent.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .status()?;
        if !permissive.success() {
            return Err("failed to create permissive parent ACL".into());
        }
        let directory = parent.path().join("private");
        fs::create_dir(&directory)?;
        let file = directory.join("secret.txt");
        fs::write(&file, b"must not be readable")?;
        restrict_private_directory(&directory)?;
        restrict_private_file(&file)?;

        let script = r#"
$secure = ConvertTo-SecureString $env:CTX_ACL_TEST_PASSWORD -AsPlainText -Force
$credential = New-Object System.Management.Automation.PSCredential(".\$env:CTX_ACL_TEST_USER", $secure)
$argument = "/D /C type `"$env:CTX_ACL_TEST_PATH`""
$process = Start-Process -FilePath cmd.exe -ArgumentList $argument -Credential $credential -Wait -PassThru -WindowStyle Hidden
exit $process.ExitCode
"#;
        let status = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("CTX_ACL_TEST_USER", &username)
            .env("CTX_ACL_TEST_PASSWORD", &password)
            .env("CTX_ACL_TEST_PATH", &file)
            .status()?;
        assert!(
            !status.success(),
            "unrelated user unexpectedly read private state"
        );

        let moved = directory.join("stolen.txt");
        let script = r#"
$secure = ConvertTo-SecureString $env:CTX_ACL_TEST_PASSWORD -AsPlainText -Force
$credential = New-Object System.Management.Automation.PSCredential(".\$env:CTX_ACL_TEST_USER", $secure)
$argument = "/D /C move /Y `"$env:CTX_ACL_TEST_PATH`" `"$env:CTX_ACL_TEST_MOVED`" && (echo attacker>`"$env:CTX_ACL_TEST_PATH`")"
$process = Start-Process -FilePath cmd.exe -ArgumentList $argument -Credential $credential -Wait -PassThru -WindowStyle Hidden
exit $process.ExitCode
"#;
        let status = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("CTX_ACL_TEST_USER", &username)
            .env("CTX_ACL_TEST_PASSWORD", &password)
            .env("CTX_ACL_TEST_PATH", &file)
            .env("CTX_ACL_TEST_MOVED", &moved)
            .status()?;
        assert!(
            !status.success(),
            "unrelated user unexpectedly replaced private state"
        );
        assert_eq!(fs::read(&file)?, b"must not be readable");
        assert!(!moved.exists());
        Ok(())
    }
}

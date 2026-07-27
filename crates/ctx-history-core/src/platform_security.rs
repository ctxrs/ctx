//! Owner-only filesystem primitives for local ctx state.

use std::{io, path::Path};

#[cfg(unix)]
use std::fs;

#[cfg(windows)]
mod windows_acl;

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
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
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
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
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
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.permissions().mode() & 0o177 == 0
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

/// Verifies an already-open Windows regular-file handle without reopening its
/// pathname. Executables use the same exact protected DACL on Windows.
#[cfg(windows)]
pub fn verify_private_file_handle(handle: &std::fs::File) -> io::Result<()> {
    windows_acl::verify_private_file_handle(handle)
}

#[cfg(unix)]
fn private_policy_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private state path is not owner-only",
    )
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

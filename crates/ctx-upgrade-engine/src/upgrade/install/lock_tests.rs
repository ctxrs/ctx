use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use tempfile::tempdir;

use super::lock::{canonical_executable, installation_lock_path, InstallationLock};
#[cfg(windows)]
use super::lock::{canonical_recovery_executable, OwnerFileLock};

const CHILD_TARGET_ENV: &str = "CTX_INSTALL_LOCK_CHILD_TARGET";

fn executable_copy(root: &Path) -> Result<PathBuf> {
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let executable = bin.join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    fs::write(&executable, b"temporary ctx executable copy")?;
    Ok(executable)
}

#[test]
fn different_data_roots_contend_on_the_same_executable_lock() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path())?;
    let data_root_a = fixture.path().join("data-a");
    let data_root_b = fixture.path().join("data-b");
    fs::create_dir(&data_root_a)?;
    fs::create_dir(&data_root_b)?;

    let first = InstallationLock::try_acquire(&executable)?.expect("first lock");
    assert!(
        InstallationLock::try_acquire(&executable)?.is_none(),
        "a second scheduler rooted at {} must contend with the scheduler rooted at {}",
        data_root_b.display(),
        data_root_a.display()
    );
    drop(first);
    assert!(InstallationLock::try_acquire(&executable)?.is_some());
    Ok(())
}

#[test]
fn fresh_root_and_installed_executable_share_the_persistent_lock() -> Result<()> {
    let fixture = tempdir()?;
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin)?;

    let root_lock =
        InstallationLock::try_acquire_at_root(fixture.path())?.expect("fresh root lock");
    let lock_path = bin.join(if cfg!(windows) {
        ".ctx.exe.install.lock"
    } else {
        ".ctx.install.lock"
    });
    assert!(
        lock_path.is_file(),
        "fresh acquisition must persist the lock"
    );

    let executable = bin.join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    fs::write(&executable, b"newly installed ctx")?;
    assert_eq!(installation_lock_path(&executable)?, lock_path);
    assert!(
        InstallationLock::try_acquire(&executable)?.is_none(),
        "the installed Core must contend with the fresh bootstrap owner"
    );
    drop(root_lock);

    let executable_lock = InstallationLock::try_acquire(&executable)?.expect("executable lock");
    assert!(InstallationLock::try_acquire_at_root(fixture.path())?.is_none());
    drop(executable_lock);
    assert!(lock_path.is_file(), "the canonical lock is never unlinked");
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_candidate_contends_with_base_version_lock() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path())?;
    let base_lock_path = fixture.path().join("bin").join(".ctx.exe.install.lock");
    let base_lock = OwnerFileLock::try_acquire(&base_lock_path)?.expect("base version lock");

    assert_eq!(
        installation_lock_path(&canonical_executable(&executable)?)?,
        base_lock_path
    );
    assert!(
        InstallationLock::try_acquire(&executable)?.is_none(),
        "the candidate must contend with the base version Windows lock"
    );

    drop(base_lock);
    assert!(InstallationLock::try_acquire(&executable)?.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn fresh_root_lock_rejects_aliased_or_non_owner_safe_bin() -> Result<()> {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    let fixture = tempdir()?;
    let real = fixture.path().join("real");
    fs::create_dir(&real)?;
    fs::create_dir(real.join("bin"))?;
    let alias = fixture.path().join("alias");
    symlink(&real, &alias)?;
    assert!(InstallationLock::try_acquire_at_root(&alias).is_err());

    fs::set_permissions(&real, fs::Permissions::from_mode(0o777))?;
    assert!(
        InstallationLock::try_acquire_at_root(&real)?.is_some(),
        "the lock does not impose private-data-root permissions on the parent"
    );
    fs::set_permissions(real.join("bin"), fs::Permissions::from_mode(0o777))?;
    assert!(InstallationLock::try_acquire_at_root(&real).is_err());
    Ok(())
}

#[test]
fn lock_ownership_is_live_and_never_recovered_from_pid_text() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path())?;
    let canonical = canonical_executable(&executable)?;
    let path = installation_lock_path(&canonical)?;
    fs::write(&path, b"1 0 stale-looking-but-irrelevant\n")?;

    {
        let first = InstallationLock::try_acquire(&executable)?.expect("first lock");
        let before = fs::metadata(&path)?;
        assert!(InstallationLock::try_acquire(&executable)?.is_none());
        drop(first);
        let after = fs::metadata(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        }
    }

    assert!(path.exists(), "the stable lock inode must not be unlinked");
    assert!(InstallationLock::try_acquire(&executable)?.is_some());
    Ok(())
}

#[test]
fn cross_process_installation_lock_is_owner_safe() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path())?;
    let first = InstallationLock::try_acquire(&executable)?.expect("parent lock");
    let status = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "upgrade::install::lock_tests::installation_lock_child_probe",
        ])
        .env(CHILD_TARGET_ENV, &executable)
        .status()?;
    drop(first);
    assert!(
        status.success(),
        "child acquired a lock owned by its parent"
    );
    Ok(())
}

#[test]
fn installation_lock_child_probe() -> Result<()> {
    let Some(executable) = std::env::var_os(CHILD_TARGET_ENV) else {
        return Ok(());
    };
    assert!(InstallationLock::try_acquire(Path::new(&executable))?.is_none());
    Ok(())
}

#[cfg(windows)]
fn ordinary_windows_disk_path(path: &Path) -> Result<PathBuf> {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt as _, OsStringExt as _},
        path::{Component, Prefix},
    };

    let Component::Prefix(prefix) = path
        .components()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Windows test path has no prefix: {}", path.display()))?
    else {
        anyhow::bail!("Windows test path is not a disk path: {}", path.display());
    };
    let wide: Vec<_> = path.as_os_str().encode_wide().collect();
    let ordinary = match prefix.kind() {
        Prefix::Disk(_) => wide.as_slice(),
        Prefix::VerbatimDisk(_) => wide.get(4..).ok_or_else(|| {
            anyhow::anyhow!(
                "Windows verbatim test path is truncated: {}",
                path.display()
            )
        })?,
        _ => anyhow::bail!("Windows test path is not a disk path: {}", path.display()),
    };
    Ok(PathBuf::from(OsString::from_wide(ordinary)))
}

#[cfg(windows)]
#[test]
fn windows_recovery_uses_one_identity_for_ordinary_missing_executable() -> Result<()> {
    let fixture = tempdir()?;
    let parent = fixture.path().join("bin");
    fs::create_dir(&parent)?;
    let canonical_parent = fs::canonicalize(&parent)?;
    let ordinary_parent = ordinary_windows_disk_path(&canonical_parent)?;
    let ordinary_executable = ordinary_parent.join("ctx-missing.exe");
    let canonical_executable = canonical_parent.join("ctx-missing.exe");
    assert!(!ordinary_executable.try_exists()?);

    assert_eq!(
        canonical_recovery_executable(&ordinary_executable)?,
        canonical_executable
    );
    assert_eq!(
        canonical_recovery_executable(&canonical_executable)?,
        canonical_executable
    );

    let first = InstallationLock::try_acquire_for_recovery(&ordinary_executable)?
        .expect("ordinary recovery lock");
    assert!(
        InstallationLock::try_acquire_for_recovery(&canonical_executable)?.is_none(),
        "ordinary and verbatim recovery paths must contend on one lock"
    );
    drop(first);
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_recovery_rejects_junction_parent_alias() -> Result<()> {
    let fixture = tempdir()?;
    let target = fixture.path().join("target");
    let junction = fixture.path().join("junction");
    fs::create_dir(&target)?;
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()?;
    assert!(
        output.status.success(),
        "failed to create junction fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(canonical_recovery_executable(&junction.join("ctx.exe")).is_err());
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_recovery_rejects_parent_casing_alias() -> Result<()> {
    let fixture = tempdir()?;
    let parent = fixture.path().join("MixedCase");
    fs::create_dir(&parent)?;
    let wrong_case = fixture.path().join("mixedcase").join("ctx.exe");

    assert!(canonical_recovery_executable(&wrong_case).is_err());
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_recovery_rejects_unc_and_device_namespaces() {
    for unsupported in [
        r"\\server\share\ctx.exe",
        r"\\?\UNC\server\share\ctx.exe",
        r"\\.\C:\ctx.exe",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\ctx.exe",
    ] {
        assert!(
            canonical_recovery_executable(Path::new(unsupported)).is_err(),
            "accepted unsupported Windows recovery path {unsupported}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_recovery_rejects_unsafe_missing_executable_leafs() -> Result<()> {
    let fixture = tempdir()?;
    let parent = fixture.path().join("bin");
    fs::create_dir(&parent)?;
    let ordinary_parent = ordinary_windows_disk_path(&fs::canonicalize(&parent)?)?;

    for leaf in [
        "ctx.",
        "ctx ",
        "ctx:stream",
        "CON",
        "con.exe",
        "PRN.txt",
        "NUL.exe",
        "COM1.exe",
        "lpt9.log",
        "CON .exe",
        "COM¹.txt",
    ] {
        assert!(
            canonical_recovery_executable(&ordinary_parent.join(leaf)).is_err(),
            "accepted unsafe Windows recovery leaf {leaf:?}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_executable_aliases_share_one_lock() -> Result<()> {
    use std::os::unix::fs::symlink;

    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path())?;
    let alias = fixture.path().join("ctx-alias");
    symlink(&executable, &alias)?;

    let first = InstallationLock::try_acquire(&executable)?.expect("canonical lock");
    assert!(InstallationLock::try_acquire(&alias)?.is_none());
    drop(first);
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_lock_file_is_rejected_fail_closed() -> Result<()> {
    use std::os::unix::fs::symlink;

    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path())?;
    let canonical = canonical_executable(&executable)?;
    let path = installation_lock_path(&canonical)?;
    let other = fixture.path().join("other-lock");
    fs::write(&other, b"")?;
    symlink(&other, &path)?;

    assert!(InstallationLock::try_acquire(&executable).is_err());
    Ok(())
}

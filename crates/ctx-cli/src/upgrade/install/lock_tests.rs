use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use tempfile::tempdir;

use super::lock::{canonical_executable, installation_lock_path, InstallationLock};

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

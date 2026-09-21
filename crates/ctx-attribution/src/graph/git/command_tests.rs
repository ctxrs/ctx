use super::*;

#[test]
fn process_spawn_failure_is_runtime_git_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let mut command = Command::new(directory.path().join("missing-git"));
    assert!(matches!(
        spawn_git(&mut command),
        Err(QueryError::GitUnavailable)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn changed_executable_is_runtime_git_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("git");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    let executable = GitExecutable::authorize(&std::fs::canonicalize(&path)?)?;
    std::fs::write(&path, b"#!/bin/sh\nexit 1\n")?;

    assert!(matches!(
        run_git(&executable, std::iter::empty::<&str>(), 1024),
        Err(QueryError::GitUnavailable)
    ));
    Ok(())
}

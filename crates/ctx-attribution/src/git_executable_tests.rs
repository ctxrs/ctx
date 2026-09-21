#[cfg(unix)]
use super::*;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};

#[cfg(unix)]
fn executable_fixture(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, b"#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn missing_symlink_and_relative_executables_are_rejected() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("git");
    let alias = directory.path().join("git-alias");
    executable_fixture(&executable)?;
    symlink(&executable, &alias)?;
    assert!(GitExecutable::authorize(Path::new("git")).is_err());
    assert!(GitExecutable::authorize(&directory.path().join("missing")).is_err());
    assert!(GitExecutable::authorize(&alias).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn replacement_and_in_place_mutation_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let canonical_root = fs::canonicalize(directory.path())?;
    let executable = canonical_root.join("git");
    let displaced = canonical_root.join("git-old");
    executable_fixture(&executable)?;
    let binding = GitExecutable::authorize(&executable)?;
    fs::rename(&executable, &displaced)?;
    executable_fixture(&executable)?;
    assert!(binding.verify().is_err());

    let binding = GitExecutable::authorize(&executable)?;
    fs::write(&executable, b"#!/bin/sh\nexit 1\n")?;
    assert!(binding.verify().is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn parent_swap_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let canonical_root = fs::canonicalize(directory.path())?;
    let bin = canonical_root.join("bin");
    let displaced = canonical_root.join("bin-old");
    fs::create_dir(&bin)?;
    executable_fixture(&bin.join("git"))?;
    let binding = GitExecutable::authorize(&bin.join("git"))?;
    fs::rename(&bin, &displaced)?;
    fs::create_dir(&bin)?;
    executable_fixture(&bin.join("git"))?;
    assert!(binding.verify().is_err());
    Ok(())
}

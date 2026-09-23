use super::*;
use std::{fs, io::Write};

#[test]
fn publishes_new_file_without_clobbering_existing_file() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("record.json");
    let mut temp = NamedTempFile::new_in(directory.path())?;
    protect(temp.path())?;
    temp.write_all(b"secret")?;
    replace(temp, &path, false)?;
    let other = NamedTempFile::new_in(directory.path())?;
    protect(other.path())?;
    assert!(replace(other, &path, false).is_err());
    assert_eq!(fs::read(&path)?, b"secret");
    Ok(())
}

#[test]
fn replaces_existing_file() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.json");
    fs::write(&path, b"before")?;
    protect(&path)?;
    let mut temp = NamedTempFile::new_in(directory.path())?;
    protect(temp.path())?;
    temp.write_all(b"after")?;
    replace(temp, &path, true)?;
    assert_eq!(fs::read(&path)?, b"after");
    Ok(())
}

#[cfg(unix)]
#[test]
fn removes_group_and_other_permissions() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir()?;
    let file = NamedTempFile::new_in(directory.path())?;
    for (path, mode) in [(file.path(), 0o600), (directory.path(), 0o700)] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o777))?;
        protect(path)?;
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, mode);
    }
    Ok(())
}

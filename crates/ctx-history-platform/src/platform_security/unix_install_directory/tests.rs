use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt as _},
};

#[test]
fn public_directory_access_is_preserved_but_other_writers_and_aliases_are_rejected(
) -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let bin = root.path().join("bin");
    fs::create_dir(&bin)?;
    fs::write(bin.join("unrelated-tool"), b"keep")?;
    for mode in [0o755, 0o775] {
        fs::set_permissions(&bin, fs::Permissions::from_mode(mode))?;
        verify_install_directory(&bin)?;
        verify_install_directory_handle(&File::open(&bin)?)?;
        assert_eq!(fs::metadata(&bin)?.mode() & 0o777, mode);
        assert_eq!(fs::read(bin.join("unrelated-tool"))?, b"keep");
    }

    let alias = root.path().join("alias");
    symlink(&bin, &alias)?;
    assert!(verify_install_directory(&alias).is_err());
    assert!(verify_install_directory(&bin.join("unrelated-tool")).is_err());
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o777))?;
    assert!(verify_install_directory(&bin).is_err());
    assert_eq!(fs::metadata(&bin)?.mode() & 0o777, 0o777);
    Ok(())
}

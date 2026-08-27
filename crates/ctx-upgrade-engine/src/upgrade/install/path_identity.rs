use std::path::Path;

/// Compares a certified managed-install target with a persisted path claim.
///
/// Windows filesystem certification returns verbatim local-disk paths even
/// when the caller supplied an ordinary disk path. These two spellings share
/// one identity. No other normalization is permitted: case, separators,
/// short names, aliases, traversal, UNC paths, and device namespaces remain
/// distinct or unsupported. Other platforms require exact path equality.
pub fn managed_install_path_identity_matches(certified_target: &Path, claimed_path: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_disk_path_identity(certified_target)
            .zip(windows_disk_path_identity(claimed_path))
            .is_some_and(|(target, claim)| target == claim)
    }
    #[cfg(not(windows))]
    {
        certified_target == claimed_path
    }
}

/// Returns an exact local disk-path identity with only the Windows verbatim
/// namespace removed.
#[cfg(windows)]
pub(super) fn windows_disk_path_identity(path: &Path) -> Option<Vec<u16>> {
    use std::{
        os::windows::ffi::OsStrExt as _,
        path::{Component, Prefix},
    };

    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };
    let verbatim = match prefix.kind() {
        Prefix::Disk(_) => false,
        Prefix::VerbatimDisk(_) => true,
        Prefix::UNC(_, _)
        | Prefix::VerbatimUNC(_, _)
        | Prefix::DeviceNS(_)
        | Prefix::Verbatim(_) => return None,
    };
    let identity: Vec<_> = path.as_os_str().encode_wide().collect();
    if !verbatim {
        return Some(identity);
    }
    const VERBATIM_NAMESPACE: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    identity
        .strip_prefix(VERBATIM_NAMESPACE)
        .map(<[u16]>::to_vec)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_ordinary_and_verbatim_local_disk_paths_share_one_identity() {
        let ordinary = Path::new(r"C:\Users\ctx\bin\ctx.exe");
        let verbatim = Path::new(r"\\?\C:\Users\ctx\bin\ctx.exe");

        assert!(managed_install_path_identity_matches(verbatim, ordinary));
        assert!(managed_install_path_identity_matches(ordinary, verbatim));
    }

    #[test]
    fn windows_unsafe_nonlocal_and_aliased_path_spellings_do_not_match() {
        let certified = Path::new(r"\\?\C:\Users\ctx\bin\ctx.exe");
        for rejected in [
            r"C:\Users\ctx\bin\other.exe",
            r"C:\Users\CTX\bin\ctx.exe",
            r"C:/Users/ctx/bin/ctx.exe",
            r"C:\Users\ctx\other\..\bin\ctx.exe",
            r"\\server\share\ctx.exe",
            r"\\?\UNC\server\share\ctx.exe",
            r"\\.\C:\Users\ctx\bin\ctx.exe",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\ctx.exe",
        ] {
            assert!(
                !managed_install_path_identity_matches(certified, Path::new(rejected)),
                "accepted unsupported managed-install path {rejected}"
            );
        }
    }
}

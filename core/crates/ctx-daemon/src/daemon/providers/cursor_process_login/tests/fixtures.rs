use std::path::Path as StdPath;

#[cfg(unix)]
pub(super) fn unix_mode(path: &StdPath) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

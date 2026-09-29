//! Bind SQLite's native no-write reader to retained provider file authority.
use super::*;

pub(super) fn available(family: &SqliteSourceFamily) -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        if family.wal.is_none() && family.shared_memory.is_none() {
            #[cfg(target_os = "linux")]
            return immutable_procfd_available(family.database.file());
            #[cfg(not(target_os = "linux"))]
            return true;
        }
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            // Stock unix SQLite may fchown an existing SHM descriptor as root.
            return false;
        }
        family.wal.is_some() && family.shared_memory.is_some()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = family;
        false
    }
}

#[cfg(target_os = "linux")]
pub(super) fn open(
    family: &SqliteSourceFamily,
    _evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<(Connection, Vec<File>)> {
    if family.wal.is_none() && family.shared_memory.is_none() {
        return Ok((open_immutable_main(&family.database)?, Vec::new()));
    }
    let (connection, authority) = open_pinned_read_only_wal(family)?;
    Ok((connection, vec![authority]))
}

#[cfg(target_os = "macos")]
pub(super) fn open(
    family: &SqliteSourceFamily,
    _evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<(Connection, Vec<File>)> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
    }
    let authority = family.retain_parent_handle()?;
    let previous_directory = File::open(".").map_err(|source| SqliteSourceAccessError::Io {
        operation: "retaining the caller directory before a selected SQLite open",
        path: PathBuf::from("."),
        source,
    })?;
    // macOS supplies a thread-local cwd. No other thread's relative opens are
    // redirected while SQLite resolves the retained directory and its leaves.
    if unsafe { pthread_fchdir_np(authority.as_raw_fd()) } != 0 {
        return Err(SqliteSourceAccessError::Io {
            operation: "binding the SQLite reader to its retained parent",
            path: family.approved_parent_path().to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let leaf = url::form_urlencoded::byte_serialize(family.database_name().as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    let mode = if family.wal.is_none() && family.shared_memory.is_none() {
        "immutable=1"
    } else {
        "readonly_shm=1"
    };
    let opened =
        Connection::open_with_flags(format!("file:{leaf}?mode=ro&{mode}&vfs=unix"), read_flags());
    if unsafe { pthread_fchdir_np(previous_directory.as_raw_fd()) } != 0 {
        drop(opened);
        return Err(SqliteSourceAccessError::Io {
            operation: "restoring the SQLite reader thread directory",
            path: family.approved_parent_path().to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let connection =
        opened.map_err(|source| sqlite_error("opening the selected provider read", source))?;
    Ok((connection, vec![authority]))
}

#[cfg(windows)]
pub(super) fn open(
    family: &SqliteSourceFamily,
    _evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<(Connection, Vec<File>)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut handles = Vec::new();
    // Deny rename/delete while SQLite opens by path. Writers retain read/write
    // sharing. Pin ancestors from the drive down so none can redirect a later
    // open, and reject reparse points through the ordinary native validation.
    let ancestors = family
        .approved_parent_path()
        .ancestors()
        .collect::<Vec<_>>();
    for path in ancestors.into_iter().rev() {
        let handle = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "pinning the selected SQLite source directory",
                path: path.to_path_buf(),
                source,
            })?;
        NativeFileState::read(&handle, path, ExpectedObjectKind::Directory)?;
        handles.push(handle);
    }
    for member in std::iter::once(&family.database)
        .chain(family.wal.iter())
        .chain(family.shared_memory.iter())
    {
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&member.path)
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "pinning a selected SQLite source member",
                path: member.path.clone(),
                source,
            })?;
        let actual = NativeFileState::read(&handle, &member.path, ExpectedObjectKind::RegularFile)?;
        if actual.identity != member.capture_state()?.identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        handles.push(handle);
    }
    family.revalidate_database_identity(_evidence)?;
    let mut uri = url::Url::from_file_path(&family.database.path).map_err(|()| {
        SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the selected SQLite source cannot be represented as a file URI".into(),
        }
    })?;
    uri.query_pairs_mut().append_pair("mode", "ro");
    if family.wal.is_none() && family.shared_memory.is_none() {
        // Sidecar-free capture is checked against its exact revision after the
        // copy. Immutable mode never discovers a newly created outside WAL.
        uri.query_pairs_mut().append_pair("immutable", "1");
    } else {
        // Existing sidecars are pinned above. A missing SHM cannot be created
        // by readonly_shm, so this path remains no-write on failure as well.
        uri.query_pairs_mut().append_pair("readonly_shm", "1");
    }
    let connection = Connection::open_with_flags(uri.as_str(), read_flags())
        .map_err(|source| sqlite_error("opening the selected provider read", source))?;
    Ok((connection, handles))
}

#[cfg(any(target_os = "macos", windows))]
fn read_flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
}

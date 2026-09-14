use std::{io, path::Path};

#[cfg(windows)]
use std::fs::OpenOptions;

use crate::publication_probe::AtomicReplacementFailureProbe;

pub(super) fn io_result<T>(result: &io::Result<T>) -> Option<Result<(), i32>> {
    match result {
        Ok(_) => Some(Ok(())),
        Err(error) => error.raw_os_error().map(Err),
    }
}

pub(super) fn capture(
    source: &Path,
    target: &Path,
    error: &io::Error,
) -> AtomicReplacementFailureProbe {
    let probe = AtomicReplacementFailureProbe {
        move_error: error.raw_os_error(),
        source_readonly: None,
        source_delete_open: None,
        parent_delete_child_open: None,
        target_delete_open: None,
        source_cleanup: None,
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

        const DELETE: u32 = 0x0001_0000;
        const FILE_DELETE_CHILD: u32 = 0x0000_0040;
        const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

        let mut probe = probe;
        let open = |path: &Path, access, flags| match OpenOptions::new()
            .access_mode(access)
            .share_mode(SHARE_ALL)
            .custom_flags(flags)
            .open(path)
        {
            Ok(file) => {
                drop(file);
                Some(Ok(()))
            }
            Err(error) => error.raw_os_error().map(Err),
        };
        probe.source_readonly = source
            .symlink_metadata()
            .ok()
            .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_READONLY != 0);
        probe.source_delete_open = open(source, DELETE, FILE_FLAG_OPEN_REPARSE_POINT);
        probe.parent_delete_child_open = source.parent().and_then(|parent| {
            open(
                parent,
                FILE_DELETE_CHILD,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            )
        });
        probe.target_delete_open = open(target, DELETE, FILE_FLAG_OPEN_REPARSE_POINT);
        probe
    }
    #[cfg(not(windows))]
    {
        let _ = (source, target);
        probe
    }
}

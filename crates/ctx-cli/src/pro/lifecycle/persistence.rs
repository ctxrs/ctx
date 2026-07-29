use super::*;

pub(super) fn prepare_install_directory(
    target: &Path,
    persistence: &mut Persistence,
) -> Result<()> {
    let parent = layout_for_target(target)?.bin_dir();
    match fs::create_dir(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("invalid_request: create Pro install directory"),
    }
    persistence.boundary("create_install_directory")?;
    protect_install_directory_tree(target)?;
    persistence.boundary("chmod_install_directory")?;
    sync_install_directory(target, persistence, "fsync_install_directory")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn durable_write(
    path: &Path,
    contents: &[u8],
    unix_mode: u32,
    persistence: &mut Persistence,
    write_boundary: &'static str,
    chmod_boundary: &'static str,
    fsync_boundary: &'static str,
) -> Result<()> {
    remove_file_if_present(path, persistence, "remove_stale_staging_file")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(unix_mode);
    }
    let mut file = options
        .open(path)
        .context("invalid_request: create staged Pro install file")?;
    if unix_mode & 0o100 != 0 {
        restrict_private_executable(path)
            .context("invalid_request: protect staged Pro install executable")?;
    } else {
        restrict_private_file(path).context("invalid_request: protect staged Pro install file")?;
    }
    file.write_all(contents)
        .context("invalid_request: write staged Pro install file")?;
    persistence.boundary(write_boundary)?;
    persistence.boundary(chmod_boundary)?;
    file.sync_all()
        .context("invalid_request: sync staged Pro install file")?;
    persistence.boundary(fsync_boundary)?;
    let _ = unix_mode;
    Ok(())
}

pub(super) fn write_journal(
    target: &Path,
    transaction: &InstallTransaction,
    persistence: &mut Persistence,
) -> Result<()> {
    let bytes = serde_json::to_vec(transaction)
        .context("invalid_response: encode Pro transaction journal")?;
    if bytes.len() as u64 > MAX_TRANSACTION_JOURNAL_BYTES {
        bail!("invalid_response: Pro transaction journal exceeds maximum size");
    }
    let next = transaction_journal_next_path(target)?;
    durable_write(
        &next,
        &bytes,
        0o600,
        persistence,
        "write_transaction_journal",
        "chmod_transaction_journal",
        "fsync_transaction_journal",
    )?;
    replace_file(&next, &transaction_journal_path(target)?)
        .context("invalid_request: publish Pro transaction journal")?;
    persistence.boundary("rename_transaction_journal")?;
    sync_install_directory(target, persistence, "fsync_transaction_journal_directory")
}

fn protect_install_directory_tree(target: &Path) -> Result<()> {
    let layout = layout_for_target(target)?;
    let pro = layout.pro_root();
    let bin = layout.bin_dir();
    for (directory, label) in [
        (layout.data_root(), "ctx data root"),
        (pro.as_path(), "Pro lifecycle root"),
        (bin.as_path(), "Pro install root"),
    ] {
        validate_private_directory(directory, label)?;
        restrict_private_directory(directory)
            .context("invalid_request: protect Pro install directory")?;
        verify_private_directory(directory)
            .context("invalid_request: verify Pro install directory")?;
    }
    Ok(())
}

pub(super) fn protect_existing_installation(target: &Path) -> Result<()> {
    protect_install_directory_tree(target)?;
    let executables = [
        target.to_path_buf(),
        previous_helper_path(target)?,
        transaction_helper_path(target)?,
        publish_helper_path(target)?,
        rollback_helper_stage_path(target)?,
    ];
    for path in executables {
        protect_existing_install_file(&path, true)?;
    }
    let files = [
        install_marker_path(target)?,
        previous_marker_path(target)?,
        transaction_journal_path(target)?,
        transaction_journal_next_path(target)?,
        transaction_marker_path(target)?,
        publish_marker_path(target)?,
        rollback_marker_stage_path(target)?,
    ];
    for path in files {
        protect_existing_install_file(&path, false)?;
    }
    Ok(())
}

fn protect_existing_install_file(path: &Path, executable: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if executable {
                restrict_private_executable(path)
                    .context("invalid_request: protect Pro installation executable")
            } else {
                restrict_private_file(path)
                    .context("invalid_request: protect Pro installation file")
            }
        }
        Ok(_) => bail!("invalid_response: Pro installation path has an unsafe file type"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("invalid_request: inspect Pro installation file"),
    }
}

pub(super) fn publish_current(
    target: &Path,
    pair: &ValidatedPair,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<()> {
    if load_pair_at(target, &install_marker_path(target)?, public_key_pem)
        .ok()
        .flatten()
        .is_some_and(|current| current.identity == pair.identity)
    {
        return Ok(());
    }
    publish_pair(
        target,
        &install_marker_path(target)?,
        &publish_helper_path(target)?,
        &publish_marker_path(target)?,
        pair,
        persistence,
        "write_current_helper_stage",
        "chmod_current_helper_stage",
        "fsync_current_helper_stage",
        "write_current_marker_stage",
        "chmod_current_marker_stage",
        "fsync_current_marker_stage",
        "rename_current_helper",
        "fsync_current_helper_directory",
        "rename_current_marker",
        "fsync_current_marker_directory",
    )
}

pub(super) fn publish_previous(
    target: &Path,
    pair: &ValidatedPair,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<()> {
    let helper = previous_helper_path(target)?;
    let marker = previous_marker_path(target)?;
    if load_pair_at(&helper, &marker, public_key_pem)
        .ok()
        .flatten()
        .is_some_and(|previous| previous.identity == pair.identity)
    {
        return Ok(());
    }
    publish_pair(
        &helper,
        &marker,
        &rollback_helper_stage_path(target)?,
        &rollback_marker_stage_path(target)?,
        pair,
        persistence,
        "write_rollback_helper_stage",
        "chmod_rollback_helper_stage",
        "fsync_rollback_helper_stage",
        "write_rollback_marker_stage",
        "chmod_rollback_marker_stage",
        "fsync_rollback_marker_stage",
        "rename_rollback_helper",
        "fsync_rollback_helper_directory",
        "rename_rollback_marker",
        "fsync_rollback_marker_directory",
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_pair(
    helper_target: &Path,
    marker_target: &Path,
    helper_stage: &Path,
    marker_stage: &Path,
    pair: &ValidatedPair,
    persistence: &mut Persistence,
    helper_write: &'static str,
    helper_chmod: &'static str,
    helper_fsync: &'static str,
    marker_write: &'static str,
    marker_chmod: &'static str,
    marker_fsync: &'static str,
    helper_rename: &'static str,
    helper_directory_fsync: &'static str,
    marker_rename: &'static str,
    marker_directory_fsync: &'static str,
) -> Result<()> {
    durable_write(
        helper_stage,
        &pair.artifact,
        0o700,
        persistence,
        helper_write,
        helper_chmod,
        helper_fsync,
    )?;
    durable_write(
        marker_stage,
        &pair.marker,
        0o600,
        persistence,
        marker_write,
        marker_chmod,
        marker_fsync,
    )?;
    sync_path_directory(
        helper_target,
        persistence,
        "fsync_publish_staging_directory",
    )?;
    replace_file(helper_stage, helper_target)
        .context("invalid_request: publish Pro helper file")?;
    persistence.boundary(helper_rename)?;
    sync_path_directory(helper_target, persistence, helper_directory_fsync)?;
    replace_file(marker_stage, marker_target)
        .context("invalid_request: publish Pro marker file")?;
    persistence.boundary(marker_rename)?;
    sync_path_directory(marker_target, persistence, marker_directory_fsync)
}

pub(super) fn remove_current_pair(target: &Path, persistence: &mut Persistence) -> Result<()> {
    remove_file_if_present(target, persistence, "remove_incomplete_current_helper")?;
    remove_file_if_present(
        &install_marker_path(target)?,
        persistence,
        "remove_incomplete_current_marker",
    )?;
    sync_install_directory(target, persistence, "fsync_removed_current_pair_directory")
}

pub(super) fn cleanup_transaction_files(
    target: &Path,
    persistence: &mut Persistence,
) -> Result<()> {
    for (path, boundary) in [
        (
            transaction_journal_next_path(target)?,
            "remove_transaction_journal_next",
        ),
        (
            transaction_helper_path(target)?,
            "remove_transaction_helper",
        ),
        (
            transaction_marker_path(target)?,
            "remove_transaction_marker",
        ),
        (publish_helper_path(target)?, "remove_publish_helper"),
        (publish_marker_path(target)?, "remove_publish_marker"),
        (
            rollback_helper_stage_path(target)?,
            "remove_rollback_helper_stage",
        ),
        (
            rollback_marker_stage_path(target)?,
            "remove_rollback_marker_stage",
        ),
    ] {
        remove_file_if_present(&path, persistence, boundary)?;
    }
    sync_install_directory(target, persistence, "fsync_transaction_cleanup_directory")?;
    remove_file_if_present(
        &transaction_journal_path(target)?,
        persistence,
        "remove_transaction_journal",
    )?;
    sync_install_directory(target, persistence, "fsync_journal_removal_directory")
}

fn remove_file_if_present(
    path: &Path,
    persistence: &mut Persistence,
    boundary: &'static str,
) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => persistence.boundary(boundary),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => bail!("invalid_request: remove Pro transaction file"),
    }
}

pub(super) fn sync_install_directory(
    target: &Path,
    persistence: &mut Persistence,
    boundary: &'static str,
) -> Result<()> {
    sync_path_directory(target, persistence, boundary)
}

fn sync_path_directory(
    path: &Path,
    persistence: &mut Persistence,
    boundary: &'static str,
) -> Result<()> {
    sync_parent_directory(path)?;
    persistence.boundary(boundary)
}

pub(in crate::pro) fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid_request: Pro install path has no parent"))?;
    #[cfg(not(windows))]
    let directory =
        fs::File::open(parent).context("invalid_request: open Pro install directory")?;
    #[cfg(windows)]
    let directory = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)
            .context("invalid_request: open Pro install directory")?
    };
    directory
        .sync_all()
        .context("invalid_request: sync Pro install directory")?;
    Ok(())
}

#[cfg(not(windows))]
pub(in crate::pro) fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
pub(in crate::pro) fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both path buffers are NUL-terminated and remain alive for the call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

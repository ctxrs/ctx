#![cfg_attr(all(test, not(windows)), allow(dead_code))]

#[cfg(windows)]
use std::path::PathBuf;
use std::{fs, path::Path, time::Duration};

use anyhow::{anyhow, Context, Result};
use uuid::Uuid;

#[cfg(windows)]
use super::super::super::{
    marker::install_marker_path,
    runtime::{StagedRuntime, StagedSemanticInstall},
};
use super::super::{
    super::{durability::backup_path, marker::install_fingerprint},
    journal::{
        self, InstallTransactionJournal, JournalPath, JournalPathKind, JournalPathState,
        JournalPhase,
    },
};
#[cfg(windows)]
use crate::upgrade::UpgradePlan;

// Other data roots observe the installation-scoped staged/applying state and
// quiesce their daemons cooperatively. Keep replacement bounded by the same
// 75-second lifecycle window used for an initiating daemon shutdown.
const RETRY_ATTEMPTS: u32 = 600;
const RETRY_DELAY: Duration = Duration::from_millis(125);

#[cfg(windows)]
pub(super) fn journal_paths(
    staged: Option<&Path>,
    plan: &UpgradePlan,
    staged_runtime: Option<&StagedRuntime>,
    staged_semantic: Option<&StagedSemanticInstall>,
    marker_staged: Option<&Path>,
    attempt_id: &str,
) -> Result<Vec<JournalPath>> {
    let mut paths = Vec::new();
    if let Some(runtime) = staged_runtime {
        paths.push(path_record(
            "ONNX Runtime sidecar",
            runtime.staged_path.clone(),
            runtime.target_path.clone(),
            journal::transaction_backup_path(&runtime.target_path, attempt_id, "runtime"),
            JournalPathKind::Directory,
        )?);
    }
    if let Some(semantic) = staged_semantic {
        for path in &semantic.paths {
            paths.push(path_record(
                path.label,
                path.staged_path.clone(),
                path.target_path.clone(),
                journal::transaction_backup_path(&path.target_path, attempt_id, path.backup_label),
                if path.is_directory {
                    JournalPathKind::Directory
                } else {
                    JournalPathKind::File
                },
            )?);
        }
    }
    match (staged, marker_staged) {
        (Some(staged), Some(marker_staged)) => {
            paths.push(path_record(
                "ctx binary",
                staged.to_path_buf(),
                plan.install_path.clone(),
                journal::transaction_backup_path(&plan.install_path, attempt_id, "binary"),
                JournalPathKind::File,
            )?);
            let marker = install_marker_path(&plan.install_path);
            paths.push(path_record(
                "ctx install marker",
                marker_staged.to_path_buf(),
                marker.clone(),
                journal::transaction_backup_path(&marker, attempt_id, "marker"),
                JournalPathKind::File,
            )?);
        }
        (None, None) => {}
        _ => {
            return Err(anyhow!(
                "ctx binary and install marker staging must be present together"
            ))
        }
    }
    Ok(paths)
}

#[cfg(windows)]
fn path_record(
    label: &str,
    staged: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: JournalPathKind,
) -> Result<JournalPath> {
    if !path_present(&staged, kind)? {
        return Err(anyhow!("missing staged Windows {label}"));
    }
    Ok(JournalPath {
        label: label.to_owned(),
        target_preexisted: path_present(&target, kind)?,
        staged,
        target,
        backup,
        kind,
        state: JournalPathState::Staged,
        staged_identity: None,
        original_target_identity: None,
        backup_identity: None,
    })
}

pub(super) fn revalidate_fingerprint(transaction: &InstallTransactionJournal) -> Result<()> {
    if publication_started(transaction)? {
        return Ok(());
    }
    let helper = transaction
        .windows_helper
        .as_ref()
        .ok_or_else(|| anyhow!("Windows install journal has no replacement helper"))?;
    let observed = install_fingerprint(&transaction.install_path)?;
    if observed.binary_sha256 != helper.expected_binary_sha256
        || observed.marker_sha256 != helper.expected_marker_sha256
    {
        return Err(anyhow!(
            "managed executable or install marker changed after this upgrade plan was created; refusing stale cross-root publication"
        ));
    }
    Ok(())
}

fn publication_started(transaction: &InstallTransactionJournal) -> Result<bool> {
    for path in &transaction.paths {
        let staged = path.staged.try_exists()?;
        let target = path.target.try_exists()?;
        let backup = path.backup.try_exists()?;
        let binary_only_backed_up = path.label == "ctx binary"
            && path.state == JournalPathState::BackedUp
            && staged
            && target;
        if (!binary_only_backed_up && path.state != JournalPathState::Staged)
            || (path.label != "ctx binary" && backup)
            || !staged
            || (path.target_preexisted && !target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn publish_paths(transaction: &mut InstallTransactionJournal) -> Result<()> {
    for index in 0..transaction.paths.len() {
        if transaction.paths[index].label == "ctx binary" {
            publish_binary(transaction, index)?;
        } else {
            publish_nonbinary(transaction, index)?;
        }
        fault_after(&transaction.paths[index].label)?;
    }
    Ok(())
}

fn publish_binary(transaction: &mut InstallTransactionJournal, index: usize) -> Result<()> {
    let path = transaction.paths[index].clone();
    if !path.target_preexisted {
        return Err(anyhow!(
            "Windows replacement refuses to publish over an absent executable"
        ));
    }
    match path.state {
        JournalPathState::Staged => {
            ensure_path(&path.staged, JournalPathKind::File)?;
            ensure_path(&path.target, JournalPathKind::File)?;
            durable_backup_file(&path.target, &path.backup)?;
            transaction.paths[index].state = JournalPathState::BackedUp;
            journal::write(transaction)?;
        }
        JournalPathState::BackedUp | JournalPathState::Published | JournalPathState::Cleaned => {}
    }

    let path = transaction.paths[index].clone();
    match path.state {
        JournalPathState::BackedUp => {
            ensure_path(&path.backup, JournalPathKind::File)?;
            if !path.target.try_exists()? {
                restore_binary_backup(&path)?;
                transaction.paths[index].state = JournalPathState::Staged;
                journal::write(transaction)?;
                return Err(anyhow!(
                    "repaired an absent executable during interrupted publication; rolling back"
                ));
            }
            if path.staged.try_exists()? {
                if let Err(error) = replace_binary_with_repair(
                    transaction,
                    index,
                    &path,
                    replace_existing_file,
                    || std::thread::sleep(RETRY_DELAY),
                ) {
                    return Err(error).context("atomically replace ctx executable");
                }
            } else {
                ensure_path(&path.target, JournalPathKind::File)?;
            }
            transaction.paths[index].state = JournalPathState::Published;
            journal::write(transaction)?;
        }
        JournalPathState::Published => {
            if !path.target.try_exists()? {
                if path.backup.try_exists()? {
                    restore_binary_backup(&path)?;
                }
                return Err(anyhow!(
                    "published Windows executable was absent during recovery"
                ));
            }
            if path.staged.try_exists()? {
                return Err(anyhow!(
                    "published Windows executable still has a staged payload"
                ));
            }
        }
        JournalPathState::Cleaned => {
            ensure_path(&path.target, JournalPathKind::File)?;
        }
        JournalPathState::Staged => unreachable!("advanced above"),
    }
    Ok(())
}

fn publish_nonbinary(transaction: &mut InstallTransactionJournal, index: usize) -> Result<()> {
    let path = transaction.paths[index].clone();
    if path.state == JournalPathState::Cleaned {
        return ensure_path(&path.target, path.kind);
    }
    if path.state == JournalPathState::Published {
        ensure_path(&path.target, path.kind)?;
        if path.staged.try_exists()? {
            return Err(anyhow!(
                "published {} still has a staged payload",
                path.label
            ));
        }
        return Ok(());
    }
    if path.state == JournalPathState::Staged && path.target_preexisted {
        if path.backup.try_exists()? {
            if path.staged.try_exists()? && path.target.try_exists()? {
                return Err(anyhow!(
                    "{} has both original and backup paths during publication",
                    path.label
                ));
            }
        } else {
            ensure_path(&path.target, path.kind)?;
            retry_io(|| fs::rename(&path.target, &path.backup))?;
        }
        transaction.paths[index].state = JournalPathState::BackedUp;
        journal::write(transaction)?;
    }

    let path = transaction.paths[index].clone();
    if !path.target_preexisted && path.state == JournalPathState::Staged {
        if path.target.try_exists()? {
            if path.staged.try_exists()? {
                return Err(anyhow!(
                    "{} target appeared after transaction preparation",
                    path.label
                ));
            }
        } else {
            ensure_path(&path.staged, path.kind)?;
            retry_io(|| fs::rename(&path.staged, &path.target))?;
        }
    } else if path.state == JournalPathState::BackedUp {
        if path.target.try_exists()? {
            if path.staged.try_exists()? {
                return Err(anyhow!(
                    "{} target appeared while its backup was authoritative",
                    path.label
                ));
            }
        } else {
            ensure_path(&path.staged, path.kind)?;
            retry_io(|| fs::rename(&path.staged, &path.target))?;
        }
    }
    ensure_path(&path.target, path.kind)?;
    if path.staged.try_exists()? {
        return Err(anyhow!(
            "{} publication did not consume its staged path",
            path.label
        ));
    }
    transaction.paths[index].state = JournalPathState::Published;
    journal::write(transaction)
}

pub(super) fn rollback_paths(transaction: &mut InstallTransactionJournal) -> Result<()> {
    let publication_started = publication_started(transaction)?;
    let mut failures = Vec::new();
    for index in (0..transaction.paths.len()).rev() {
        let path = transaction.paths[index].clone();
        // Record rollback intent before moving the authoritative backup. If
        // the process dies after the move, Staged + target present + backup
        // absent is an explicitly recoverable completed-rollback layout.
        if transaction.paths[index].state != JournalPathState::Staged {
            transaction.paths[index].state = JournalPathState::Staged;
            if let Err(error) = journal::write(transaction) {
                failures.push(format!("record rollback {}: {error:#}", path.label));
                continue;
            }
        }
        if let Err(error) = rollback_path(&path) {
            failures.push(format!("{}: {error:#}", path.label));
        }
    }
    if failures.is_empty() && publication_started {
        if let Err(error) = verify_rolled_back_fingerprint(transaction) {
            failures.push(format!("verify restored executable and marker: {error:#}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("; ")))
    }
}

fn rollback_path(path: &JournalPath) -> Result<()> {
    if path.target_preexisted {
        if path.backup.try_exists()? {
            if path.label == "ctx binary" {
                if path.staged.try_exists()? && path.target.try_exists()? {
                    // The binary backup is a copy, not a rename. With the
                    // staged payload still present, publication never began;
                    // discard the backup and preserve a target that may have
                    // caused stale-fingerprint rejection.
                    remove_file_if_present(&path.backup)?;
                } else {
                    retry_io(|| move_replace_file(&path.backup, &path.target))?;
                }
            } else {
                remove_path_if_present(&path.target, path.kind)?;
                retry_io(|| fs::rename(&path.backup, &path.target))?;
            }
        }
        ensure_path(&path.target, path.kind)?;
    } else {
        remove_path_if_present(&path.target, path.kind)?;
    }
    remove_path_if_present(&path.staged, path.kind)?;
    if path.backup.try_exists()? {
        remove_path_if_present(&path.backup, path.kind)?;
    }
    Ok(())
}

fn verify_rolled_back_fingerprint(transaction: &InstallTransactionJournal) -> Result<()> {
    let helper = transaction
        .windows_helper
        .as_ref()
        .ok_or_else(|| anyhow!("Windows install journal has no replacement helper"))?;
    let observed = install_fingerprint(&transaction.install_path)?;
    if observed.binary_sha256 != helper.expected_binary_sha256
        || observed.marker_sha256 != helper.expected_marker_sha256
    {
        return Err(anyhow!(
            "rolled-back Windows executable identity does not match its journal"
        ));
    }
    Ok(())
}

pub(super) fn repair_executable_presence(
    transaction: &mut InstallTransactionJournal,
) -> Result<()> {
    let Some(index) = transaction
        .paths
        .iter()
        .position(|path| path.label == "ctx binary")
    else {
        return ensure_path(&transaction.install_path, JournalPathKind::File)
            .context("Windows Semantic repair transaction lost its managed executable");
    };
    let path = transaction.paths[index].clone();
    if path.target.try_exists()? {
        return Ok(());
    }
    ensure_path(&path.backup, JournalPathKind::File)
        .context("Windows executable is absent and its transaction backup is unavailable")?;
    restore_binary_backup(&path)?;
    transaction.paths[index].state = JournalPathState::Staged;
    transaction.phase = JournalPhase::RollingBack;
    let helper = transaction
        .windows_helper
        .as_mut()
        .expect("validated Windows helper");
    helper.failure.get_or_insert_with(|| {
        "repaired an absent executable from the transaction backup".to_owned()
    });
    journal::write(transaction)
}

fn restore_binary_backup(path: &JournalPath) -> Result<()> {
    #[cfg(windows)]
    ctx_history_core::platform_security::verify_private_executable(&path.backup)
        .context("verify private Windows executable transaction backup")?;
    retry_io(|| move_replace_file(&path.backup, &path.target))
        .context("restore ctx executable from transaction backup")
}

pub(super) fn finish_committed(
    transaction: &mut InstallTransactionJournal,
) -> Result<Option<String>> {
    for path in &transaction.paths {
        ensure_path(&path.target, path.kind).with_context(|| {
            format!(
                "committed Windows transaction has incomplete {} publication",
                path.label
            )
        })?;
        if path.staged.try_exists()? {
            return Err(anyhow!(
                "committed Windows transaction retained staged {}",
                path.label
            ));
        }
    }
    let mut cleanup_errors = Vec::new();
    for index in 0..transaction.paths.len() {
        if transaction.paths[index].state == JournalPathState::Cleaned {
            continue;
        }
        let path = transaction.paths[index].clone();
        let result = if path.backup.try_exists()? {
            if path.label == "ctx binary" {
                let durable = backup_path(&transaction.install_path);
                retry_io(|| move_replace_file(&path.backup, &durable))
            } else {
                retry_io(|| remove_path_if_present(&path.backup, path.kind))
            }
        } else {
            Ok(())
        };
        match result {
            Ok(()) => {
                transaction.paths[index].state = JournalPathState::Cleaned;
                if let Err(error) = journal::write(transaction) {
                    cleanup_errors.push(format!("record cleanup {}: {error:#}", path.label));
                }
            }
            Err(error) => cleanup_errors.push(format!("cleanup {}: {error:#}", path.label)),
        }
    }
    if cleanup_errors.is_empty() {
        transaction.phase = JournalPhase::Committed;
        journal::write(transaction)?;
        Ok(None)
    } else {
        transaction.phase = JournalPhase::CleanupPending;
        journal::write(transaction)?;
        Ok(Some(cleanup_errors.join("; ")))
    }
}

#[cfg(windows)]
pub(super) fn remove_unpublished_file(path: &Path) -> Result<()> {
    remove_file_if_present(path).map_err(Into::into)
}

#[cfg(windows)]
pub(super) fn remove_unpublished_directory(path: &Path) -> Result<()> {
    remove_path_if_present(path, JournalPathKind::Directory).map_err(Into::into)
}

#[cfg(windows)]
pub(super) fn durable_replace_file(temporary: &Path, target: &Path) -> Result<()> {
    retry_io(|| move_replace_file(temporary, target))
        .with_context(|| format!("durably publish Windows journal {}", target.display()))
}

fn durable_backup_file(source: &Path, backup: &Path) -> Result<()> {
    let parent = backup
        .parent()
        .ok_or_else(|| anyhow!("Windows backup path has no parent"))?;
    let temporary = parent.join(format!(
        ".ctx-upgrade-backup.{}.tmp",
        Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut from =
            fs::File::open(source).with_context(|| format!("open {}", source.display()))?;
        let mut to = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        std::io::copy(&mut from, &mut to)?;
        to.sync_all()?;
        drop(to);
        #[cfg(windows)]
        {
            ctx_history_core::platform_security::restrict_private_executable(&temporary)?;
            ctx_history_core::platform_security::verify_private_executable(&temporary)?;
        }
        retry_io(|| move_replace_file(&temporary, backup))
    })();
    if result.is_err() {
        let _ = remove_file_if_present(&temporary);
    }
    result
}

fn ensure_path(path: &Path, kind: JournalPathKind) -> Result<()> {
    if path_present(path, kind)? {
        Ok(())
    } else {
        Err(anyhow!(
            "required Windows transaction path is missing: {}",
            path.display()
        ))
    }
}

fn path_present(path: &Path, kind: JournalPathKind) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    let right_kind = match kind {
        JournalPathKind::File => metadata.file_type().is_file(),
        JournalPathKind::Directory => metadata.file_type().is_dir(),
    };
    if !right_kind {
        return Err(anyhow!(
            "refusing to use unsafe Windows transaction path {}",
            path.display()
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(anyhow!(
                "refusing to use unsafe Windows transaction path {}",
                path.display()
            ));
        }
    }
    Ok(true)
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_path_if_present(path: &Path, kind: JournalPathKind) -> std::io::Result<()> {
    match kind {
        JournalPathKind::File => remove_file_if_present(path),
        JournalPathKind::Directory => match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
    }
}

fn retry_io(mut operation: impl FnMut() -> std::io::Result<()>) -> Result<()> {
    let mut last = None;
    for attempt in 0..RETRY_ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(last
        .unwrap_or_else(|| std::io::Error::other("Windows retry exhausted"))
        .into())
}

/// Repair a missing executable immediately after each failed ReplaceFileW
/// call. Waiting before restoration can leave the installed command absent
/// throughout the retry delay.
pub(super) fn replace_binary_with_repair(
    transaction: &mut InstallTransactionJournal,
    index: usize,
    path: &JournalPath,
    mut replace: impl FnMut(&Path, &Path) -> std::io::Result<()>,
    mut wait: impl FnMut(),
) -> Result<()> {
    let mut last = None;
    for attempt in 0..RETRY_ATTEMPTS {
        match replace(&path.target, &path.staged) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let target_exists = path.target.try_exists()?;
                let staged_exists = path.staged.try_exists()?;
                if !target_exists {
                    ensure_path(&path.backup, JournalPathKind::File).context(
                        "ReplaceFileW left ctx.exe absent and its transaction backup is unavailable",
                    )?;
                    restore_binary_backup(path)?;
                    transaction.paths[index].state = JournalPathState::Staged;
                    journal::write(transaction)?;
                    return Err(error.into());
                }
                if !staged_exists {
                    return Err(error.into());
                }
                last = Some(error);
                if attempt + 1 < RETRY_ATTEMPTS {
                    wait();
                }
            }
        }
    }
    Err(last
        .unwrap_or_else(|| std::io::Error::other("Windows replacement retry exhausted"))
        .into())
}

#[cfg(windows)]
fn replace_existing_file(target: &Path, replacement: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let target = wide(target);
    let replacement = wide(replacement);
    // ReplaceFileW currently supports no behavior flags. In particular,
    // REPLACEFILE_WRITE_THROUGH is documented but unsupported.
    if unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn move_replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = wide(source);
    let target = wide(target);
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(all(test, not(windows)))]
fn replace_existing_file(target: &Path, replacement: &Path) -> std::io::Result<()> {
    let swap = target.with_extension("test-swap");
    fs::rename(target, &swap)?;
    fs::rename(replacement, target)?;
    fs::remove_file(swap)
}

#[cfg(all(test, not(windows)))]
fn move_replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    remove_file_if_present(target)?;
    fs::rename(source, target)
}

#[cfg(test)]
fn fault_after(label: &str) -> Result<()> {
    if std::env::var_os("CTX_UPGRADE_WINDOWS_FAULT_AFTER_PUBLICATION_FOR_TESTS")
        .is_some_and(|value| value == label)
    {
        return Err(anyhow!("injected termination during {label} publication"));
    }
    Ok(())
}

#[cfg(not(test))]
fn fault_after(_label: &str) -> Result<()> {
    Ok(())
}

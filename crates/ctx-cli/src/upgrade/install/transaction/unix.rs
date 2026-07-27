use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, Context, Result};

use super::super::super::{env_flag, UpgradePlan};
use super::super::{
    durability::{backup_file_for_atomic_replace, backup_path, sync_directory},
    marker::install_marker_path,
    runtime::{
        semantic_cache_root, semantic_provisioning_runtime_root, semantic_runtime_root,
        StagedRuntime, StagedSemanticInstall,
    },
};
use super::{
    journal::{
        self, InstallTransactionJournal, JournalPath, JournalPathIdentity, JournalPathKind,
        JournalPathState, JournalPhase, LegacyInstallTransactionJournal, LegacyJournalPhase,
    },
    ApplyResult, RecoveryOutcome,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishedPathKind {
    File,
    Directory,
}

struct PublishedPath {
    label: &'static str,
    staged: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: PublishedPathKind,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_install(
    staged: Option<&Path>,
    plan: &UpgradePlan,
    staged_runtime: Option<&StagedRuntime>,
    staged_semantic: Option<&StagedSemanticInstall>,
    marker_staged: Option<&Path>,
    unique: &str,
    data_root: &Path,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    let marker_path = install_marker_path(&plan.install_path);
    let mut paths = Vec::new();
    if let Some(runtime) = staged_runtime {
        paths.push(PublishedPath::new(
            "ONNX Runtime sidecar",
            runtime.staged_path.clone(),
            runtime.target_path.clone(),
            journal::transaction_backup_path(&runtime.target_path, unique, "runtime"),
            PublishedPathKind::Directory,
        )?);
    }
    if let Some(semantic) = staged_semantic {
        for path in &semantic.paths {
            paths.push(PublishedPath::new(
                path.label,
                path.staged_path.clone(),
                path.target_path.clone(),
                journal::transaction_backup_path(&path.target_path, unique, path.backup_label),
                if path.is_directory {
                    PublishedPathKind::Directory
                } else {
                    PublishedPathKind::File
                },
            )?);
        }
    }
    match (staged, marker_staged) {
        (Some(staged), Some(marker_staged)) => {
            paths.push(PublishedPath::new(
                "ctx binary",
                staged.to_path_buf(),
                plan.install_path.clone(),
                journal::transaction_backup_path(&plan.install_path, unique, "binary"),
                PublishedPathKind::File,
            )?);
            paths.push(PublishedPath::new(
                "ctx install marker",
                marker_staged.to_path_buf(),
                marker_path.clone(),
                journal::transaction_backup_path(&marker_path, unique, "marker"),
                PublishedPathKind::File,
            )?);
        }
        (None, None) => {}
        _ => {
            return Err(anyhow!(
                "ctx binary and install marker staging must be present together"
            ))
        }
    }

    sync_staged_parent_directories(&paths)?;
    let has_semantic_runtime = paths.iter().any(|path| {
        matches!(
            path.label,
            "Semantic CPU runtime" | "Semantic Windows ML runtime" | "Semantic CUDA runtime"
        )
    });
    let mut transaction = InstallTransactionJournal::new(
        unique.to_owned(),
        fs::canonicalize(data_root)
            .with_context(|| format!("canonicalize upgrade data root {}", data_root.display()))?,
        if has_semantic_runtime {
            semantic_provisioning_runtime_root(data_root)?
        } else {
            semantic_runtime_root(data_root)?
        },
        plan.install_path.clone(),
        paths
            .iter()
            .map(PublishedPath::journal_path)
            .collect::<Result<Vec<_>>>()?,
        None,
    );
    if staged_semantic.is_some() {
        transaction.semantic_cache_root = Some(
            fs::canonicalize(semantic_cache_root(data_root)?)
                .context("canonicalize selected Semantic cache root")?,
        );
    }
    journal::write(&transaction)?;
    before_publish()?;
    transaction.phase = JournalPhase::Publishing;
    journal::write(&transaction)?;

    let publish_result = publish_paths(&mut paths, &mut transaction, data_root);
    if let Err(primary) = publish_result {
        return rollback_failed_publication(primary, data_root, &mut transaction, true);
    }
    transaction.phase = JournalPhase::Committed;
    if let Err(primary) = journal::write(&transaction) {
        return rollback_failed_publication(primary, data_root, &mut transaction, false);
    }
    if cfg!(debug_assertions) && env_flag("CTX_UPGRADE_ABORT_AFTER_COMMIT_FOR_TESTS") {
        std::process::exit(88);
    }
    match finish_committed_transaction(data_root, &mut transaction)? {
        RecoveryOutcome::Committed => Ok(ApplyResult::Applied),
        RecoveryOutcome::CleanupPending { warning } => {
            Ok(ApplyResult::AppliedCleanupPending { warning })
        }
        outcome => Err(anyhow!(
            "committed install returned an invalid recovery outcome: {outcome:?}"
        )),
    }
}

fn publish_paths(
    paths: &mut [PublishedPath],
    transaction: &mut InstallTransactionJournal,
    _data_root: &Path,
) -> Result<()> {
    for (index, path) in paths.iter_mut().enumerate() {
        path.backup(&mut transaction.paths[index])?;
        if transaction.paths[index].target_preexisted {
            journal::write(transaction)?;
            abort_after_backup_for_tests(path.label);
        }
        if path.label == "ctx install marker"
            && cfg!(debug_assertions)
            && env_flag("CTX_UPGRADE_FAIL_MARKER_PUBLISH_FOR_TESTS")
        {
            return Err(anyhow!("injected install marker publication failure"));
        }
        path.publish(&mut transaction.paths[index])?;
        journal::write(transaction)?;
        abort_after_publish_for_tests(path.test_point());
    }
    Ok(())
}

fn rollback_failed_publication(
    primary: anyhow::Error,
    _data_root: &Path,
    transaction: &mut InstallTransactionJournal,
    inject_runtime_restore_failure: bool,
) -> Result<ApplyResult> {
    let mut failures = Vec::new();
    transaction.phase = JournalPhase::RollingBack;
    if let Err(error) = journal::write(transaction) {
        failures.push(format!("record rollback phase: {error:#}"));
    }
    if let Err(error) = rollback_paths(
        &transaction.paths,
        &transaction.install_path,
        inject_runtime_restore_failure,
    ) {
        failures.push(format!("{error:#}"));
    }
    if failures.is_empty() {
        transaction.phase = JournalPhase::RolledBack;
        if let Err(error) = journal::write(transaction) {
            failures.push(format!("record rolled-back phase: {error:#}"));
        }
    }
    if failures.is_empty() {
        if let Err(error) = journal::remove(&transaction.install_path) {
            failures.push(format!("remove transaction journal: {error:#}"));
        } else {
            return Err(primary);
        }
    }
    Err(anyhow!(
        "{primary:#}; rollback failures: {}",
        failures.join("; ")
    ))
}

pub(super) fn recover_transaction(
    data_root: &Path,
    transaction: &mut InstallTransactionJournal,
) -> Result<RecoveryOutcome> {
    match transaction.phase {
        JournalPhase::Prepared => {
            remove_prepared_staging(&transaction.paths)?;
            journal::remove(&transaction.install_path)?;
            Ok(RecoveryOutcome::RolledBack {
                restored_executable: None,
            })
        }
        phase if phase.mutation_may_have_started() => {
            transaction.phase = JournalPhase::RollingBack;
            journal::write(transaction)?;
            let restored_executable =
                rollback_paths(&transaction.paths, &transaction.install_path, false)?;
            transaction.phase = JournalPhase::RolledBack;
            journal::write(transaction)?;
            journal::remove(&transaction.install_path)?;
            Ok(RecoveryOutcome::RolledBack {
                restored_executable,
            })
        }
        phase if phase.committed() => finish_committed_transaction(data_root, transaction),
        JournalPhase::RolledBack => {
            journal::remove(&transaction.install_path)?;
            Ok(RecoveryOutcome::RolledBack {
                restored_executable: None,
            })
        }
        JournalPhase::HelperReady => Err(anyhow!(
            "Unix install transaction has an invalid helper-ready phase"
        )),
        _ => Err(anyhow!(
            "Unix install transaction has an invalid recovery phase"
        )),
    }
}

/// Recover the exact v0.25 journal format after strict path validation.  The
/// old format did not persist path identities, so it is never accepted as a
/// new journal; it is consumed once from the invoking data root and removed.
pub(super) fn recover_legacy_transaction(
    data_root: &Path,
    transaction: &LegacyInstallTransactionJournal,
) -> Result<RecoveryOutcome> {
    match transaction.phase {
        LegacyJournalPhase::Publishing => {
            let mut restored_executable = None;
            for path in transaction.paths.iter().rev() {
                let staged = legacy_present(&path.staged)?;
                let target = legacy_present(&path.target)?;
                let backup = legacy_present(&path.backup)?;
                if backup {
                    if staged {
                        // v0.25 could crash after the old target was moved out
                        // and before the staged path was published.  A runtime
                        // directory has no atomic-replace hard-link behavior,
                        // so staged+backup with no target must restore the
                        // backup before the staged directory is discarded.
                        // The one safe discard case is its file publication
                        // path: both the original target and staged file are
                        // still present, so the backup is redundant.
                        if target {
                            if path.kind == JournalPathKind::Directory {
                                return Err(anyhow!(
                                    "v0.25 interrupted {} has both target and staged directories",
                                    path.label
                                ));
                            }
                            legacy_remove(&path.backup, path.kind)?;
                        } else {
                            fs::rename(&path.backup, &path.target).with_context(|| {
                                format!(
                                    "restore v0.25 interrupted {} from {}",
                                    path.label,
                                    path.backup.display()
                                )
                            })?;
                            if path.label == "ctx binary" {
                                restored_executable = Some(path.target.clone());
                            }
                        }
                    } else {
                        if target {
                            legacy_remove(&path.target, path.kind)?;
                        }
                        fs::rename(&path.backup, &path.target).with_context(|| {
                            format!(
                                "restore v0.25 interrupted {} from {}",
                                path.label,
                                path.backup.display()
                            )
                        })?;
                        if path.label == "ctx binary" {
                            restored_executable = Some(path.target.clone());
                        }
                    }
                } else if !staged && target {
                    legacy_remove(&path.target, path.kind)?;
                }
                if staged {
                    legacy_remove(&path.staged, path.kind)?;
                }
                if let Some(parent) = path.target.parent() {
                    sync_directory(parent)?;
                }
            }
            journal::remove_legacy(data_root)?;
            Ok(RecoveryOutcome::RolledBack {
                restored_executable,
            })
        }
        LegacyJournalPhase::Committed => {
            for path in &transaction.paths {
                if !legacy_present(&path.target)? || legacy_present(&path.staged)? {
                    return Err(anyhow!(
                        "v0.25 committed install transaction has incomplete {} publication",
                        path.label
                    ));
                }
            }
            for path in &transaction.paths {
                if !legacy_present(&path.backup)? {
                    continue;
                }
                if path.label == "ctx binary" {
                    remove_owner_regular_file(&backup_path(&transaction.install_path))?;
                    fs::rename(&path.backup, backup_path(&transaction.install_path))?;
                } else {
                    legacy_remove(&path.backup, path.kind)?;
                }
                if let Some(parent) = path.target.parent() {
                    sync_directory(parent)?;
                }
            }
            journal::remove_legacy(data_root)?;
            Ok(RecoveryOutcome::Committed)
        }
    }
}

fn legacy_present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(anyhow!(
            "v0.25 journal path is a symlink: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspect v0.25 journal path {}", path.display()))
        }
    }
}

fn legacy_remove(path: &Path, kind: JournalPathKind) -> Result<()> {
    match kind {
        JournalPathKind::File => fs::remove_file(path),
        JournalPathKind::Directory => fs::remove_dir_all(path),
    }
    .with_context(|| format!("remove v0.25 journal path {}", path.display()))
}

#[allow(dead_code)]
pub(super) fn reexec_restored_executable(path: &Path, attempt_id: &str) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    let error = Command::new(path)
        .args(env::args_os().skip(1))
        .env(super::RECOVERY_REEXEC_ENV, attempt_id)
        .exec();
    Err(error).with_context(|| format!("re-exec restored ctx {}", path.display()))
}

fn remove_prepared_staging(paths: &[JournalPath]) -> Result<()> {
    for path in paths {
        remove_owned_path(&path.staged, path.kind, path.staged_identity.as_ref())?;
        if let Some(parent) = path.staged.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn rollback_paths(
    paths: &[JournalPath],
    install_path: &Path,
    inject_runtime_restore_failure: bool,
) -> Result<Option<PathBuf>> {
    let mut restored_executable = None;
    for path in paths.iter().rev() {
        let target_is_published =
            path_matches_identity(&path.target, path.staged_identity.as_ref())?;
        let target_is_original =
            path_matches_identity(&path.target, path.original_target_identity.as_ref())?;
        let expected_backup_identity = path
            .backup_identity
            .as_ref()
            .or(path.original_target_identity.as_ref());

        reject_replaced_owned_path(&path.staged, path.staged_identity.as_ref(), &path.label)?;
        reject_replaced_owned_path(&path.backup, expected_backup_identity, &path.label)?;
        if path.target_preexisted {
            if target_is_published {
                remove_owned_path(&path.target, path.kind, path.staged_identity.as_ref())?;
            } else if path_present(&path.target)? && !target_is_original {
                return Err(anyhow!(
                    "refusing to replace changed {} target {} during recovery",
                    path.label,
                    path.target.display()
                ));
            }
            restore_or_discard_backup(
                path,
                install_path,
                expected_backup_identity,
                inject_runtime_restore_failure,
                &mut restored_executable,
            )?;
        } else if target_is_published {
            remove_owned_path(&path.target, path.kind, path.staged_identity.as_ref())?;
        } else if path_present(&path.target)? {
            return Err(anyhow!(
                "refusing to remove unexpected {} target {} that did not preexist",
                path.label,
                path.target.display()
            ));
        } else if path_present(&path.backup)? {
            return Err(anyhow!(
                "unexpected backup exists for newly created {} at {}",
                path.label,
                path.backup.display()
            ));
        }
        remove_owned_path(&path.staged, path.kind, path.staged_identity.as_ref())?;
        if let Some(parent) = path.target.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(restored_executable)
}

fn restore_or_discard_backup(
    path: &JournalPath,
    install_path: &Path,
    expected_backup_identity: Option<&JournalPathIdentity>,
    inject_runtime_restore_failure: bool,
    restored_executable: &mut Option<PathBuf>,
) -> Result<()> {
    if !path_present(&path.backup)? {
        if path_present(&path.target)? {
            return Ok(());
        }
        return Err(anyhow!(
            "interrupted {} lost both its original target and recoverable backup",
            path.label
        ));
    }
    if inject_runtime_restore_failure
        && path.label == "ONNX Runtime sidecar"
        && cfg!(debug_assertions)
        && env_flag("CTX_UPGRADE_FAIL_RUNTIME_RESTORE_FOR_TESTS")
    {
        return Err(anyhow!(
            "injected ONNX Runtime restore failure; recoverable backup retained at {}",
            path.backup.display()
        ));
    }
    if path_present(&path.target)? {
        if !path_matches_identity(&path.target, path.original_target_identity.as_ref())? {
            return Err(anyhow!(
                "cannot restore {} while an unowned target exists at {}",
                path.label,
                path.target.display()
            ));
        }
        remove_owned_path(&path.backup, path.kind, expected_backup_identity)?;
    } else {
        fs::rename(&path.backup, &path.target).with_context(|| {
            format!(
                "restore interrupted {} from {}",
                path.label,
                path.backup.display()
            )
        })?;
        if path.target == install_path {
            *restored_executable = Some(path.target.clone());
        }
    }
    Ok(())
}

fn finish_committed_transaction(
    _data_root: &Path,
    transaction: &mut InstallTransactionJournal,
) -> Result<RecoveryOutcome> {
    let mut cleanup_errors = finish_committed_paths(transaction)?;
    if cleanup_errors.is_empty() {
        if let Err(error) = journal::remove(&transaction.install_path) {
            cleanup_errors.push(format!("remove transaction journal: {error:#}"));
        }
    }
    if cleanup_errors.is_empty() {
        return Ok(RecoveryOutcome::Committed);
    }
    transaction.phase = JournalPhase::CleanupPending;
    if let Err(error) = journal::write(transaction) {
        cleanup_errors.push(format!("record cleanup-pending phase: {error:#}"));
    }
    Ok(RecoveryOutcome::CleanupPending {
        warning: format!(
            "upgrade committed; cleanup remains pending: {}",
            cleanup_errors.join("; ")
        ),
    })
}

fn finish_committed_paths(transaction: &mut InstallTransactionJournal) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    for path in &transaction.paths {
        match (
            path_matches_identity(&path.target, path.staged_identity.as_ref()),
            path_present(&path.staged),
        ) {
            (Ok(true), Ok(false)) => {}
            (Ok(_), Ok(_)) => {
                return Err(anyhow!(
                    "committed install transaction has incomplete or replaced {} publication",
                    path.label
                ))
            }
            (Err(error), _) | (_, Err(error)) => return Err(error),
        }
    }
    for path in &mut transaction.paths {
        match path_present(&path.backup) {
            Ok(false) => {
                path.state = JournalPathState::Cleaned;
                continue;
            }
            Ok(true) => {}
            Err(error) => {
                errors.push(format!("{error:#}"));
                continue;
            }
        }
        if cfg!(debug_assertions) && env_flag("CTX_UPGRADE_FAIL_POST_COMMIT_CLEANUP_FOR_TESTS") {
            errors.push(format!(
                "injected post-commit cleanup failure for {}",
                path.label
            ));
            continue;
        }
        let expected = path
            .backup_identity
            .as_ref()
            .or(path.original_target_identity.as_ref());
        let result = if path.label == "ctx binary" {
            retain_binary_backup(path, &backup_path(&transaction.install_path), expected)
        } else {
            remove_owned_path(&path.backup, path.kind, expected)
        };
        match result {
            Ok(()) => {
                path.state = JournalPathState::Cleaned;
                if let Some(parent) = path.target.parent() {
                    if let Err(error) = sync_directory(parent) {
                        errors.push(format!("{error:#}"));
                    }
                }
            }
            Err(error) => errors.push(format!("{error:#}")),
        }
    }
    Ok(errors)
}

fn retain_binary_backup(
    path: &JournalPath,
    durable_backup: &Path,
    expected_identity: Option<&JournalPathIdentity>,
) -> Result<()> {
    ensure_owned_identity(&path.backup, expected_identity)?;
    remove_owner_regular_file(durable_backup)?;
    fs::rename(&path.backup, durable_backup).with_context(|| {
        format!(
            "retain previous ctx binary {} at {}",
            path.backup.display(),
            durable_backup.display()
        )
    })
}

impl PublishedPath {
    fn new(
        label: &'static str,
        staged: PathBuf,
        target: PathBuf,
        backup: PathBuf,
        kind: PublishedPathKind,
    ) -> Result<Self> {
        validate_path_kind(&staged, kind)?;
        Ok(Self {
            label,
            staged,
            target,
            backup,
            kind,
        })
    }

    fn journal_path(&self) -> Result<JournalPath> {
        let staged_identity = path_identity(&self.staged)?;
        let original_target_identity = path_identity(&self.target)?;
        Ok(JournalPath {
            label: self.label.to_owned(),
            staged: self.staged.clone(),
            target: self.target.clone(),
            backup: self.backup.clone(),
            kind: self.kind.into(),
            target_preexisted: original_target_identity.is_some(),
            state: JournalPathState::Staged,
            staged_identity,
            original_target_identity,
            backup_identity: None,
        })
    }

    fn backup(&self, record: &mut JournalPath) -> Result<()> {
        if path_present(&self.backup)? {
            return Err(anyhow!(
                "{} transaction backup already exists at {}",
                self.label,
                self.backup.display()
            ));
        }
        ensure_owned_identity(&self.staged, record.staged_identity.as_ref())?;
        if record.target_preexisted {
            ensure_owned_identity(&self.target, record.original_target_identity.as_ref())?;
            backup_target(&self.target, &self.backup, self.label, self.kind)?;
            if let Some(parent) = self.target.parent() {
                sync_directory(parent)?;
            }
            record.backup_identity = path_identity(&self.backup)?;
            record.state = JournalPathState::BackedUp;
        } else if path_present(&self.target)? {
            return Err(anyhow!(
                "{} target appeared after transaction preparation at {}",
                self.label,
                self.target.display()
            ));
        }
        Ok(())
    }

    fn publish(&self, record: &mut JournalPath) -> Result<()> {
        ensure_owned_identity(&self.staged, record.staged_identity.as_ref())?;
        if record.target_preexisted {
            if self.kind == PublishedPathKind::Directory && path_present(&self.target)? {
                return Err(anyhow!(
                    "{} target still exists after directory backup at {}",
                    self.label,
                    self.target.display()
                ));
            }
            if !path_present(&self.backup)? {
                return Err(anyhow!(
                    "{} recoverable backup disappeared before publication at {}",
                    self.label,
                    self.backup.display()
                ));
            }
        } else if path_present(&self.target)? {
            return Err(anyhow!(
                "{} target appeared before publication at {}",
                self.label,
                self.target.display()
            ));
        }
        fs::rename(&self.staged, &self.target).with_context(|| {
            format!(
                "publish {} {} to {}",
                self.label,
                self.staged.display(),
                self.target.display()
            )
        })?;
        if let Some(parent) = self.target.parent() {
            sync_directory(parent)?;
        }
        ensure_owned_identity(&self.target, record.staged_identity.as_ref())?;
        record.state = JournalPathState::Published;
        Ok(())
    }

    fn test_point(&self) -> &'static str {
        match self.label {
            "ONNX Runtime sidecar"
            | "Semantic CPU runtime"
            | "Semantic Windows ML runtime"
            | "Semantic CUDA runtime" => "runtime",
            "Semantic model" | "Semantic Core ML bundle" | "Semantic Core ML completion marker" => {
                "semantic"
            }
            "ctx binary" => "binary",
            "ctx install marker" => "marker",
            _ => "unknown",
        }
    }
}

impl From<PublishedPathKind> for JournalPathKind {
    fn from(value: PublishedPathKind) -> Self {
        match value {
            PublishedPathKind::File => Self::File,
            PublishedPathKind::Directory => Self::Directory,
        }
    }
}

fn backup_target(target: &Path, backup: &Path, label: &str, kind: PublishedPathKind) -> Result<()> {
    match kind {
        PublishedPathKind::File => backup_file_for_atomic_replace(target, backup, label),
        PublishedPathKind::Directory => fs::rename(target, backup).with_context(|| {
            format!(
                "backup {label} {} to {}",
                target.display(),
                backup.display()
            )
        }),
    }
}

fn sync_staged_parent_directories(paths: &[PublishedPath]) -> Result<()> {
    let mut parents = BTreeSet::new();
    for path in paths {
        let parent = path
            .staged
            .parent()
            .ok_or_else(|| anyhow!("staged {} has no parent", path.label))?;
        parents.insert(parent.to_path_buf());
    }
    for parent in parents {
        sync_directory(&parent)?;
    }
    Ok(())
}

fn validate_path_kind(path: &Path, kind: PublishedPathKind) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect staged path {}", path.display()))?;
    let valid = match kind {
        PublishedPathKind::File => metadata.file_type().is_file(),
        PublishedPathKind::Directory => metadata.file_type().is_dir(),
    };
    if !valid {
        return Err(anyhow!(
            "staged install path has the wrong kind: {}",
            path.display()
        ));
    }
    Ok(())
}

fn path_identity(path: &Path) -> Result<Option<JournalPathIdentity>> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect identity of {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(anyhow!(
            "install transaction path is not owner-safe: {}",
            path.display()
        ));
    }
    Ok(Some(JournalPathIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
    }))
}

fn path_matches_identity(path: &Path, expected: Option<&JournalPathIdentity>) -> Result<bool> {
    let Some(expected) = expected else {
        return Ok(false);
    };
    Ok(path_identity(path)?.as_ref() == Some(expected))
}

fn path_present(path: &Path) -> Result<bool> {
    Ok(path_identity(path)?.is_some())
}

fn ensure_owned_identity(path: &Path, expected: Option<&JournalPathIdentity>) -> Result<()> {
    let expected = expected.ok_or_else(|| {
        anyhow!(
            "install transaction has no ownership identity for {}",
            path.display()
        )
    })?;
    if path_identity(path)?.as_ref() != Some(expected) {
        return Err(anyhow!(
            "refusing to clean replaced install transaction path {}",
            path.display()
        ));
    }
    Ok(())
}

fn reject_replaced_owned_path(
    path: &Path,
    expected: Option<&JournalPathIdentity>,
    label: &str,
) -> Result<()> {
    if path_present(path)? && !path_matches_identity(path, expected)? {
        return Err(anyhow!(
            "refusing to clean replacement {label} path {}",
            path.display()
        ));
    }
    Ok(())
}

fn remove_owned_path(
    path: &Path,
    kind: JournalPathKind,
    expected: Option<&JournalPathIdentity>,
) -> Result<()> {
    if !path_present(path)? {
        return Ok(());
    }
    ensure_owned_identity(path, expected)?;
    let result = match kind {
        JournalPathKind::File => fs::remove_file(path),
        JournalPathKind::Directory => fs::remove_dir_all(path),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

pub(super) fn remove_owner_regular_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(anyhow!(
            "refusing to remove non-owned regular file {}",
            path.display()
        ));
    }
    fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
}

pub(super) fn remove_owner_directory_tree(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(anyhow!(
            "refusing to remove non-owned directory {}",
            path.display()
        ));
    }
    fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
}

fn abort_after_publish_for_tests(point: &str) {
    if cfg!(debug_assertions)
        && env::var("CTX_UPGRADE_ABORT_AFTER_PUBLISH_FOR_TESTS")
            .ok()
            .is_some_and(|value| value == point)
    {
        std::process::exit(86);
    }
}

fn abort_after_backup_for_tests(label: &str) {
    let point = match label {
        "ONNX Runtime sidecar"
        | "Semantic CPU runtime"
        | "Semantic Windows ML runtime"
        | "Semantic CUDA runtime" => "runtime",
        "Semantic model" | "Semantic Core ML bundle" | "Semantic Core ML completion marker" => {
            "semantic"
        }
        "ctx binary" => "binary",
        "ctx install marker" => "marker",
        _ => return,
    };
    if cfg!(debug_assertions)
        && env::var("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS")
            .ok()
            .is_some_and(|value| value == point)
    {
        std::process::exit(87);
    }
}

#[cfg(test)]
pub(super) fn path_identity_for_test(path: &Path) -> Result<JournalPathIdentity> {
    path_identity(path)?.ok_or_else(|| anyhow!("missing test path {}", path.display()))
}

#[cfg(test)]
pub(super) fn rollback_paths_for_test(
    paths: &[JournalPath],
    install_path: &Path,
) -> Result<Option<PathBuf>> {
    rollback_paths(paths, install_path, false)
}

#[cfg(test)]
pub(super) fn finish_committed_for_test(
    data_root: &Path,
    transaction: &mut InstallTransactionJournal,
) -> Result<RecoveryOutcome> {
    finish_committed_transaction(data_root, transaction)
}

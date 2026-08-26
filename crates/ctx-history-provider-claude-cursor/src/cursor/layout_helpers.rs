use super::*;

pub(super) enum DirectChild {
    Missing,
    File(OpenedProviderSourceFile),
    Directory(ProviderSourceDirectory),
    Failed,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn open_direct_child(
    parent_path: &Path,
    parent: &ProviderSourceDirectory,
    name: &OsStr,
    symlink_invalidates: bool,
    special_invalidates: bool,
    limits: CursorInventoryLimits,
    inventory: &mut CursorRootInventory,
) -> DirectChild {
    let path = parent_path.join(name);
    if !admit_metadata_entry(&path, limits, inventory) {
        return DirectChild::Failed;
    }
    match parent.open_child(name) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            inventory.stats.regular_files_visited =
                inventory.stats.regular_files_visited.saturating_add(1);
            DirectChild::File(file)
        }
        Ok(OpenedProviderSourcePath::Directory(directory)) => {
            if admit_directory(&path, limits, inventory) {
                DirectChild::Directory(directory)
            } else {
                DirectChild::Failed
            }
        }
        Err(error) if capture_error_is_not_found(&error) => DirectChild::Missing,
        Err(error) => {
            reject_open_error(
                path,
                error,
                symlink_invalidates,
                special_invalidates,
                inventory,
            );
            DirectChild::Failed
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn open_enumerated_child(
    path: &Path,
    parent: &ProviderSourceDirectory,
    name: &OsStr,
    symlink_invalidates: bool,
    special_invalidates: bool,
    limits: CursorInventoryLimits,
    inventory: &mut CursorRootInventory,
) -> Option<OpenedProviderSourcePath> {
    match parent.open_child(name) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            inventory.stats.regular_files_visited =
                inventory.stats.regular_files_visited.saturating_add(1);
            Some(OpenedProviderSourcePath::File(file))
        }
        Ok(OpenedProviderSourcePath::Directory(directory)) => {
            admit_directory(path, limits, inventory)
                .then_some(OpenedProviderSourcePath::Directory(directory))
        }
        Err(error) => {
            reject_open_error(
                path.to_path_buf(),
                error,
                symlink_invalidates,
                special_invalidates,
                inventory,
            );
            None
        }
    }
}

pub(super) fn read_directory_entries(
    directory_path: &Path,
    directory: &ProviderSourceDirectory,
    limits: CursorInventoryLimits,
    inventory: &mut CursorRootInventory,
) -> Vec<std::ffi::OsString> {
    // Both scan goals use this bounded sorted view. Complete inventory needs
    // deterministic order; retaining it for the existential goal also keeps
    // Found-versus-budget outcomes independent of native enumeration order.
    let remaining = limits
        .max_metadata_entries
        .saturating_sub(inventory.stats.entries_visited);
    let reader = match directory.entries(remaining.saturating_add(1)) {
        Ok(entries) => entries,
        Err(CaptureError::InvalidProviderTranscriptPath { .. }) => {
            inventory.reject(
                directory_path.to_path_buf(),
                CursorDiscoveryIssueKind::LimitExceeded,
                format!(
                    "Cursor discovery exceeds the {}-metadata-entry inventory limit",
                    limits.max_metadata_entries
                ),
                true,
            );
            return Vec::new();
        }
        Err(error) => {
            inventory.reject(
                directory_path.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for name in reader {
        let path = directory_path.join(&name);
        if !admit_metadata_entry(&path, limits, inventory) {
            return Vec::new();
        }
        entries.push(name);
    }
    entries
}

pub(super) fn admit_metadata_entry(
    path: &Path,
    limits: CursorInventoryLimits,
    inventory: &mut CursorRootInventory,
) -> bool {
    let observed = inventory.stats.entries_visited.saturating_add(1);
    if observed > limits.max_metadata_entries {
        inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::LimitExceeded,
            format!(
                "Cursor discovery exceeds the {}-metadata-entry inventory limit",
                limits.max_metadata_entries
            ),
            true,
        );
        return false;
    }
    if path.as_os_str().as_encoded_bytes().len() > limits.max_path_bytes {
        inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::LimitExceeded,
            format!(
                "Cursor discovery path exceeds the {}-encoded-byte inventory limit",
                limits.max_path_bytes
            ),
            true,
        );
        return false;
    }
    inventory.stats.entries_visited = observed;
    true
}

pub(super) fn admit_directory(
    path: &Path,
    limits: CursorInventoryLimits,
    inventory: &mut CursorRootInventory,
) -> bool {
    let observed = inventory.stats.directories_visited.saturating_add(1);
    if observed > limits.max_directories {
        inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::LimitExceeded,
            format!(
                "Cursor discovery exceeds the {}-directory inventory limit",
                limits.max_directories
            ),
            true,
        );
        return false;
    }
    inventory.stats.directories_visited = observed;
    true
}

fn reject_open_error(
    path: PathBuf,
    error: CaptureError,
    symlink_invalidates: bool,
    special_invalidates: bool,
    inventory: &mut CursorRootInventory,
) {
    if ctx_history_provider_runtime::source_io::is_symlink_source_rejection(&error) {
        inventory.reject(
            path,
            CursorDiscoveryIssueKind::Symlink,
            error.to_string(),
            symlink_invalidates,
        );
    } else if ctx_history_provider_runtime::source_io::is_non_regular_source_rejection(&error) {
        inventory.reject(
            path,
            CursorDiscoveryIssueKind::SpecialFile,
            error.to_string(),
            special_invalidates,
        );
    } else {
        inventory.reject(path, CursorDiscoveryIssueKind::Io, error.to_string(), true);
    }
}

pub(super) fn capture_error_is_not_found(error: &CaptureError) -> bool {
    matches!(error, CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
        || matches!(
            error,
            CaptureError::SystemIo { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound
        )
}

pub(super) fn revalidate_directory(
    path: &Path,
    directory: &ProviderSourceDirectory,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    if let Err(error) = directory.revalidate() {
        inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::Io,
            error.to_string(),
            true,
        );
        if goal == CursorScanGoal::FirstTranscript {
            inventory.transcripts.clear();
        }
    }
}

pub(super) fn scan_should_stop(goal: CursorScanGoal, inventory: &CursorRootInventory) -> bool {
    inventory.has_issue_kind(CursorDiscoveryIssueKind::LimitExceeded)
        || (goal == CursorScanGoal::FirstTranscript && inventory.has_transcripts())
}

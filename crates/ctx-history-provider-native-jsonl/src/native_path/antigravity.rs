use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::CaptureProvider;
use ctx_history_jsonl::{
    JsonlFamilyError, JsonlFamilyLeaf, JsonlFamilyRejectedLeaf, OpenedProviderSourceFile,
    OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot,
};
use ctx_history_source_io::{
    open_provider_source_path_mapped as open_provider_source_path, SourceIoError,
    NON_REGULAR_PROVIDER_SOURCE_REASON, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, REPARSE_PROVIDER_SOURCE_REASON,
    SYMLINK_PROVIDER_SOURCE_REASON,
};

use super::source_backed::{
    bind_opened_leaf, DirectJsonlFamilyAdapter, DirectJsonlInventoryBudget,
};
use crate::{
    NativeJsonlError as CaptureError, NativeJsonlRuntime, ProviderJsonlInventoryLimit, Result,
    ANTIGRAVITY_CLI_SOURCE_FORMAT,
};

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v7-record-admission-order";
const SYSTEM_GENERATED_DIR: &str = ".system_generated";
const LOGS_DIR: &str = "logs";
const FULL_TRANSCRIPT_FILE: &str = "transcript_full.jsonl";
const LIVE_TRANSCRIPT_FILE: &str = "transcript.jsonl";

pub const fn antigravity_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        "antigravity-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}

pub(super) fn discover_directory<R: NativeJsonlRuntime>(
    adapter: DirectJsonlFamilyAdapter<R>,
    source_root: &Path,
    directory: &ProviderSourceDirectory<CaptureError>,
    authority: &Arc<ProviderSourceRoot<CaptureError>>,
    leaves: &mut Vec<JsonlFamilyLeaf<CaptureError>>,
    rejected_leaves: &mut Vec<JsonlFamilyRejectedLeaf>,
    budget: &mut DirectJsonlInventoryBudget,
) -> Result<()> {
    visit_antigravity_root(
        source_root,
        directory,
        budget,
        true,
        |child_path, child_relative_path, file| {
            bind_opened_leaf(
                adapter,
                source_root,
                child_path,
                child_relative_path,
                Arc::clone(authority),
                file,
                leaves,
                rejected_leaves,
            )
        },
    )
}

trait AntigravitySourceError {
    fn is_not_found_err(&self) -> bool;
    fn is_ignorable_entry_err(&self) -> bool;
}

impl AntigravitySourceError for CaptureError {
    fn is_not_found_err(&self) -> bool {
        self.is_not_found()
    }

    fn is_ignorable_entry_err(&self) -> bool {
        self.is_ignorable_membership_entry()
    }
}

impl AntigravitySourceError for SourceIoError {
    fn is_not_found_err(&self) -> bool {
        matches!(
            self,
            Self::Io(error) | Self::SystemIo { source: error, .. }
                if error.kind() == std::io::ErrorKind::NotFound
        )
    }

    fn is_ignorable_entry_err(&self) -> bool {
        matches!(
            self,
            Self::InvalidProviderTranscriptPath { reason, .. }
                if *reason == SYMLINK_PROVIDER_SOURCE_REASON
                    || *reason == REPARSE_PROVIDER_SOURCE_REASON
                    || *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
        )
    }
}

/// Visits only the finite Antigravity transcript structure under `directory`,
/// pruning tool-step and scratch subtrees (such as `.system_generated/steps/<n>`)
/// that cannot contain official transcript leaves.
pub(super) fn visit_antigravity_root(
    source_root: &Path,
    directory: &ProviderSourceDirectory<CaptureError>,
    budget: &mut DirectJsonlInventoryBudget,
    strict_root_revalidation: bool,
    on_selected: impl FnMut(PathBuf, PathBuf, OpenedProviderSourceFile<CaptureError>) -> Result<()>,
) -> Result<()> {
    visit_antigravity_root_with_error(
        source_root,
        directory,
        budget,
        strict_root_revalidation,
        on_selected,
    )
}

pub(crate) fn visit_native_jsonl_files_with<E>(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> std::result::Result<(), E>,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
{
    match open_provider_source_path::<SourceIoError>(root).map_err(E::from)? {
        OpenedProviderSourcePath::File(file) => {
            file.revalidate_same_object().map_err(E::from)?;
            if super::super::dialect::native_jsonl_file_is_selected(
                CaptureProvider::Antigravity,
                root,
                root,
                false,
            ) {
                visit(root)?;
                Ok(1)
            } else {
                Ok(0)
            }
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let mut budget = DirectJsonlInventoryBudget::default();
            let mut count = 0usize;
            visit_antigravity_root_with_error::<SourceIoError, E>(
                root,
                &directory,
                &mut budget,
                false,
                |path, _, file| {
                    file.revalidate_same_object().map_err(E::from)?;
                    visit(&path)?;
                    count = count.saturating_add(1);
                    Ok(())
                },
            )?;
            Ok(count)
        }
    }
}

fn visit_antigravity_root_with_error<M, E>(
    source_root: &Path,
    directory: &ProviderSourceDirectory<M>,
    budget: &mut DirectJsonlInventoryBudget,
    strict_root_revalidation: bool,
    mut on_selected: impl FnMut(
        PathBuf,
        PathBuf,
        OpenedProviderSourceFile<M>,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    M: AntigravitySourceError + From<SourceIoError>,
    E: From<M> + From<SourceIoError>,
{
    count_directory_with::<E>(budget)?;
    let names = directory
        .entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)
        .map_err(E::from)?;
    count_metadata_entries_with::<E>(budget, names.len())?;

    let mut checked_direct_logs = false;
    for name in names {
        if name == OsStr::new(FULL_TRANSCRIPT_FILE) || name == OsStr::new(LIVE_TRANSCRIPT_FILE) {
            if !checked_direct_logs {
                checked_direct_logs = true;
                visit_logs_directory(source_root, Path::new(""), directory, &mut on_selected)?;
            }
            continue;
        }
        let child_path = source_root.join(&name);
        let child_relative = PathBuf::from(&name);
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            Err(error) if error.is_ignorable_entry_err() => continue,
            Err(error) => return Err(E::from(error)),
        };
        let OpenedProviderSourcePath::Directory(child_dir) = opened else {
            continue;
        };
        if name == OsStr::new(SYSTEM_GENERATED_DIR) {
            count_directory_with::<E>(budget)?;
            visit_system_generated_directory(
                &child_path,
                &child_relative,
                &child_dir,
                budget,
                &mut on_selected,
            )?;
            child_dir.revalidate_same_object().map_err(E::from)?;
            continue;
        }
        count_directory_with::<E>(budget)?;
        visit_session_directory(
            &child_path,
            &child_relative,
            &child_dir,
            budget,
            &mut on_selected,
        )?;
        child_dir.revalidate_same_object().map_err(E::from)?;
    }

    if strict_root_revalidation {
        directory.revalidate().map_err(E::from)?;
    } else {
        directory.revalidate_same_object().map_err(E::from)?;
    }
    Ok(())
}

/// Enumerates every selected Antigravity transcript path under `root` for
/// opening/terminal membership observation without descending into unrelated
/// subtrees such as `.system_generated/steps/<n>`.
pub(super) fn enumerate_selected_paths(root: &Path) -> Result<Vec<PathBuf>> {
    match open_provider_source_path(root)? {
        OpenedProviderSourcePath::File(file) => {
            file.revalidate_same_object()?;
            if super::super::dialect::native_jsonl_file_is_selected(
                CaptureProvider::Antigravity,
                root,
                root,
                false,
            ) {
                Ok(vec![root.to_path_buf()])
            } else {
                Ok(Vec::new())
            }
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let mut budget = DirectJsonlInventoryBudget::default();
            let mut paths = Vec::new();
            visit_antigravity_root(root, &directory, &mut budget, false, |path, _, file| {
                file.revalidate_same_object()?;
                paths.push(path);
                Ok(())
            })?;
            Ok(paths)
        }
    }
}

fn visit_session_directory<M, E>(
    session_path: &Path,
    session_relative: &Path,
    session_dir: &ProviderSourceDirectory<M>,
    budget: &mut DirectJsonlInventoryBudget,
    on_selected: &mut impl FnMut(
        PathBuf,
        PathBuf,
        OpenedProviderSourceFile<M>,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    M: AntigravitySourceError + From<SourceIoError>,
    E: From<M> + From<SourceIoError>,
{
    let sysgen_name = OsStr::new(SYSTEM_GENERATED_DIR);
    let opened = match session_dir.open_child(sysgen_name) {
        Ok(opened) => opened,
        Err(error) if error.is_not_found_err() || error.is_ignorable_entry_err() => {
            return Ok(());
        }
        Err(error) => return Err(E::from(error)),
    };
    let OpenedProviderSourcePath::Directory(sysgen_dir) = opened else {
        return Ok(());
    };
    count_directory_with::<E>(budget)?;
    visit_system_generated_directory(
        &session_path.join(sysgen_name),
        &session_relative.join(sysgen_name),
        &sysgen_dir,
        budget,
        on_selected,
    )?;
    sysgen_dir.revalidate_same_object().map_err(E::from)?;
    Ok(())
}

fn visit_system_generated_directory<M, E>(
    sysgen_path: &Path,
    sysgen_relative: &Path,
    sysgen_dir: &ProviderSourceDirectory<M>,
    budget: &mut DirectJsonlInventoryBudget,
    on_selected: &mut impl FnMut(
        PathBuf,
        PathBuf,
        OpenedProviderSourceFile<M>,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    M: AntigravitySourceError + From<SourceIoError>,
    E: From<M> + From<SourceIoError>,
{
    let logs_name = OsStr::new(LOGS_DIR);
    let opened = match sysgen_dir.open_child(logs_name) {
        Ok(opened) => opened,
        Err(error) if error.is_not_found_err() || error.is_ignorable_entry_err() => {
            return Ok(());
        }
        Err(error) => return Err(E::from(error)),
    };
    let OpenedProviderSourcePath::Directory(logs_dir) = opened else {
        return Ok(());
    };
    count_directory_with::<E>(budget)?;
    visit_logs_directory(
        &sysgen_path.join(logs_name),
        &sysgen_relative.join(logs_name),
        &logs_dir,
        on_selected,
    )?;
    logs_dir.revalidate_same_object().map_err(E::from)?;
    Ok(())
}

fn visit_logs_directory<M, E>(
    logs_path: &Path,
    logs_relative: &Path,
    logs_dir: &ProviderSourceDirectory<M>,
    on_selected: &mut impl FnMut(
        PathBuf,
        PathBuf,
        OpenedProviderSourceFile<M>,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    M: AntigravitySourceError + From<SourceIoError>,
    E: From<M>,
{
    let full_name = OsStr::new(FULL_TRANSCRIPT_FILE);
    match logs_dir.open_child(full_name) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            return on_selected(
                logs_path.join(full_name),
                logs_relative.join(full_name),
                file,
            );
        }
        Ok(OpenedProviderSourcePath::Directory(_)) => {}
        Err(error) if error.is_not_found_err() => {}
        Err(error) => return Err(E::from(error)),
    }

    let live_name = OsStr::new(LIVE_TRANSCRIPT_FILE);
    match logs_dir.open_child(live_name) {
        Ok(OpenedProviderSourcePath::File(file)) => on_selected(
            logs_path.join(live_name),
            logs_relative.join(live_name),
            file,
        ),
        Ok(OpenedProviderSourcePath::Directory(_)) => Ok(()),
        Err(error) if error.is_not_found_err() => Ok(()),
        Err(error) => Err(E::from(error)),
    }
}

#[cfg(test)]
fn count_directory(budget: &mut DirectJsonlInventoryBudget) -> Result<()> {
    count_directory_with(budget)
}

fn count_directory_with<E: From<SourceIoError>>(
    budget: &mut DirectJsonlInventoryBudget,
) -> std::result::Result<(), E> {
    budget.directories = budget.directories.saturating_add(1);
    if budget.directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
        return Err(E::from(
            SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::Directories,
                maximum: PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
                observed: budget.directories,
            },
        ));
    }
    Ok(())
}

fn count_metadata_entries_with<E: From<SourceIoError>>(
    budget: &mut DirectJsonlInventoryBudget,
    count: usize,
) -> std::result::Result<(), E> {
    budget.metadata_entries = budget.metadata_entries.saturating_add(count);
    if budget.metadata_entries > PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES {
        return Err(E::from(
            SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::MetadataEntries,
                maximum: PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
                observed: budget.metadata_entries,
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_jsonl::JsonlFamilyAdapter;

    use super::*;
    use crate::test_support::NativeJsonlTestRuntime;
    use crate::test_support_paths::tempdir;

    fn write_transcript(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("{{\"type\":\"user\",\"content\":{content:?},\"created_at\":\"2026-08-24T12:00:00Z\"}}\n"),
        )
        .unwrap();
    }

    #[test]
    fn antigravity_prunes_step_subtrees_and_preserves_directory_bound() {
        let temp = tempdir().unwrap();
        let brain = temp.path().join("brain");
        let session = brain.join("session-001");
        let transcript = session.join(".system_generated/logs/transcript.jsonl");
        write_transcript(&transcript, "hello antigravity");

        // Create step and scratch subdirectories under the session; none of
        // them may be visited or counted against the directory inventory bound.
        let steps = session.join(".system_generated/steps");
        fs::create_dir_all(&steps).unwrap();
        for index in 0..256 {
            fs::create_dir(steps.join(format!("{index}"))).unwrap();
        }
        fs::create_dir_all(session.join("scratch/nested/deep")).unwrap();
        fs::write(
            session.join("scratch/nested/deep/transcript.jsonl"),
            b"{\"type\":\"user\",\"content\":\"ignored\"}\n",
        )
        .unwrap();

        let OpenedProviderSourcePath::Directory(root_dir) =
            open_provider_source_path::<CaptureError>(&brain).unwrap()
        else {
            panic!("brain root must open as directory");
        };
        let mut budget = DirectJsonlInventoryBudget::default();
        let mut selected = Vec::new();
        visit_antigravity_root(&brain, &root_dir, &mut budget, true, |path, _, _| {
            selected.push(path);
            Ok(())
        })
        .unwrap();
        assert_eq!(selected, vec![transcript]);
        assert_eq!(
            budget.directories, 4,
            "only brain, session, .system_generated, and logs should be opened"
        );
        assert_eq!(budget.metadata_entries, 1);

        // Verify the directory inventory DoS bound itself remains enforced.
        let mut maxed = DirectJsonlInventoryBudget {
            directories: PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
            metadata_entries: 0,
        };
        assert!(matches!(
            count_directory(&mut maxed),
            Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::Directories,
                ..
            })
        ));

        let adapter = antigravity_source_backed_adapter::<NativeJsonlTestRuntime>();
        let inventory = adapter.discover(&brain).unwrap();
        assert_eq!(inventory.accepted_len(), 1);
        assert_eq!(inventory.quarantined_leaves().count(), 0);

        let opening = adapter
            .observe_terminal_membership(&brain, &inventory)
            .unwrap();
        let terminal = adapter
            .observe_terminal_membership(&brain, &inventory)
            .unwrap();
        assert_eq!(opening.unbound_routes().count(), 0);
        assert_eq!(terminal.unbound_routes().count(), 0);
    }

    #[test]
    fn antigravity_membership_detects_new_and_upgraded_transcripts() {
        let temp = tempdir().unwrap();
        let brain = temp.path().join("brain");
        let live = brain.join("session-a/.system_generated/logs/transcript.jsonl");
        write_transcript(&live, "live");

        let adapter = antigravity_source_backed_adapter::<NativeJsonlTestRuntime>();
        let inventory = adapter.discover(&brain).unwrap();
        assert_eq!(
            enumerate_selected_paths(&brain).unwrap(),
            vec![live.clone()]
        );

        // Upgrading live transcript to transcript_full.jsonl replaces the route.
        let full = brain.join("session-a/.system_generated/logs/transcript_full.jsonl");
        write_transcript(&full, "full");
        assert_eq!(
            enumerate_selected_paths(&brain).unwrap(),
            vec![full.clone()]
        );

        // Adding a second session appears in selected membership as an unbound route.
        let second = brain.join("session-b/.system_generated/logs/transcript.jsonl");
        write_transcript(&second, "second");
        let observed = adapter
            .observe_terminal_membership(&brain, &inventory)
            .unwrap();
        let unbound: Vec<_> = observed
            .unbound_routes()
            .map(|(path, _, _)| path.to_path_buf())
            .collect();
        assert!(unbound.contains(&full));
        assert!(unbound.contains(&second));
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_selected_transcript_symlink_fails_closed() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let outside = temp.path().join("outside.jsonl");
        write_transcript(&outside, "secret");
        let brain = temp.path().join("brain");
        let logs = brain.join("session-link/.system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        symlink(&outside, logs.join("transcript_full.jsonl")).unwrap();
        write_transcript(&logs.join("transcript.jsonl"), "live");

        let adapter = antigravity_source_backed_adapter::<NativeJsonlTestRuntime>();
        assert!(adapter.discover(&brain).is_err());
        assert!(enumerate_selected_paths(&brain).is_err());
    }
}

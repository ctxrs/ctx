//! Inventory only Prime's selected flat sessions and finite artifact/T3 layouts.

use super::*;
use ctx_history_source_io::{
    is_non_regular_source_rejection, is_symlink_source_rejection, open_provider_source_path,
    OpenedProviderSourcePath, ProviderSourceDirectory,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
};
use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Layout {
    Sessions,
    Artifacts,
    Children,
    T3,
    Instance,
}

pub(super) fn session_filename(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && !matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("semantic-edges.jsonl" | "rlm-subagents.jsonl" | "rlm-ledger.jsonl")
        )
}

pub(super) fn paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    match open_provider_source_path(root)? {
        OpenedProviderSourcePath::File(_) => {
            if !session_filename(root) {
                return Err(invalid("Prime Agent input is not a session transcript"));
            }
            paths.push(root.to_path_buf());
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let mut remaining = PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES;
            let layout = if root.ends_with(".t3/userdata/prime-agent") {
                Layout::T3
            } else if root
                .file_name()
                .is_some_and(|name| name == "session-artifacts")
            {
                Layout::Artifacts
            } else {
                Layout::Sessions
            };
            walk(root, &directory, layout, &mut remaining, &mut paths)?;
            directory.authority_root().revalidate()?;
        }
    }
    Ok(paths)
}

fn walk(
    path: &Path,
    directory: &ProviderSourceDirectory,
    layout: Layout,
    remaining: &mut usize,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let names = directory.entries_snapshot(*remaining)?;
    *remaining = remaining.saturating_sub(names.len());
    for name in names {
        let path = path.join(&name);
        if matches!(layout, Layout::Sessions) {
            // Do not open unrelated files or descend into project directories.
            // Selected files are admitted independently by the adapter, so one
            // unreadable transcript cannot hide its readable neighbors.
            if session_filename(&path) {
                paths.push(path);
            }
            continue;
        }
        if matches!(layout, Layout::Children)
            && !name
                .to_str()
                .is_some_and(|name| name.starts_with("sub-") && name.len() > 4)
        {
            continue;
        }
        let child = match directory.open_child(&name) {
            Ok(OpenedProviderSourcePath::Directory(child)) => child,
            Ok(OpenedProviderSourcePath::File(_)) => continue,
            Err(error)
                if is_symlink_source_rejection(&error)
                    || is_non_regular_source_rejection(&error) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        };
        let next = match layout {
            Layout::Artifacts => Layout::Children,
            Layout::Children => Layout::Sessions,
            Layout::T3 => Layout::Instance,
            Layout::Instance if name == "session-artifacts" => Layout::Artifacts,
            Layout::Instance => Layout::Sessions,
            Layout::Sessions => unreachable!(),
        };
        walk(&path, &child, next, remaining, paths)?;
    }
    directory.revalidate().map_err(CaptureError::from)
}

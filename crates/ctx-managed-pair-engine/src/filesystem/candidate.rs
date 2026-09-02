use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context as _, Result};

use super::{ensure_directory, Entry, Layout, Slot};

pub(super) const APPLY_CANDIDATE_DIRECTORY: &str = ".managed-pair-apply-v1";

pub(crate) fn apply_candidate_root(install_root: &Path) -> PathBuf {
    install_root
        .join("share/ctx")
        .join(APPLY_CANDIDATE_DIRECTORY)
}

pub(crate) fn create_apply_candidate(install_root: &Path) -> Result<PathBuf> {
    let root = apply_candidate_root(install_root);
    ensure_directory(&root)?;
    Layout::open(&root, true)?;
    Ok(root)
}

pub(crate) fn apply_candidate_exists(layout: &Layout) -> Result<bool> {
    match layout.open_apply_candidate() {
        Ok(_) => Ok(true),
        Err(error) if is_not_found(&error) => Ok(false),
        Err(error) => Err(error).context("inspect managed-pair apply candidate"),
    }
}

pub(crate) fn remove_apply_candidate(layout: &Layout) -> Result<()> {
    if !apply_candidate_exists(layout)? {
        return Ok(());
    }
    let candidate = layout.open_apply_candidate()?;
    for slot in Slot::ALL {
        let entry = candidate.target(slot);
        match entry.directory.entry_metadata(&entry.name, entry.path())? {
            Some(metadata) if metadata.is_file || metadata.is_symlink => {
                entry.directory.remove_file(&entry.name, entry.path())?;
                entry.directory.sync()?;
            }
            Some(_) => bail!("managed-pair candidate {} is unsafe", slot.label()),
            None => {}
        }
    }
    candidate
        .share_directory
        .remove_directory(OsStr::new("ctx"))?;
    candidate
        .root_directory
        .remove_directory(OsStr::new("share"))?;
    candidate
        .root_directory
        .remove_directory(OsStr::new("libexec"))?;
    candidate
        .root_directory
        .remove_directory(OsStr::new("bin"))?;
    layout
        .ctx_directory
        .remove_directory(OsStr::new(APPLY_CANDIDATE_DIRECTORY))?;
    layout.ctx_directory.sync()
}

pub(super) fn transaction_sibling(target: &Entry, attempt_id: &str) -> Entry {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("managed-pair");
    target.sibling(format!(".{name}.managed-pair-{attempt_id}.new").into())
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

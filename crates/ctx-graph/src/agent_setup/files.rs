use super::*;

pub(super) fn check_parents(path: &Path) -> Result<()> {
    for parent in path.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(m) => ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "directory must not be a symlink: {}",
                parent.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    check_parents(path)?;
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "expected regular file: {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LIMIT * 16 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT * 16,
        "file exceeds size limit: {}",
        path.display()
    );
    Ok(Some(bytes))
}

pub(super) fn atomic(
    path: &Path,
    bytes: &[u8],
    expected: Option<&[u8]>,
    source: Option<&Path>,
    executable: bool,
) -> Result<()> {
    ensure!(
        read(path)?.as_deref() == expected,
        "file changed; refusing to replace {}",
        path.display()
    );
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    check_parents(path)?;
    let staging = tempfile::tempdir_in(parent)?;
    switch_files::protect(staging.path())?;
    let mut temp = tempfile::NamedTempFile::new_in(staging.path())?;
    switch_files::protect(temp.path())?;
    if let Some(source) = source.or_else(|| expected.map(|_| path)) {
        switch_files::preserve_permissions(temp.as_file(), source)?;
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mode = temp.as_file().metadata()?.permissions().mode();
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode | 0o100))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    ensure!(
        read(path)?.as_deref() == expected,
        "file changed during setup: {}",
        path.display()
    );
    switch_files::replace(temp, path, expected.is_some())
}

pub(super) fn planned(path: PathBuf, after: Vec<u8>, dedicated: bool) -> Result<Change> {
    let before = read(&path)?;
    ensure!(
        !dedicated || before.is_none(),
        "unowned file already exists: {}",
        path.display()
    );
    ensure!(after.len() as u64 <= LIMIT, "configuration exceeds 8 MiB");
    Ok(Change {
        path,
        before,
        after,
        permission_source: None,
        executable: false,
        previous_after: None,
    })
}

pub(super) fn edited(path: PathBuf, before: Option<Vec<u8>>, after: Vec<u8>) -> Result<Change> {
    let change = planned(path, after, false)?;
    ensure!(
        change.before == before,
        "file changed while planning installation"
    );
    Ok(change)
}

pub(super) fn receipt_path(scope: &Path, host: &str, component: &str) -> PathBuf {
    scope
        .join(".graf/ctx-setup")
        .join(format!("{host}-{component}.json"))
}

pub(super) fn load(path: &Path, scope: &Path, allowed: &[PathBuf]) -> Result<Option<Receipt>> {
    let Some(bytes) = read(path)? else {
        return Ok(None);
    };
    let receipt: Receipt = serde_json::from_slice(&bytes).context("invalid setup receipt")?;
    ensure!(
        receipt.version == 1 && receipt.scope == scope && !receipt.changes.is_empty(),
        "setup receipt scope/version mismatch"
    );
    let mut paths = std::collections::BTreeSet::new();
    for change in &receipt.changes {
        ensure!(
            allowed.contains(&change.path) && paths.insert(&change.path),
            "receipt contains unexpected or duplicate destination"
        );
        ensure!(
            change
                .permission_source
                .as_ref()
                .is_none_or(|s| allowed.contains(s)),
            "receipt contains unexpected permissions source"
        );
    }
    Ok(Some(receipt))
}
pub(super) fn recorded(change: &Change, current: &Option<Vec<u8>>) -> bool {
    current.as_deref() == Some(change.after.as_slice())
        || *current == change.before
        || change
            .previous_after
            .as_ref()
            .is_some_and(|old| current.as_deref() == Some(old.as_slice()))
}

pub(super) fn verify(receipt: &Receipt) -> Result<()> {
    for change in &receipt.changes {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed after installation; refusing to modify {}",
            change.path.display()
        );
        if current.as_deref() == Some(change.after.as_slice()) {
            ensure!(
                executable_matches(change)?,
                "hook permissions changed after installation: {}",
                change.path.display()
            );
        }
    }
    Ok(())
}

pub(super) fn executable_matches(change: &Change) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if change.executable {
            return Ok(fs::metadata(&change.path)?.permissions().mode() & 0o100 != 0);
        }
    }
    #[cfg(not(unix))]
    let _ = change;
    Ok(true)
}

// A persisted receipt precedes changes. Interrupted operations can be resumed or
// uninstalled; no unrecorded overwrite is needed to recover.
pub(super) fn apply(path: &Path, receipt: &Receipt) -> Result<bool> {
    verify(receipt)?;
    let mut changed = false;
    let saved = read(path)?;
    let bytes = serde_json::to_vec(receipt)?;
    if saved.as_deref() != Some(bytes.as_slice()) {
        ensure!(
            bytes.len() as u64 <= LIMIT * 16,
            "setup receipt exceeds size limit"
        );
        atomic(path, &bytes, saved.as_deref(), None, false)?;
        changed = true;
    }
    for change in &receipt.changes {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed during setup: {}",
            change.path.display()
        );
        if current.as_deref() != Some(change.after.as_slice()) {
            atomic(
                &change.path,
                &change.after,
                current.as_deref(),
                change.permission_source.as_deref(),
                change.executable,
            )?;
            changed = true;
        }
    }
    if receipt.changes.iter().any(|c| c.previous_after.is_some()) {
        let mut completed = receipt.clone();
        for change in &mut completed.changes {
            change.previous_after = None;
        }
        atomic(
            path,
            &serde_json::to_vec(&completed)?,
            Some(&bytes),
            None,
            false,
        )?;
    }
    Ok(changed)
}

pub(super) fn undo(path: &Path, receipt: &Receipt) -> Result<()> {
    verify(receipt)?;
    for change in receipt.changes.iter().rev() {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed during uninstall: {}",
            change.path.display()
        );
        if current == change.before {
            continue;
        }
        if let Some(before) = &change.before {
            // Hook originals remain in their sibling backup until the hook has
            // been restored. Use that backup to recover its exact access ACL.
            let backup = change.path.with_file_name(format!(
                "{}.ctx-graph-original",
                change.path.file_name().unwrap().to_string_lossy()
            ));
            let source = receipt
                .changes
                .iter()
                .any(|c| c.path == backup)
                .then_some(backup);
            atomic(
                &change.path,
                before,
                current.as_deref(),
                source.as_deref(),
                false,
            )?;
        } else {
            ensure!(
                read(&change.path)? == current,
                "file changed during uninstall"
            );
            fs::remove_file(&change.path)?;
        }
    }
    fs::remove_file(path)?;
    Ok(())
}

pub(super) struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

pub(super) fn lock(scope: &Path, installing: bool) -> Result<Lock> {
    let directory = scope.join(".graf/ctx-setup");
    check_parents(&directory.join("record"))?;
    fs::create_dir_all(&directory)?;
    // Only this dedicated receipt directory is made private, never the project.
    switch_files::protect(&directory)?;
    let path = directory.join("lock");
    fs::create_dir(&path).context("another setup operation is active; if interrupted, remove .graf/ctx-setup/lock before retrying")?;
    let lock = Lock(path);
    if installing {
        let ignore = directory.join(".gitignore");
        if read(&ignore)?.is_none() {
            atomic(&ignore, b"*\n", None, None, false)?;
        }
        // Keep databases and receipts out of normal git add, even before indexing.
        let ignore = scope.join(".graf/.gitignore");
        if read(&ignore)?.is_none() {
            atomic(&ignore, b"*\n", None, None, false)?;
        }
    }
    Ok(lock)
}

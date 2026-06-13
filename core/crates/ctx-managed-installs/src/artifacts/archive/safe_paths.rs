use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(super) fn normalize_archive_entry_path_allowing_empty(
    raw_path: &Path,
    label: &str,
) -> Result<PathBuf> {
    let raw_display = raw_path.display().to_string();
    if raw_display.contains('\\') {
        anyhow::bail!("{label} must not contain backslashes: {raw_display}");
    }

    let mut normalized = PathBuf::new();
    for component in raw_path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::bail!("{label} must not contain parent directory segments: {raw_display}");
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("{label} must be relative: {raw_display}");
            }
        }
    }
    Ok(normalized)
}

fn normalize_archive_entry_path(raw_path: &Path, label: &str) -> Result<PathBuf> {
    let normalized = normalize_archive_entry_path_allowing_empty(raw_path, label)?;
    if normalized.as_os_str().is_empty() {
        anyhow::bail!("{label} is empty");
    }
    Ok(normalized)
}

pub(super) fn safe_archive_dest(out_dir: &Path, raw_path: &Path, label: &str) -> Result<PathBuf> {
    Ok(out_dir.join(normalize_archive_entry_path(raw_path, label)?))
}

pub(super) fn ensure_archive_root(out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    std::fs::canonicalize(out_dir).with_context(|| format!("canonicalize {}", out_dir.display()))
}

fn ensure_canonical_path_inside_root(root: &Path, path: &Path, label: &str) -> Result<()> {
    let canonical =
        std::fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))?;
    if !canonical.starts_with(root) {
        anyhow::bail!(
            "{label} escaped extraction root: {} -> {}",
            path.display(),
            canonical.display()
        );
    }
    Ok(())
}

fn canonical_path_if_exists(path: &Path) -> Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound || is_filesystem_loop_error(&err) =>
        {
            Ok(None)
        }
        Err(err) => Err(err).with_context(|| format!("canonicalize {}", path.display())),
    }
}

fn is_filesystem_loop_error(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(err.raw_os_error(), Some(40) | Some(62))
    }
    #[cfg(not(unix))]
    {
        let _ = err;
        false
    }
}

fn reject_existing_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "archive extraction refused to write through symlink: {}",
                path.display()
            )
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

fn lexical_dest_path_from_canonical_parent(root: &Path, dest: &Path) -> Result<PathBuf> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("archive destination has no parent: {}", dest.display()))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .with_context(|| format!("canonicalize destination parent {}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        anyhow::bail!(
            "archive destination parent escaped extraction root: {} -> {}",
            parent.display(),
            canonical_parent.display()
        );
    }
    let file_name = dest.file_name().ok_or_else(|| {
        anyhow::anyhow!("archive destination has no file name: {}", dest.display())
    })?;
    Ok(canonical_parent.join(file_name))
}

fn root_filesystem_is_ascii_case_insensitive(root: &Path) -> Result<bool> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16 {
        let base = format!(
            ".ctx-case-probe-{}-{timestamp}-{attempt}",
            std::process::id()
        );
        let upper = root.join(format!("{base}-A"));
        let lower = root.join(format!("{base}-a"));
        if upper.exists() || lower.exists() {
            continue;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&upper)
        {
            Ok(_) => {
                let is_case_insensitive = std::fs::metadata(&lower).is_ok();
                std::fs::remove_file(&upper).with_context(|| {
                    format!("remove case-sensitivity probe {}", upper.display())
                })?;
                return Ok(is_case_insensitive);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("create case-sensitivity probe {}", upper.display()));
            }
        }
    }
    anyhow::bail!(
        "failed to create case-sensitivity probe under {}",
        root.display()
    );
}

fn paths_equal_for_extraction_root(root: &Path, left: &Path, right: &Path) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    if !left
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
    {
        return Ok(false);
    }
    root_filesystem_is_ascii_case_insensitive(root)
}

fn prepare_existing_symlink_for_file_write(root: &Path, path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(path)
                .with_context(|| format!("read symlink target {}", path.display()))?;
            let target_resolved = validate_symlink_target(root, path, &target)?;
            if canonical_path_if_exists(path)?.is_some() {
                ensure_canonical_path_inside_root(root, path, "archive file symlink target")?;
                return Ok(());
            }
            let dest_resolved = lexical_dest_path_from_canonical_parent(root, path)?;
            if paths_equal_for_extraction_root(root, &target_resolved, &dest_resolved)? {
                std::fs::remove_file(path).with_context(|| {
                    format!("remove self-referential symlink {}", path.display())
                })?;
            }
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

fn ensure_existing_ancestors_inside_root(root: &Path, out_dir: &Path, dest: &Path) -> Result<()> {
    let rel = dest
        .strip_prefix(out_dir)
        .with_context(|| format!("archive destination escaped root: {}", dest.display()))?;
    let Some(parent) = rel.parent() else {
        return Ok(());
    };

    let mut current = out_dir.to_path_buf();
    for component in parent.components() {
        match component {
            Component::Normal(segment) => {
                current.push(segment);
                match std::fs::symlink_metadata(&current) {
                    Ok(_) => ensure_canonical_path_inside_root(root, &current, "archive ancestor")?,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
                    Err(err) => {
                        return Err(err).with_context(|| format!("stat {}", current.display()))
                    }
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "archive destination has unsafe ancestor: {}",
                    dest.display()
                );
            }
        }
    }
    Ok(())
}

fn prepare_archive_entry_parent(root: &Path, out_dir: &Path, dest: &Path) -> Result<()> {
    ensure_existing_ancestors_inside_root(root, out_dir, dest)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        ensure_canonical_path_inside_root(root, parent, "archive parent")?;
    }
    ensure_existing_ancestors_inside_root(root, out_dir, dest)?;
    Ok(())
}

pub(super) fn create_archive_dir(root: &Path, out_dir: &Path, dest: &Path) -> Result<()> {
    prepare_archive_entry_parent(root, out_dir, dest)?;
    reject_existing_symlink(dest)?;
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    reject_existing_symlink(dest)?;
    ensure_canonical_path_inside_root(root, dest, "archive directory")?;
    Ok(())
}

fn validate_symlink_target(root: &Path, dest: &Path, target: &Path) -> Result<PathBuf> {
    let target_display = target.display().to_string();
    if target_display.is_empty() {
        anyhow::bail!("archive symlink target is empty for {}", dest.display());
    }
    if target_display.contains('\\') {
        anyhow::bail!("archive symlink target must not contain backslashes: {target_display}");
    }

    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("archive symlink has no parent: {}", dest.display()))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .with_context(|| format!("canonicalize symlink parent {}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        anyhow::bail!(
            "archive symlink parent escaped extraction root: {} -> {}",
            parent.display(),
            canonical_parent.display()
        );
    }

    let parent_rel = canonical_parent.strip_prefix(root).with_context(|| {
        format!(
            "archive symlink parent escaped extraction root: {}",
            canonical_parent.display()
        )
    })?;
    let mut resolved = root.to_path_buf();
    for component in parent_rel.components() {
        match component {
            Component::Normal(segment) => resolved.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "archive symlink parent escaped extraction root: {}",
                    canonical_parent.display()
                );
            }
        }
    }

    for component in target.components() {
        match component {
            Component::Normal(segment) => resolved.push(segment),
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved == root || !resolved.pop() || !resolved.starts_with(root) {
                    anyhow::bail!(
                        "archive symlink target escapes extraction root: {} -> {}",
                        dest.display(),
                        target.display()
                    );
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "archive symlink target must be relative: {} -> {}",
                    dest.display(),
                    target.display()
                );
            }
        }
    }
    if !resolved.starts_with(root) {
        anyhow::bail!(
            "archive symlink target escapes extraction root: {} -> {}",
            dest.display(),
            target.display()
        );
    }
    ensure_existing_ancestors_inside_root(root, root, &resolved)?;
    if canonical_path_if_exists(&resolved)?.is_some() {
        ensure_canonical_path_inside_root(root, &resolved, "archive symlink target")?;
    }
    Ok(resolved)
}

fn symlink_targets_resolve_to_same_existing_path(left: &Path, right: &Path) -> Result<bool> {
    let Some(left) = canonical_path_if_exists(left)? else {
        return Ok(false);
    };
    let Some(right) = canonical_path_if_exists(right)? else {
        return Ok(false);
    };
    Ok(left == right)
}

pub(super) fn create_archive_symlink(
    root: &Path,
    out_dir: &Path,
    dest: &Path,
    target: &Path,
) -> Result<()> {
    prepare_archive_entry_parent(root, out_dir, dest)?;
    if let Ok(metadata) = std::fs::symlink_metadata(dest) {
        if metadata.file_type().is_symlink() {
            let existing_target = std::fs::read_link(dest)
                .with_context(|| format!("read symlink target {}", dest.display()))?;
            let existing_resolved = validate_symlink_target(root, dest, &existing_target)?;
            let target_resolved = validate_symlink_target(root, dest, target)?;
            if existing_target == target
                || paths_equal_for_extraction_root(root, &existing_resolved, &target_resolved)?
                || symlink_targets_resolve_to_same_existing_path(
                    &existing_resolved,
                    &target_resolved,
                )?
            {
                return Ok(());
            }
        }
        anyhow::bail!(
            "archive extraction refused to replace existing path with symlink: {}",
            dest.display()
        );
    }
    validate_symlink_target(root, dest, target)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dest).with_context(|| {
            format!("create symlink {} -> {}", dest.display(), target.display())
        })?;
        if canonical_path_if_exists(dest)?.is_some() {
            ensure_canonical_path_inside_root(root, dest, "archive symlink target")?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (dest, target);
        anyhow::bail!("archive symlink entries are not supported on this platform");
    }
}

pub(super) fn create_archive_file<R: Read>(
    root: &Path,
    out_dir: &Path,
    dest: &Path,
    reader: &mut R,
    mode: Option<u32>,
) -> Result<()> {
    prepare_archive_entry_parent(root, out_dir, dest)?;
    prepare_existing_symlink_for_file_write(root, dest)?;
    let mut out =
        std::fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    std::io::copy(reader, &mut out).context("extract archive entry")?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode & 0o777))
            .with_context(|| format!("chmod {}", dest.display()))?;
    }
    Ok(())
}

pub(super) fn create_archive_hardlink(
    root: &Path,
    out_dir: &Path,
    dest: &Path,
    target: &Path,
) -> Result<()> {
    let target_dest = safe_archive_dest(out_dir, target, "tar hardlink target")?;
    ensure_canonical_path_inside_root(root, &target_dest, "tar hardlink target")?;
    let target_metadata = std::fs::symlink_metadata(&target_dest)
        .with_context(|| format!("stat tar hardlink target {}", target_dest.display()))?;
    if target_metadata.file_type().is_symlink() {
        anyhow::bail!(
            "archive hardlink target must not be a symlink: {}",
            target.display()
        );
    }
    if !target_metadata.file_type().is_file() {
        anyhow::bail!(
            "archive hardlink target must be a file: {}",
            target.display()
        );
    }

    prepare_archive_entry_parent(root, out_dir, dest)?;
    reject_existing_symlink(dest)?;
    if std::fs::symlink_metadata(dest).is_ok() {
        anyhow::bail!(
            "archive extraction refused to replace existing path with hardlink: {}",
            dest.display()
        );
    }
    std::fs::hard_link(&target_dest, dest).with_context(|| {
        format!(
            "create hardlink {} -> {}",
            dest.display(),
            target_dest.display()
        )
    })?;
    Ok(())
}

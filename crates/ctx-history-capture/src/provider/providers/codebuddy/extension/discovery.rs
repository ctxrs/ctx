use std::{fs, path::Path};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::Result;

fn codebuddy_is_session_dir(path: &Path) -> bool {
    codebuddy_is_regular_file(&path.join("index.json"))
        && codebuddy_is_directory(&path.join("messages"))
}

fn codebuddy_is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn codebuddy_is_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

pub(super) fn visit_codebuddy_extension_sessions(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.file_name().and_then(|name| name.to_str()) != Some("index.json") {
            return Ok(0);
        }
        let Some(parent) = root.parent() else {
            return Ok(0);
        };
        if codebuddy_is_session_dir(parent) {
            visit(parent)?;
            return Ok(1);
        }
        return visit_codebuddy_project_sessions(parent, visit);
    }
    if !metadata.file_type().is_dir() {
        return Ok(0);
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if codebuddy_is_session_dir(root) {
        visit(root)?;
        return Ok(1);
    }

    let mut visited = visit_codebuddy_project_sessions(root, visit)?;
    if root.file_name().and_then(|name| name.to_str()) == Some("history") {
        return Ok(visited.saturating_add(visit_codebuddy_history_root(root, visit)?));
    }
    let mut inspected = 0_usize;
    visited = visited.saturating_add(visit_nested_codebuddy_history_roots(
        root,
        0,
        &mut inspected,
        visit,
    )?);
    Ok(visited)
}

fn visit_codebuddy_project_sessions(
    project_dir: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let mut visited = 0_usize;
    let Ok(entries) = fs::read_dir(project_dir) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && codebuddy_is_session_dir(&entry.path()) {
            visit(&entry.path())?;
            visited = visited.saturating_add(1);
        }
    }
    Ok(visited)
}

fn visit_codebuddy_history_root(
    history_dir: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let mut visited = 0_usize;
    let Ok(entries) = fs::read_dir(history_dir) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            visited =
                visited.saturating_add(visit_codebuddy_project_sessions(&entry.path(), visit)?);
        }
    }
    Ok(visited)
}

fn visit_nested_codebuddy_history_roots(
    dir: &Path,
    depth: usize,
    inspected: &mut usize,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    if depth > 8 || *inspected >= 20_000 {
        return Ok(0);
    }
    let mut visited = 0_usize;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        *inspected = inspected.saturating_add(1);
        if *inspected > 20_000 {
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()) == Some("history") {
            visited = visited.saturating_add(visit_codebuddy_history_root(&path, visit)?);
        } else {
            visited = visited.saturating_add(visit_nested_codebuddy_history_roots(
                &path,
                depth.saturating_add(1),
                inspected,
                visit,
            )?);
        }
    }
    Ok(visited)
}

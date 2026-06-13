use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use crate::patch;

#[derive(Debug)]
pub(super) struct JjStatusEntry {
    pub(super) status: char,
    pub(super) path: String,
}

#[derive(Debug)]
pub(super) struct JjUntrackedEntry {
    pub(super) path: String,
    pub(super) is_dir: bool,
}

#[derive(Debug)]
pub(super) struct JjStatusParsed {
    pub(super) entries: Vec<JjStatusEntry>,
    pub(super) untracked: Vec<JjUntrackedEntry>,
}

pub(super) fn parse_jj_status_output(output: &str) -> JjStatusParsed {
    let mut entries = Vec::new();
    let mut untracked = Vec::new();
    for line in output.lines() {
        let line = line.trim_end();
        if line.len() < 3 {
            continue;
        }
        let mut chars = line.chars();
        let status = chars.next().unwrap_or(' ');
        if chars.next() != Some(' ') {
            continue;
        }
        let mut path = chars.as_str().trim().to_string();
        if path.is_empty() {
            continue;
        }
        match status {
            '?' => {
                let mut is_dir = false;
                if path.ends_with('/') || path.ends_with('\\') {
                    is_dir = true;
                    path = path.trim_end_matches(&['/', '\\'][..]).to_string();
                }
                if !path.is_empty() {
                    untracked.push(JjUntrackedEntry { path, is_dir });
                }
            }
            'M' | 'A' | 'D' | 'R' | 'C' => {
                entries.push(JjStatusEntry { status, path });
            }
            _ => {}
        }
    }
    JjStatusParsed { entries, untracked }
}

pub(super) async fn collect_jj_untracked_files(
    root: &Path,
    entries: &[JjUntrackedEntry],
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        if entry.is_dir {
            collect_jj_untracked_dir(root, &entry.path, &mut out, &mut seen).await?;
        } else if !patch::should_ignore_path(Path::new(&entry.path))
            && seen.insert(entry.path.clone())
        {
            out.push(entry.path.clone());
        }
    }
    out.sort();
    Ok(out)
}

pub(super) async fn count_jj_untracked_files(
    root: &Path,
    entries: &[JjUntrackedEntry],
) -> Result<i64> {
    let mut total = 0;
    let mut seen = HashSet::new();
    for entry in entries {
        if entry.is_dir {
            total += count_jj_untracked_dir(root, &entry.path, &mut seen).await?;
        } else if !patch::should_ignore_path(Path::new(&entry.path))
            && seen.insert(entry.path.clone())
        {
            total += 1;
        }
    }
    Ok(total)
}

async fn collect_jj_untracked_dir(
    root: &Path,
    rel_dir: &str,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let mut stack = vec![root.join(rel_dir)];
    while let Some(dir) = stack.pop() {
        let mut dir_entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            let rel = match path.strip_prefix(root) {
                Ok(rel) => rel,
                Err(_) => continue,
            };
            if patch::should_ignore_path(rel) {
                continue;
            }
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let rel_str = rel.to_string_lossy().to_string();
                if seen.insert(rel_str.clone()) {
                    out.push(rel_str);
                }
            }
        }
    }
    Ok(())
}

async fn count_jj_untracked_dir(
    root: &Path,
    rel_dir: &str,
    seen: &mut HashSet<String>,
) -> Result<i64> {
    let mut total = 0;
    let mut stack = vec![root.join(rel_dir)];
    while let Some(dir) = stack.pop() {
        let mut dir_entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            let rel = match path.strip_prefix(root) {
                Ok(rel) => rel,
                Err(_) => continue,
            };
            if patch::should_ignore_path(rel) {
                continue;
            }
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let rel_str = rel.to_string_lossy().to_string();
                if seen.insert(rel_str) {
                    total += 1;
                }
            }
        }
    }
    Ok(total)
}

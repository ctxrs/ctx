//! Bounded local Git blame under an exact executable and certified root.

use std::fs;
#[cfg(test)]
use std::path::PathBuf;
use std::path::{Component, Path};
#[cfg(test)]
use std::process::{Command, Stdio};
#[cfg(test)]
use std::time::{Duration, Instant};

use crate::git_executable::GitExecutable;
use crate::query::{
    Citation, FileBlamePosition, GitBlameWindow, GitLineObservation, LineRange, QueryError,
    ResourceId,
};
use crate::repository_path::{BoundRepositoryFile, BoundRepositoryRoot};

pub(super) use authority::{
    CertifiedLiveRoot, ExactGitBlameFile, GitBlameAuthority, RepositoryWorktreeIdentity,
};
#[cfg(test)]
use command::run_git_with_timeout;
use command::{
    MAX_GIT_IDENTITY_BYTES, MAX_GIT_OUTPUT_BYTES, git_output, git_text, require_git_success,
    run_git,
};
#[cfg(test)]
use live_root::live_root_fingerprint;
use live_root::{
    live_root_head, revalidate_live_root, unique_root, validate_geometry_fingerprint,
    validate_relative_file, validate_root, verify_git_top_level,
};

mod authority;
mod command;
mod live_root;
#[cfg(test)]
mod tests;

const BLAME_WINDOW_LINES: u32 = 500;
const BLAME_LOOKAHEAD_LINES: u32 = 1;

pub(super) fn blame_with_authority<A: GitBlameAuthority + ?Sized>(
    authority: &A,
    file_id: &ResourceId,
    requested: Option<LineRange>,
    resume: Option<&FileBlamePosition>,
) -> Result<GitBlameWindow, QueryError> {
    let git_executable = required_git_executable(authority.authorized_git_executable())?;
    let file = authority.exact_file_authority(file_id)?;
    validate_relative_file(&file.relative_path)?;
    let live_root = unique_root(authority, &file.repository)?;
    revalidate_live_root(authority, &live_root)?;
    git_executable
        .verify()
        .map_err(|_| QueryError::GitUnavailable)?;
    let (root, geometry) = validate_root(&live_root, git_executable)?;
    verify_git_top_level(&root, git_executable)?;
    let file_on_disk = bind_live_file_if_present(&root, Path::new(&file.relative_path))?;
    let root_text = root
        .path()
        .to_str()
        .ok_or(QueryError::RepositoryUnavailable)?;

    let head_oid = git_text(
        git_executable,
        ["-C", root_text, "rev-parse", "--verify", "HEAD^{commit}"],
        MAX_GIT_IDENTITY_BYTES,
    )?
    .trim()
    .to_ascii_lowercase();
    if !is_object_id(&head_oid) {
        return Err(QueryError::RepositoryUnavailable);
    }
    let tree_path = format!("{head_oid}:{}", file.relative_path);
    require_git_success(
        git_executable,
        ["-C", root_text, "cat-file", "-e", &tree_path],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    let blob_is_empty = git_text(
        git_executable,
        ["-C", root_text, "cat-file", "-s", &tree_path],
        MAX_GIT_IDENTITY_BYTES,
    )?
    .trim()
    .parse::<u64>()
    .map_err(|_| QueryError::RepositoryUnavailable)?
        == 0;

    validate_resume(requested.as_ref(), resume, &head_oid)?;
    validate_requested_range(
        git_executable,
        root_text,
        &head_oid,
        &file.relative_path,
        requested.as_ref(),
    )?;

    let requested_start = requested.as_ref().map_or(1, |lines| lines.start);
    let requested_end = requested.as_ref().map(|lines| lines.end);
    let window_start = resume.map_or(requested_start, |position| position.window_start);
    let natural_end = window_start.saturating_add(BLAME_WINDOW_LINES - 1);
    let window_end = requested_end.map_or(natural_end, |end| end.min(natural_end));
    let next_line = resume.map_or(window_start, |position| position.next_line);
    if window_end < window_start || next_line < window_start || next_line > window_end {
        return Err(QueryError::InvalidRequest(
            "file cursor window is invalid".to_owned(),
        ));
    }

    let window_lines = window_end.saturating_sub(window_start).saturating_add(1);
    let lookahead = if requested_end == Some(window_end) {
        0
    } else {
        BLAME_LOOKAHEAD_LINES
    };
    let count = window_lines.saturating_add(lookahead);
    let range = format!("{window_start},+{count}");
    let output = if blob_is_empty {
        Vec::new()
    } else {
        git_output(
            git_executable,
            [
                "-c",
                "core.pager=cat",
                "-c",
                "pager.blame=false",
                "-C",
                root_text,
                "blame",
                "--line-porcelain",
                "--no-progress",
                "--no-textconv",
                "-L",
                &range,
                &head_oid,
                "--",
                &file.relative_path,
            ],
            MAX_GIT_OUTPUT_BYTES,
        )?
    };
    let parsed = parse_porcelain(&output)?;
    let more_committed_lines =
        parsed.iter().any(|line| line.line > window_end) && requested_end != Some(window_end);
    let observations = group_lines(
        file_id,
        &file.core_citation,
        parsed
            .into_iter()
            .filter(|line| line.line >= next_line && line.line <= window_end)
            .collect(),
    );
    let worktree_status =
        worktree_status(git_executable, root_text, &head_oid, &file.relative_path)?;

    root.verify()
        .map_err(|_| QueryError::RepositoryUnavailable)?;
    geometry.verify()?;
    if let Some(file) = file_on_disk.as_ref() {
        file.verify()
            .map_err(|_| QueryError::RepositoryUnavailable)?;
    }
    revalidate_live_root(authority, &live_root)?;
    let final_head = git_text(
        git_executable,
        ["-C", root_text, "rev-parse", "--verify", "HEAD^{commit}"],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    if final_head.trim().to_ascii_lowercase() != head_oid {
        return Err(QueryError::StaleSnapshot);
    }
    git_executable
        .verify()
        .map_err(|_| QueryError::GitUnavailable)?;
    root.verify()
        .map_err(|_| QueryError::RepositoryUnavailable)?;
    geometry.verify()?;
    if let Some(file) = file_on_disk.as_ref() {
        file.verify()
            .map_err(|_| QueryError::RepositoryUnavailable)?;
    }
    validate_geometry_fingerprint(&root, &live_root, git_executable)?.verify()?;
    revalidate_live_root(authority, &live_root)?;
    let current_root = unique_root(authority, &file.repository)?;
    if current_root != live_root {
        return Err(QueryError::StaleSnapshot);
    }
    if live_root_head(&current_root, git_executable)? != head_oid {
        return Err(QueryError::StaleSnapshot);
    }
    Ok(GitBlameWindow {
        head_oid,
        worktree_status,
        window_start,
        window_end,
        observations,
        more_committed_lines,
    })
}

fn required_git_executable(
    git_executable: Option<&GitExecutable>,
) -> Result<&GitExecutable, QueryError> {
    git_executable.ok_or(QueryError::GitUnavailable)
}

fn validate_resume(
    requested: Option<&LineRange>,
    resume: Option<&FileBlamePosition>,
    head_oid: &str,
) -> Result<(), QueryError> {
    let Some(resume) = resume else {
        return Ok(());
    };
    if resume.head_oid != head_oid {
        return Err(QueryError::StaleSnapshot);
    }
    let requested_start = requested.map_or(1, |lines| lines.start);
    let requested_end = requested.map(|lines| lines.end);
    if resume.requested_start != requested_start || resume.requested_end != requested_end {
        return Err(QueryError::InvalidRequest(
            "file cursor range does not match the request".to_owned(),
        ));
    }
    let expected_end = requested_end.map_or(
        resume.window_start.saturating_add(BLAME_WINDOW_LINES - 1),
        |end| end.min(resume.window_start.saturating_add(BLAME_WINDOW_LINES - 1)),
    );
    let aligned = resume
        .window_start
        .checked_sub(requested_start)
        .is_some_and(|offset| offset % BLAME_WINDOW_LINES == 0);
    if !aligned
        || resume.window_end != expected_end
        || resume.next_line < resume.window_start
        || resume.next_line > resume.window_end
    {
        return Err(QueryError::InvalidRequest(
            "file cursor window is not canonical".to_owned(),
        ));
    }
    Ok(())
}

fn validate_requested_range(
    git_executable: &GitExecutable,
    root: &str,
    head_oid: &str,
    file: &str,
    requested: Option<&LineRange>,
) -> Result<(), QueryError> {
    let Some(lines) = requested else {
        return Ok(());
    };
    let range = format!("{},+1", lines.end);
    let (status, _) = run_git(
        git_executable,
        [
            "-c",
            "core.pager=cat",
            "-c",
            "pager.blame=false",
            "-C",
            root,
            "blame",
            "--line-porcelain",
            "--no-progress",
            "--no-textconv",
            "-L",
            &range,
            head_oid,
            "--",
            file,
        ],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    if !status.success() {
        return Err(QueryError::LineOutOfRange);
    }
    Ok(())
}

fn worktree_status(
    git_executable: &GitExecutable,
    root: &str,
    head_oid: &str,
    file: &str,
) -> Result<crate::protocol::WorktreeStatus, QueryError> {
    let (status, _) = run_git(
        git_executable,
        [
            "-C",
            root,
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            head_oid,
            "--",
            file,
        ],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    match status.code() {
        Some(0) => Ok(crate::protocol::WorktreeStatus::Clean),
        Some(1) => Ok(crate::protocol::WorktreeStatus::Differs),
        _ => Err(QueryError::RepositoryUnavailable),
    }
}

fn bind_live_file_if_present(
    root: &BoundRepositoryRoot,
    relative: &Path,
) -> Result<Option<BoundRepositoryFile>, QueryError> {
    let mut candidate = root.path().to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        candidate.push(component);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(QueryError::RepositoryUnavailable);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(QueryError::RepositoryUnavailable),
        }
    }
    root.existing_file(relative)
        .map(Some)
        .map_err(|_| QueryError::RepositoryUnavailable)
}

#[derive(Clone)]
struct ParsedLine {
    commit: String,
    line: u32,
}

fn parse_porcelain(output: &[u8]) -> Result<Vec<ParsedLine>, QueryError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| QueryError::Backend("Git blame output was not UTF-8".to_owned()))?;
    let lines = text.lines().collect::<Vec<_>>();
    let mut parsed = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let fields = lines[index].split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 3
            && matches!(fields[0].len(), 40 | 64)
            && fields[0].bytes().all(|byte| byte == b'0')
        {
            return Err(QueryError::RepositoryUnavailable);
        }
        if fields.len() < 3 || !is_object_id(fields[0]) {
            index += 1;
            continue;
        }
        let final_line = fields[2]
            .parse::<u32>()
            .map_err(|_| QueryError::Backend("Git blame line was invalid".to_owned()))?;
        let commit = fields[0].to_ascii_lowercase();
        index += 1;
        while index < lines.len() {
            if lines[index].starts_with('\t') {
                index += 1;
                break;
            }
            index += 1;
        }
        parsed.push(ParsedLine {
            commit,
            line: final_line,
        });
    }
    Ok(parsed)
}

fn group_lines(
    file_id: &ResourceId,
    core_citation: &Citation,
    lines: Vec<ParsedLine>,
) -> Vec<GitLineObservation> {
    let mut observations: Vec<GitLineObservation> = Vec::new();
    for line in lines {
        if let Some(previous) = observations.last_mut()
            && previous.commit_selector == line.commit
            && previous.lines.end.checked_add(1) == Some(line.line)
        {
            previous.lines.end = line.line;
            continue;
        }
        let range = LineRange {
            start: line.line,
            end: line.line,
        };
        observations.push(GitLineObservation {
            file: file_id.clone(),
            lines: range.clone(),
            commit_selector: line.commit.clone(),
            citation: core_citation.clone(),
        });
    }
    observations
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value.bytes().any(|byte| byte != b'0')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

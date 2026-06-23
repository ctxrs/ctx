use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use work_record_core::{
    Confidence, PullRequestLinkSource, PullRequestProvider, VcsHost, VcsKind,
    WorkRecordLinkTargetType, WorkRecordLinkType,
};

#[derive(Debug, Error)]
pub enum VcsError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("path does not exist: {0}")]
    MissingPath(PathBuf),
    #[error("could not parse pull request URL: {0}")]
    InvalidPullRequestUrl(String),
}

pub type Result<T> = std::result::Result<T, VcsError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcsInspection {
    pub cwd: String,
    pub git: GitDetection,
    pub jj: JjDetection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDetection {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<GitWorkspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorkspace {
    pub root_path: String,
    pub git_dir: String,
    pub git_common_dir: String,
    pub is_worktree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub status: GitStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<GitUpstream>,
    #[serde(default)]
    pub recent_commits: Vec<GitCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_remote: Option<GitRemote>,
    #[serde(default)]
    pub remotes: Vec<GitRemote>,
    pub repo_fingerprint: RepoFingerprint,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub dirty: bool,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub conflicted: bool,
    #[serde(default)]
    pub entries: Vec<GitStatusEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusEntry {
    pub path: String,
    pub index_status: String,
    pub worktree_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitUpstream {
    pub name: String,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommit {
    pub sha: String,
    pub short_sha: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRemote {
    pub name: String,
    pub normalized_url: String,
    pub redacted_url: String,
    pub host: VcsHost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFingerprint {
    pub kind: VcsKind,
    pub algorithm: String,
    pub value: String,
    pub source: RepoFingerprintSource,
    pub root_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_remote_url_normalized: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoFingerprintSource {
    RemoteAndPath,
    PathOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JjDetection {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<JjWorkspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JjWorkspace {
    pub root_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_copy: Option<JjCommit>,
    #[serde(default)]
    pub parents: Vec<JjCommit>,
    #[serde(default)]
    pub bookmarks: Vec<JjBookmark>,
    #[serde(default)]
    pub recent_changes: Vec<JjCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colocated_git: Option<JjColocatedGit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JjCommit {
    pub change_id: String,
    pub commit_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_commit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub bookmarks: Vec<String>,
    #[serde(default)]
    pub parent_change_ids: Vec<String>,
    #[serde(default)]
    pub parent_commit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JjBookmark {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<String>,
    #[serde(default)]
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JjColocatedGit {
    pub root_path: String,
    pub git_dir: String,
    pub git_common_dir: String,
    pub is_worktree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub status: GitStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRemoteUrl {
    pub normalized_url: String,
    pub redacted_url: String,
    pub host: VcsHost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedPullRequest {
    pub provider: PullRequestProvider,
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub normalized_url: String,
    pub confidence: Confidence,
    pub link_source: PullRequestLinkSource,
    pub link: LinkCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCandidate {
    pub target_type: WorkRecordLinkTargetType,
    pub link_type: WorkRecordLinkType,
    pub confidence: Confidence,
    pub source: PullRequestLinkSource,
}

pub fn inspect_path(path: impl AsRef<Path>) -> Result<VcsInspection> {
    let cwd = canonical_existing_dir(path.as_ref())?;
    Ok(VcsInspection {
        cwd: path_to_string(&cwd),
        git: inspect_git(&cwd),
        jj: inspect_jj(&cwd),
    })
}

pub fn inspect_git(cwd: &Path) -> GitDetection {
    let root = match command_stdout("git", &["rev-parse", "--show-toplevel"], cwd) {
        CommandResult::Success(stdout) => PathBuf::from(stdout),
        CommandResult::Unavailable(error) => {
            return GitDetection {
                available: false,
                workspace: None,
                error: Some(error),
            };
        }
        CommandResult::Failure(error) => {
            return GitDetection {
                available: true,
                workspace: None,
                error: Some(error),
            };
        }
    };

    let root_path = canonicalize_or_self(&root);
    let git_dir = optional_command_stdout("git", &["rev-parse", "--absolute-git-dir"], cwd)
        .map(PathBuf::from)
        .map(|path| canonicalize_or_self(&path))
        .unwrap_or_else(|| root_path.join(".git"));
    let git_common_dir = git_common_dir(cwd, &git_dir);
    let is_worktree = git_dir != git_common_dir;
    let remotes = git_remotes(cwd);
    let primary_remote = primary_remote(&remotes).cloned();
    let repo_fingerprint = repo_fingerprint(
        VcsKind::Git,
        &root_path,
        primary_remote
            .as_ref()
            .map(|remote| remote.normalized_url.as_str()),
    );

    GitDetection {
        available: true,
        workspace: Some(GitWorkspace {
            root_path: path_to_string(&root_path),
            git_dir: path_to_string(&git_dir),
            git_common_dir: path_to_string(&git_common_dir),
            is_worktree,
            head_sha: optional_command_stdout("git", &["rev-parse", "--verify", "HEAD"], cwd),
            branch: optional_non_empty_command_stdout("git", &["branch", "--show-current"], cwd),
            status: git_status(cwd),
            upstream: git_upstream(cwd),
            recent_commits: git_recent_commits(cwd, 10),
            primary_remote,
            remotes,
            repo_fingerprint,
        }),
        error: None,
    }
}

pub fn inspect_jj(cwd: &Path) -> JjDetection {
    match command_stdout("jj", &["root"], cwd) {
        CommandResult::Success(stdout) => {
            let root_path = canonicalize_or_self(&PathBuf::from(stdout));
            JjDetection {
                available: true,
                workspace: Some(JjWorkspace {
                    root_path: path_to_string(&root_path),
                    working_copy: jj_log_commits(cwd, "@", 1).into_iter().next(),
                    parents: jj_log_commits(cwd, "@-", 8),
                    bookmarks: jj_bookmarks(cwd),
                    recent_changes: jj_log_commits(cwd, "ancestors(@, 10)", 10),
                    colocated_git: jj_colocated_git(cwd),
                }),
                error: None,
            }
        }
        CommandResult::Unavailable(error) => JjDetection {
            available: false,
            workspace: None,
            error: Some(error),
        },
        CommandResult::Failure(error) => JjDetection {
            available: true,
            workspace: None,
            error: Some(error),
        },
    }
}

pub fn normalize_remote_url(raw: &str) -> NormalizedRemoteUrl {
    let trimmed = raw.trim();
    if let Some(normalized) = normalize_scp_like(trimmed) {
        return normalized;
    }

    match Url::parse(trimmed) {
        Ok(url) => normalize_url(url),
        Err(_) => normalize_local_remote(trimmed),
    }
}

pub fn parse_pull_request_url(raw: &str) -> Result<ParsedPullRequest> {
    let url = parse_url_lenient(raw)
        .ok_or_else(|| VcsError::InvalidPullRequestUrl(raw.trim().to_owned()))?;
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| VcsError::InvalidPullRequestUrl(raw.trim().to_owned()))?;
    let segments = path_segments(&url);

    if let Some(parsed) = parse_github_pr(&host, &segments) {
        return Ok(parsed);
    }
    if let Some(parsed) = parse_gitlab_pr(&host, &segments) {
        return Ok(parsed);
    }

    Err(VcsError::InvalidPullRequestUrl(raw.trim().to_owned()))
}

fn parse_github_pr(host: &str, segments: &[String]) -> Option<ParsedPullRequest> {
    if segments.len() < 4 || segments.get(2)? != "pull" {
        return None;
    }
    let number = segments.get(3)?.parse().ok()?;
    let owner = segments.first()?.to_owned();
    let repo = strip_dot_git(segments.get(1)?);
    Some(parsed_pr(
        PullRequestProvider::Github,
        host,
        &owner,
        repo,
        number,
        format!("https://{host}/{owner}/{repo}/pull/{number}"),
    ))
}

fn parse_gitlab_pr(host: &str, segments: &[String]) -> Option<ParsedPullRequest> {
    let marker = segments
        .windows(3)
        .position(|window| window[0] == "-" && window[1] == "merge_requests")?;
    let number = segments.get(marker + 2)?.parse().ok()?;
    let repo_path = &segments[..marker];
    if repo_path.len() < 2 {
        return None;
    }
    let repo = strip_dot_git(repo_path.last()?);
    let owner = repo_path[..repo_path.len() - 1].join("/");
    Some(parsed_pr(
        PullRequestProvider::Gitlab,
        host,
        &owner,
        repo,
        number,
        format!("https://{host}/{owner}/{repo}/-/merge_requests/{number}"),
    ))
}

fn parsed_pr(
    provider: PullRequestProvider,
    host: &str,
    owner: &str,
    repo: &str,
    number: u64,
    normalized_url: String,
) -> ParsedPullRequest {
    ParsedPullRequest {
        provider,
        host: host.to_owned(),
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        number,
        normalized_url,
        confidence: Confidence::Explicit,
        link_source: PullRequestLinkSource::Explicit,
        link: LinkCandidate {
            target_type: WorkRecordLinkTargetType::PullRequest,
            link_type: WorkRecordLinkType::References,
            confidence: Confidence::Explicit,
            source: PullRequestLinkSource::Explicit,
        },
    }
}

fn canonical_existing_dir(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Err(VcsError::MissingPath(path.to_path_buf()));
    }
    let dir = if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    Ok(fs::canonicalize(dir)?)
}

fn git_common_dir(cwd: &Path, git_dir: &Path) -> PathBuf {
    optional_command_stdout(
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd,
    )
    .or_else(|| optional_command_stdout("git", &["rev-parse", "--git-common-dir"], cwd))
    .map(PathBuf::from)
    .map(|path| {
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    })
    .map(|path| canonicalize_or_self(&path))
    .unwrap_or_else(|| git_dir.to_path_buf())
}

fn git_remotes(cwd: &Path) -> Vec<GitRemote> {
    let names = match optional_command_stdout("git", &["remote"], cwd) {
        Some(stdout) => stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };

    let mut remotes = Vec::new();
    for name in names {
        let Some(urls) =
            optional_command_stdout("git", &["remote", "get-url", "--all", &name], cwd)
        else {
            continue;
        };
        for raw_url in urls.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let normalized = normalize_remote_url(raw_url);
            remotes.push(GitRemote {
                name: name.clone(),
                normalized_url: normalized.normalized_url,
                redacted_url: normalized.redacted_url,
                host: normalized.host,
                owner: normalized.owner,
                repo: normalized.repo,
            });
        }
    }
    remotes.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.normalized_url.cmp(&right.normalized_url))
    });
    remotes
}

fn git_status(cwd: &Path) -> GitStatus {
    optional_command_stdout("git", &["status", "--porcelain=v1", "-z"], cwd)
        .map(|stdout| parse_git_status_porcelain_z(stdout.as_bytes()))
        .unwrap_or_default()
}

fn parse_git_status_porcelain_z(raw: &[u8]) -> GitStatus {
    let mut status = GitStatus::default();
    let mut parts = raw.split(|byte| *byte == 0).filter(|part| !part.is_empty());

    while let Some(part) = parts.next() {
        if part.len() < 3 {
            continue;
        }
        let index = part[0] as char;
        let worktree = part[1] as char;
        let path = String::from_utf8_lossy(&part[3..]).into_owned();
        let original_path = if index == 'R' || index == 'C' {
            parts
                .next()
                .map(|path| String::from_utf8_lossy(path).into_owned())
        } else {
            None
        };

        let entry = GitStatusEntry {
            path,
            index_status: index.to_string(),
            worktree_status: worktree.to_string(),
            original_path,
        };
        let staged = index != ' ' && index != '?' && index != '!';
        let untracked = index == '?' && worktree == '?';
        let unstaged = worktree != ' ' || untracked;
        let conflicted = is_conflicted_git_status(index, worktree);

        status.staged |= staged;
        status.unstaged |= unstaged;
        status.untracked |= untracked;
        status.conflicted |= conflicted;
        status.entries.push(entry);
    }

    status.dirty = !status.entries.is_empty();
    status
}

fn is_conflicted_git_status(index: char, worktree: char) -> bool {
    matches!(
        (index, worktree),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}

fn git_upstream(cwd: &Path) -> Option<GitUpstream> {
    let name = optional_non_empty_command_stdout(
        "git",
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
        cwd,
    )?;
    let (ahead, behind) = optional_command_stdout(
        "git",
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        cwd,
    )
    .and_then(|stdout| parse_git_ahead_behind(&stdout))
    .unwrap_or((0, 0));
    Some(GitUpstream {
        name,
        ahead,
        behind,
    })
}

fn parse_git_ahead_behind(stdout: &str) -> Option<(u32, u32)> {
    let mut fields = stdout.split_whitespace();
    let ahead = fields.next()?.parse().ok()?;
    let behind = fields.next()?.parse().ok()?;
    Some((ahead, behind))
}

fn git_recent_commits(cwd: &Path, limit: usize) -> Vec<GitCommit> {
    let format = "%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e";
    optional_command_stdout(
        "git",
        &[
            "log",
            "--date=iso-strict",
            &format!("--max-count={limit}"),
            &format!("--format={format}"),
        ],
        cwd,
    )
    .map(|stdout| parse_git_log_records(&stdout))
    .unwrap_or_default()
}

fn parse_git_log_records(stdout: &str) -> Vec<GitCommit> {
    stdout
        .split('\x1e')
        .filter_map(|record| {
            let trimmed = record.trim_matches(|ch| ch == '\n' || ch == '\r');
            if trimmed.is_empty() {
                return None;
            }
            let mut fields = trimmed.split('\x1f');
            let sha = non_empty_string(fields.next()?)?;
            let short_sha = non_empty_string(fields.next()?)?;
            let author_name = optional_field(fields.next());
            let author_email = optional_field(fields.next());
            let author_timestamp = optional_field(fields.next());
            let summary = fields.collect::<Vec<_>>().join("\x1f");
            Some(GitCommit {
                sha,
                short_sha,
                summary,
                author_name,
                author_email,
                author_timestamp,
            })
        })
        .collect()
}

fn primary_remote(remotes: &[GitRemote]) -> Option<&GitRemote> {
    remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .or_else(|| remotes.first())
}

fn jj_log_commits(cwd: &Path, revset: &str, limit: usize) -> Vec<JjCommit> {
    let template = r#"change_id ++ "\x1f" ++ commit_id ++ "\x1f" ++ description.first_line() ++ "\x1f" ++ bookmarks.join(",") ++ "\x1f" ++ parents.map(|c| c.change_id()).join(",") ++ "\x1f" ++ parents.map(|c| c.commit_id()).join(",") ++ "\x1f" ++ author.name() ++ "\x1f" ++ author.email() ++ "\x1f" ++ author.timestamp().format("%Y-%m-%dT%H:%M:%SZ") ++ "\x1e""#;
    optional_command_stdout(
        "jj",
        &[
            "log",
            "--no-graph",
            "-r",
            revset,
            "--limit",
            &limit.to_string(),
            "-T",
            template,
        ],
        cwd,
    )
    .map(|stdout| parse_jj_log_records(&stdout))
    .unwrap_or_default()
}

fn parse_jj_log_records(stdout: &str) -> Vec<JjCommit> {
    stdout
        .split('\x1e')
        .filter_map(|record| {
            let trimmed = record.trim_matches(|ch| ch == '\n' || ch == '\r');
            if trimmed.is_empty() {
                return None;
            }
            let mut fields = trimmed.split('\x1f');
            let change_id = non_empty_string(fields.next()?)?;
            let commit_id = non_empty_string(fields.next()?)?;
            let description = optional_field(fields.next());
            let bookmarks = split_csv_field(fields.next());
            let parent_change_ids = split_csv_field(fields.next());
            let parent_commit_ids = split_csv_field(fields.next());
            let author_name = optional_field(fields.next());
            let author_email = optional_field(fields.next());
            let author_timestamp = optional_field(fields.next());
            Some(JjCommit {
                short_commit_id: Some(short_id(&commit_id)),
                change_id,
                commit_id,
                description,
                bookmarks,
                parent_change_ids,
                parent_commit_ids,
                author_name,
                author_email,
                author_timestamp,
            })
        })
        .collect()
}

fn jj_bookmarks(cwd: &Path) -> Vec<JjBookmark> {
    let template = r#"name ++ "\x1f" ++ if(remote, "remote", "local") ++ "\x1f" ++ change_id ++ "\x1f" ++ commit_id ++ "\x1e""#;
    optional_command_stdout("jj", &["bookmark", "list", "-T", template], cwd)
        .map(|stdout| parse_jj_bookmark_records(&stdout))
        .unwrap_or_default()
}

fn parse_jj_bookmark_records(stdout: &str) -> Vec<JjBookmark> {
    stdout
        .split('\x1e')
        .filter_map(|record| {
            let trimmed = record.trim_matches(|ch| ch == '\n' || ch == '\r');
            if trimmed.is_empty() {
                return None;
            }
            let mut fields = trimmed.split('\x1f');
            let name = non_empty_string(fields.next()?)?;
            let remote = matches!(fields.next().map(str::trim), Some("remote"));
            let change_id = optional_field(fields.next());
            let commit_id = optional_field(fields.next());
            Some(JjBookmark {
                name,
                change_id,
                commit_id,
                remote,
            })
        })
        .collect()
}

fn jj_colocated_git(cwd: &Path) -> Option<JjColocatedGit> {
    let root = match command_stdout("git", &["rev-parse", "--show-toplevel"], cwd) {
        CommandResult::Success(stdout) => PathBuf::from(stdout),
        CommandResult::Failure(_) | CommandResult::Unavailable(_) => return None,
    };
    let root_path = canonicalize_or_self(&root);
    let git_dir = optional_command_stdout("git", &["rev-parse", "--absolute-git-dir"], cwd)
        .map(PathBuf::from)
        .map(|path| canonicalize_or_self(&path))
        .unwrap_or_else(|| root_path.join(".git"));
    let git_common_dir = git_common_dir(cwd, &git_dir);
    Some(JjColocatedGit {
        root_path: path_to_string(&root_path),
        git_dir: path_to_string(&git_dir),
        git_common_dir: path_to_string(&git_common_dir),
        is_worktree: git_dir != git_common_dir,
        head_sha: optional_command_stdout("git", &["rev-parse", "--verify", "HEAD"], cwd),
        branch: optional_non_empty_command_stdout("git", &["branch", "--show-current"], cwd),
        status: git_status(cwd),
    })
}

fn repo_fingerprint(
    kind: VcsKind,
    root_path: &Path,
    primary_remote_url_normalized: Option<&str>,
) -> RepoFingerprint {
    let root = path_to_string(root_path);
    let source = if primary_remote_url_normalized.is_some() {
        RepoFingerprintSource::RemoteAndPath
    } else {
        RepoFingerprintSource::PathOnly
    };
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(root.as_bytes());
    hasher.update(b"\0");
    if let Some(remote) = primary_remote_url_normalized {
        hasher.update(remote.as_bytes());
    }
    let value = hex_lower(&hasher.finalize());

    RepoFingerprint {
        kind,
        algorithm: "sha256".to_owned(),
        value,
        source,
        root_path: root,
        primary_remote_url_normalized: primary_remote_url_normalized.map(str::to_owned),
    }
}

fn normalize_scp_like(raw: &str) -> Option<NormalizedRemoteUrl> {
    let (before_colon, path) = raw.split_once(':')?;
    if before_colon.contains("://") || path.starts_with('/') {
        return None;
    }
    let host = before_colon
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(before_colon)
        .to_ascii_lowercase();
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(normalized_network_remote(&host, path))
}

fn normalize_url(mut url: Url) -> NormalizedRemoteUrl {
    match url.scheme() {
        "http" | "https" | "ssh" | "git" => {
            let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
                return normalize_local_remote(url.as_str());
            };
            normalized_network_remote(&host, url.path().trim_start_matches('/'))
        }
        "file" => {
            url.set_query(None);
            url.set_fragment(None);
            let normalized = strip_dot_git(url.as_str().trim_end_matches('/')).to_owned();
            NormalizedRemoteUrl {
                normalized_url: normalized.clone(),
                redacted_url: normalized,
                host: VcsHost::Local,
                owner: None,
                repo: None,
            }
        }
        _ => normalize_local_remote(url.as_str()),
    }
}

fn normalized_network_remote(host: &str, raw_path: &str) -> NormalizedRemoteUrl {
    let path = normalized_remote_path(raw_path);
    let normalized_url = format!("https://{host}/{path}");
    let (owner, repo) = remote_owner_repo(host, &path);
    NormalizedRemoteUrl {
        normalized_url: normalized_url.clone(),
        redacted_url: normalized_url,
        host: detect_vcs_host(host),
        owner,
        repo,
    }
}

fn normalize_local_remote(raw: &str) -> NormalizedRemoteUrl {
    let normalized = strip_dot_git(raw.trim().trim_end_matches('/')).to_owned();
    NormalizedRemoteUrl {
        normalized_url: normalized.clone(),
        redacted_url: normalized,
        host: VcsHost::Local,
        owner: None,
        repo: None,
    }
}

fn remote_owner_repo(host: &str, path: &str) -> (Option<String>, Option<String>) {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return (None, segments.last().cloned());
    }

    if detect_vcs_host(host) == VcsHost::Gitlab && segments.len() > 2 {
        (
            Some(segments[..segments.len() - 1].join("/")),
            segments.last().cloned(),
        )
    } else {
        (segments.first().cloned(), segments.get(1).cloned())
    }
}

fn normalized_remote_path(raw_path: &str) -> String {
    strip_dot_git(raw_path.trim().trim_matches('/')).to_owned()
}

fn strip_dot_git(value: &str) -> &str {
    value.strip_suffix(".git").unwrap_or(value)
}

fn detect_vcs_host(host: &str) -> VcsHost {
    if host.contains("github") {
        VcsHost::Github
    } else if host.contains("gitlab") {
        VcsHost::Gitlab
    } else if host.contains("bitbucket") {
        VcsHost::Bitbucket
    } else {
        VcsHost::Unknown
    }
}

fn parse_url_lenient(raw: &str) -> Option<Url> {
    let trimmed = raw.trim();
    Url::parse(trimmed)
        .ok()
        .or_else(|| Url::parse(&format!("https://{trimmed}")).ok())
}

fn path_segments(url: &Url) -> Vec<String> {
    url.path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

enum CommandResult {
    Success(String),
    Failure(String),
    Unavailable(String),
}

fn optional_non_empty_command_stdout(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    optional_command_stdout(program, args, cwd).filter(|output| !output.is_empty())
}

fn optional_command_stdout(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    match command_stdout(program, args, cwd) {
        CommandResult::Success(stdout) => Some(stdout),
        CommandResult::Failure(_) | CommandResult::Unavailable(_) => None,
    }
}

fn command_stdout(program: &str, args: &[&str], cwd: &Path) -> CommandResult {
    match Command::new(program).args(args).current_dir(cwd).output() {
        Ok(output) if output.status.success() => {
            CommandResult::Success(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let message = if stderr.is_empty() { stdout } else { stderr };
            let command = display_command(program, args);
            CommandResult::Failure(redact_command_error(if message.is_empty() {
                format!("{command} failed: exited with {}", output.status)
            } else {
                format!("{command} failed: {message}")
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            CommandResult::Unavailable(format!("{program} executable not found"))
        }
        Err(error) => CommandResult::Failure(redact_command_error(format!(
            "{} failed: {error}",
            display_command(program, args)
        ))),
    }
}

fn display_command(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_command_error(message: impl AsRef<str>) -> String {
    let mut redacted = Vec::with_capacity(message.as_ref().len());
    for token in message.as_ref().split_whitespace() {
        let token = redact_url_credentials(token);
        let lower = token.to_ascii_lowercase();
        if lower.contains("token=")
            || lower.contains("access_token=")
            || lower.contains("password=")
            || lower.contains("credential=")
        {
            redacted.push(redact_query_value(&token));
        } else {
            redacted.push(token);
        }
    }
    redacted.join(" ")
}

fn redact_url_credentials(token: &str) -> String {
    let Some(scheme_end) = token.find("://") else {
        return token.to_owned();
    };
    let authority_start = scheme_end + 3;
    let Some(at_offset) = token[authority_start..].find('@') else {
        return token.to_owned();
    };
    let at_index = authority_start + at_offset;
    format!(
        "{}<redacted>@{}",
        &token[..authority_start],
        &token[at_index + 1..]
    )
}

fn redact_query_value(token: &str) -> String {
    let mut out = token.to_owned();
    for key in ["access_token", "credential", "password", "token"] {
        out = redact_key_value(&out, key);
    }
    out
}

fn redact_key_value(input: &str, key: &str) -> String {
    let mut remaining = input;
    let mut output = String::with_capacity(input.len());
    let needle = format!("{key}=");
    while let Some(index) = remaining.to_ascii_lowercase().find(&needle) {
        output.push_str(&remaining[..index + needle.len()]);
        output.push_str("<redacted>");
        remaining = &remaining[index + needle.len()..];
        let end = remaining
            .find(|ch| matches!(ch, '&' | ' ' | '\'' | '"' | ')' | ']'))
            .unwrap_or(remaining.len());
        remaining = &remaining[end..];
    }
    output.push_str(remaining);
    output
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn optional_field(value: Option<&str>) -> Option<String> {
    value.and_then(non_empty_string)
}

fn split_csv_field(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .filter_map(non_empty_string)
        .collect()
}

fn short_id(value: &str) -> String {
    value.chars().take(12).collect()
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsStr, fs};
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-data/work-record-vcs");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("work-record-vcs-")
            .tempdir_in(root)
            .unwrap()
    }

    fn git<I, S>(cwd: &Path, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git(path, ["init"]);
        git(path, ["config", "user.name", "Ctx Test"]);
        git(path, ["config", "user.email", "ctx@example.invalid"]);
    }

    #[test]
    fn nested_git_repo_detects_inner_root() {
        let temp = tempdir();
        let outer = temp.path().join("outer");
        let inner = outer.join("vendor/inner");
        init_git(&outer);
        init_git(&inner);
        let nested = inner.join("src/deep");
        fs::create_dir_all(&nested).unwrap();

        let inspection = inspect_path(&nested).unwrap();
        let git = inspection.git.workspace.unwrap();

        assert_eq!(
            git.root_path,
            fs::canonicalize(&inner).unwrap().display().to_string()
        );
        assert!(git.remotes.is_empty());
        assert_eq!(git.repo_fingerprint.source, RepoFingerprintSource::PathOnly);
    }

    #[test]
    fn linked_git_worktree_is_marked_as_worktree() {
        let temp = tempdir();
        let repo = temp.path().join("repo");
        init_git(&repo);
        fs::write(repo.join("README.md"), "hello\n").unwrap();
        git(&repo, ["add", "README.md"]);
        git(&repo, ["commit", "-m", "initial"]);
        let linked = temp.path().join("linked");
        git(&repo, ["worktree", "add", linked.to_str().unwrap()]);

        let inspection = inspect_path(&linked).unwrap();
        let git = inspection.git.workspace.unwrap();

        assert_eq!(
            git.root_path,
            fs::canonicalize(&linked).unwrap().display().to_string()
        );
        assert!(git.is_worktree);
        assert!(git.git_common_dir.ends_with("/.git"));
    }

    #[test]
    fn git_repo_without_remote_has_path_only_fingerprint() {
        let temp = tempdir();
        init_git(temp.path());

        let inspection = inspect_path(temp.path()).unwrap();
        let git = inspection.git.workspace.unwrap();

        assert!(git.primary_remote.is_none());
        assert!(git.remotes.is_empty());
        assert_eq!(git.repo_fingerprint.source, RepoFingerprintSource::PathOnly);
        assert!(git.repo_fingerprint.primary_remote_url_normalized.is_none());
    }

    #[test]
    fn git_status_marks_staged_unstaged_untracked_and_recent_commits() {
        let temp = tempdir();
        init_git(temp.path());
        fs::write(temp.path().join("tracked.txt"), "base\n").unwrap();
        git(temp.path(), ["add", "tracked.txt"]);
        git(temp.path(), ["commit", "-m", "initial"]);
        fs::write(temp.path().join("tracked.txt"), "base\nunstaged\n").unwrap();
        fs::write(temp.path().join("staged.txt"), "staged\n").unwrap();
        git(temp.path(), ["add", "staged.txt"]);
        fs::write(temp.path().join("new.txt"), "new\n").unwrap();

        let inspection = inspect_path(temp.path()).unwrap();
        let workspace = inspection.git.workspace.unwrap();
        let paths = workspace
            .status
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();

        assert!(workspace.status.dirty);
        assert!(workspace.status.staged);
        assert!(workspace.status.unstaged);
        assert!(workspace.status.untracked);
        assert!(!workspace.status.conflicted);
        assert!(paths.contains(&"tracked.txt"));
        assert!(paths.contains(&"staged.txt"));
        assert!(paths.contains(&"new.txt"));
        assert_eq!(workspace.recent_commits.len(), 1);
        assert_eq!(workspace.recent_commits[0].summary, "initial");
        assert_eq!(
            workspace.recent_commits[0].author_email.as_deref(),
            Some("ctx@example.invalid")
        );
    }

    #[test]
    fn git_upstream_reports_ahead_and_behind_counts() {
        let temp = tempdir();
        let remote = temp.path().join("remote.git");
        let repo = temp.path().join("repo");
        git(temp.path(), ["init", "--bare", remote.to_str().unwrap()]);
        init_git(&repo);
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, ["add", "README.md"]);
        git(&repo, ["commit", "-m", "base"]);
        git(&repo, ["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&repo, ["push", "-u", "origin", "HEAD:main"]);
        fs::write(repo.join("local.txt"), "local\n").unwrap();
        git(&repo, ["add", "local.txt"]);
        git(&repo, ["commit", "-m", "local"]);

        let clone = temp.path().join("clone");
        git(
            temp.path(),
            ["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
        );
        git(&clone, ["config", "user.name", "Ctx Test"]);
        git(&clone, ["config", "user.email", "ctx@example.invalid"]);
        git(&clone, ["checkout", "main"]);
        fs::write(clone.join("remote.txt"), "remote\n").unwrap();
        git(&clone, ["add", "remote.txt"]);
        git(&clone, ["commit", "-m", "remote"]);
        git(&clone, ["push"]);
        git(&repo, ["fetch", "origin"]);

        let inspection = inspect_path(&repo).unwrap();
        let upstream = inspection.git.workspace.unwrap().upstream.unwrap();

        assert_eq!(upstream.name, "origin/main");
        assert_eq!(upstream.ahead, 1);
        assert_eq!(upstream.behind, 1);
    }

    #[test]
    fn parses_git_porcelain_status_with_renames_and_conflicts() {
        let raw = b" M changed.txt\0A  staged.txt\0?? new.txt\0R  renamed.txt\0old.txt\0UU conflict.txt\0";
        let status = parse_git_status_porcelain_z(raw);

        assert!(status.dirty);
        assert!(status.staged);
        assert!(status.unstaged);
        assert!(status.untracked);
        assert!(status.conflicted);
        assert_eq!(status.entries[3].path, "renamed.txt");
        assert_eq!(status.entries[3].original_path.as_deref(), Some("old.txt"));
    }

    #[test]
    fn multiple_remotes_choose_origin_and_normalize_urls() {
        let temp = tempdir();
        init_git(temp.path());
        git(
            temp.path(),
            ["remote", "add", "upstream", "git@gitlab.com:ctxrs/ctx.git"],
        );
        git(
            temp.path(),
            [
                "remote",
                "add",
                "origin",
                "https://github.com/ctxrs/ctx.git",
            ],
        );

        let inspection = inspect_path(temp.path()).unwrap();
        let workspace = inspection.git.workspace.unwrap();
        let primary = workspace.primary_remote.unwrap();

        assert_eq!(workspace.remotes.len(), 2);
        assert_eq!(primary.name, "origin");
        assert_eq!(primary.normalized_url, "https://github.com/ctxrs/ctx");
        assert_eq!(
            workspace
                .repo_fingerprint
                .primary_remote_url_normalized
                .as_deref(),
            Some("https://github.com/ctxrs/ctx")
        );
        assert_eq!(
            workspace.repo_fingerprint.source,
            RepoFingerprintSource::RemoteAndPath
        );
    }

    #[test]
    fn private_token_remote_urls_are_redacted() {
        let normalized = normalize_remote_url(
            "https://x-access-token:ghp_secret@example@github.com/ctxrs/ctx.git?token=secret",
        );

        assert_eq!(normalized.normalized_url, "https://github.com/ctxrs/ctx");
        assert_eq!(normalized.redacted_url, "https://github.com/ctxrs/ctx");
        assert!(!normalized.normalized_url.contains("ghp_secret"));
        assert!(!normalized.redacted_url.contains("x-access-token"));
        assert!(!normalized.redacted_url.contains("secret"));
    }

    #[test]
    fn parses_jj_log_records_with_change_ids_bookmarks_and_parents() {
        let raw = "changeabc\x1fcommitabc123456789\x1fimplement thing\x1fmain,feature\x1fparentchange\x1fparentcommit\x1fA U Thor\x1fa@example.invalid\x1f2026-06-23T12:00:00Z\x1e";

        let commits = parse_jj_log_records(raw);

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].change_id, "changeabc");
        assert_eq!(commits[0].commit_id, "commitabc123456789");
        assert_eq!(commits[0].short_commit_id.as_deref(), Some("commitabc123"));
        assert_eq!(commits[0].description.as_deref(), Some("implement thing"));
        assert_eq!(commits[0].bookmarks, vec!["main", "feature"]);
        assert_eq!(commits[0].parent_change_ids, vec!["parentchange"]);
        assert_eq!(commits[0].parent_commit_ids, vec!["parentcommit"]);
        assert_eq!(
            commits[0].author_email.as_deref(),
            Some("a@example.invalid")
        );
    }

    #[test]
    fn parses_jj_bookmark_records() {
        let raw = "main\x1flocal\x1fchangeabc\x1fcommitabc\x1eorigin/main\x1fremote\x1f\x1fcommitremote\x1e";

        let bookmarks = parse_jj_bookmark_records(raw);

        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].name, "main");
        assert!(!bookmarks[0].remote);
        assert_eq!(bookmarks[0].change_id.as_deref(), Some("changeabc"));
        assert_eq!(bookmarks[1].name, "origin/main");
        assert!(bookmarks[1].remote);
        assert!(bookmarks[1].change_id.is_none());
        assert_eq!(bookmarks[1].commit_id.as_deref(), Some("commitremote"));
    }

    #[test]
    fn command_errors_are_explicit_and_redacted() {
        let message = redact_command_error(
            "git fetch failed: https://x-access-token:ghp_secret@github.com/ctxrs/ctx.git?token=secret failed",
        );

        assert!(message.contains("git fetch failed"));
        assert!(message.contains("<redacted>@github.com"));
        assert!(message.contains("token=<redacted>"));
        assert!(!message.contains("ghp_secret"));
        assert!(!message.contains("token=secret"));
    }

    #[test]
    fn parses_github_pull_request_urls() {
        let parsed = parse_pull_request_url("https://github.com/ctxrs/ctx/pull/42/files").unwrap();

        assert_eq!(parsed.provider, PullRequestProvider::Github);
        assert_eq!(parsed.host, "github.com");
        assert_eq!(parsed.owner, "ctxrs");
        assert_eq!(parsed.repo, "ctx");
        assert_eq!(parsed.number, 42);
        assert_eq!(
            parsed.normalized_url,
            "https://github.com/ctxrs/ctx/pull/42"
        );
        assert_eq!(parsed.confidence, Confidence::Explicit);
        assert_eq!(
            parsed.link.target_type,
            WorkRecordLinkTargetType::PullRequest
        );
    }

    #[test]
    fn parses_gitlab_merge_request_urls_with_nested_groups() {
        let parsed = parse_pull_request_url(
            "https://gitlab.example.com/platform/team/ctx/-/merge_requests/7",
        )
        .unwrap();

        assert_eq!(parsed.provider, PullRequestProvider::Gitlab);
        assert_eq!(parsed.host, "gitlab.example.com");
        assert_eq!(parsed.owner, "platform/team");
        assert_eq!(parsed.repo, "ctx");
        assert_eq!(parsed.number, 7);
        assert_eq!(
            parsed.normalized_url,
            "https://gitlab.example.com/platform/team/ctx/-/merge_requests/7"
        );
    }
}

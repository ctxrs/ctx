#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeVcsGitCommand {
    IsInsideWorkTree,
    Status {
        include_untracked_files: bool,
    },
    ListUntracked,
    DiffNameStatus {
        base_commit_sha: String,
        no_renames: bool,
    },
    RevParse {
        reference: String,
    },
    RevParseRefs {
        references: Vec<String>,
    },
    MergeBase {
        target_branch: String,
    },
}

impl WorktreeVcsGitCommand {
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::IsInsideWorkTree => {
                vec!["rev-parse".to_string(), "--is-inside-work-tree".to_string()]
            }
            Self::Status {
                include_untracked_files,
            } => {
                let untracked_mode = if *include_untracked_files {
                    "--untracked-files=all"
                } else {
                    "--untracked-files=normal"
                };
                vec![
                    "status".to_string(),
                    "--porcelain".to_string(),
                    "-z".to_string(),
                    "--branch".to_string(),
                    untracked_mode.to_string(),
                ]
            }
            Self::ListUntracked => vec![
                "ls-files".to_string(),
                "--others".to_string(),
                "--exclude-standard".to_string(),
                "-z".to_string(),
            ],
            Self::DiffNameStatus {
                base_commit_sha,
                no_renames,
            } => {
                let mut args = vec!["diff".to_string()];
                if *no_renames {
                    args.push("--no-renames".to_string());
                }
                args.extend([
                    "--name-status".to_string(),
                    "-z".to_string(),
                    base_commit_sha.clone(),
                ]);
                args
            }
            Self::RevParse { reference } => vec!["rev-parse".to_string(), reference.clone()],
            Self::RevParseRefs { references } => {
                let mut args = Vec::with_capacity(references.len() + 1);
                args.push("rev-parse".to_string());
                args.extend(references.iter().cloned());
                args
            }
            Self::MergeBase { target_branch } => vec![
                "merge-base".to_string(),
                target_branch.clone(),
                "HEAD".to_string(),
            ],
        }
    }
}

pub fn parse_git_list_untracked(stdout: &[u8]) -> Vec<String> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect()
}

pub fn parse_git_diff_name_status(stdout: &[u8]) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    let mut parts = stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .peekable();
    while let Some(part) = parts.next() {
        let status = part.trim().to_string();
        if status.is_empty() {
            continue;
        }
        let Some(path) = parts.next() else {
            continue;
        };
        if path.trim().is_empty() {
            continue;
        }
        let status_char = status.chars().next().unwrap_or('M');
        if status_char == 'R' || status_char == 'C' {
            let Some(next_path) = parts.next() else {
                continue;
            };
            if next_path.trim().is_empty() {
                continue;
            }
            out.push((status, next_path, Some(path)));
        } else {
            out.push((status, path, None));
        }
    }
    out
}

pub fn parse_git_single_ref(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_string()
}

pub fn parse_git_refs(stdout: &[u8], expected_count: usize) -> anyhow::Result<Vec<String>> {
    let commits = String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if commits.len() != expected_count {
        anyhow::bail!(
            "git rev-parse returned {} refs for {} requested refs",
            commits.len(),
            expected_count
        );
    }
    Ok(commits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_command_selects_untracked_mode() {
        assert_eq!(
            WorktreeVcsGitCommand::Status {
                include_untracked_files: true
            }
            .args(),
            vec![
                "status",
                "--porcelain",
                "-z",
                "--branch",
                "--untracked-files=all"
            ]
        );
        assert_eq!(
            WorktreeVcsGitCommand::Status {
                include_untracked_files: false
            }
            .args(),
            vec![
                "status",
                "--porcelain",
                "-z",
                "--branch",
                "--untracked-files=normal"
            ]
        );
    }

    #[test]
    fn diff_command_can_disable_renames_for_summary_counts() {
        assert_eq!(
            WorktreeVcsGitCommand::DiffNameStatus {
                base_commit_sha: "abc123".to_string(),
                no_renames: true,
            }
            .args(),
            vec!["diff", "--no-renames", "--name-status", "-z", "abc123"]
        );
    }

    #[test]
    fn parse_git_diff_name_status_keeps_rename_orig_path() {
        let parsed = parse_git_diff_name_status(b"R100\0old.rs\0new.rs\0M\0lib.rs\0");

        assert_eq!(
            parsed,
            vec![
                (
                    "R100".to_string(),
                    "new.rs".to_string(),
                    Some("old.rs".to_string())
                ),
                ("M".to_string(), "lib.rs".to_string(), None),
            ]
        );
    }

    #[test]
    fn parse_git_list_untracked_ignores_empty_parts() {
        assert_eq!(
            parse_git_list_untracked(b"one.txt\0\0two.txt\0"),
            vec!["one.txt".to_string(), "two.txt".to_string()]
        );
    }

    #[test]
    fn parse_git_refs_rejects_missing_refs() {
        let err = parse_git_refs(b"abc123\n", 2).expect_err("expected ref-count error");
        assert!(err
            .to_string()
            .contains("git rev-parse returned 1 refs for 2 requested refs"));
    }
}

use std::collections::HashSet;

use anyhow::Result;
use ctx_core::models::WorktreeVcsTouchedFile;

#[async_trait::async_trait]
pub trait WorktreeVcsDiffPathSource {
    async fn diff_name_status(
        &self,
        base_commit_sha: &str,
        summary_count: bool,
    ) -> Result<Vec<(String, String, Option<String>)>>;

    async fn list_untracked(&self) -> Result<Vec<String>>;
}

pub async fn load_diff_file_count_from_source(
    source: &(impl WorktreeVcsDiffPathSource + Sync),
    base_commit_sha: &str,
) -> Result<i64> {
    // Tier 1 stays merge-base diff based, but avoids rename detection so large
    // change sets do not spend the hot path scoring rename candidates.
    let entries = source.diff_name_status(base_commit_sha, true).await?;
    let untracked = source.list_untracked().await?;
    count_diff_paths(entries, untracked)
}

pub async fn load_diff_touched_entries_from_source(
    source: &(impl WorktreeVcsDiffPathSource + Sync),
    base_commit_sha: &str,
) -> Result<Vec<WorktreeVcsTouchedFile>> {
    let entries = source.diff_name_status(base_commit_sha, false).await?;
    let untracked = source.list_untracked().await?;
    let paths = build_diff_path_states(entries, untracked)?;
    Ok(paths
        .into_iter()
        .map(|(path, orig_path, status)| WorktreeVcsTouchedFile {
            path,
            orig_path,
            index_status: Some(status),
            worktree_status: None,
        })
        .collect())
}

pub fn count_diff_paths(
    entries: Vec<(String, String, Option<String>)>,
    untracked: Vec<String>,
) -> Result<i64> {
    let mut seen = HashSet::new();
    for (status, path, _) in entries {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if !seen.insert(path.to_string()) {
            continue;
        }
        if status.chars().next().is_none() {
            anyhow::bail!("vcs diff returned an empty status for {path}");
        }
    }
    for path in untracked {
        let path = path.trim();
        if !path.is_empty() {
            seen.insert(path.to_string());
        }
    }
    Ok(seen.len() as i64)
}

pub fn build_diff_path_states(
    entries: Vec<(String, String, Option<String>)>,
    untracked: Vec<String>,
) -> Result<Vec<(String, Option<String>, String)>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (status, path, orig_path) in entries {
        let path = path.trim().to_string();
        if path.is_empty() {
            continue;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(status_kind) = status.chars().next() else {
            anyhow::bail!("vcs diff returned an empty status for {path}");
        };
        out.push((path, orig_path, status_kind.to_string()));
    }
    for path in untracked {
        let path = path.trim().to_string();
        if path.is_empty() {
            continue;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        out.push((path, None, "?".to_string()));
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeDiffPathSource {
        entries: Vec<(String, String, Option<String>)>,
        untracked: Vec<String>,
    }

    #[async_trait::async_trait]
    impl WorktreeVcsDiffPathSource for FakeDiffPathSource {
        async fn diff_name_status(
            &self,
            _base_commit_sha: &str,
            summary_count: bool,
        ) -> Result<Vec<(String, String, Option<String>)>> {
            assert!(summary_count);
            Ok(self.entries.clone())
        }

        async fn list_untracked(&self) -> Result<Vec<String>> {
            Ok(self.untracked.clone())
        }
    }

    #[test]
    fn build_diff_path_states_deduplicates_untracked_paths_already_in_diff() -> Result<()> {
        let out = build_diff_path_states(
            vec![("D".to_string(), "src/example.rs".to_string(), None)],
            vec!["src/example.rs".to_string()],
        )?;

        assert_eq!(
            out,
            vec![("src/example.rs".to_string(), None, "D".to_string())]
        );
        Ok(())
    }

    #[test]
    fn count_diff_paths_deduplicates_untracked_paths_already_in_diff() -> Result<()> {
        let count = count_diff_paths(
            vec![("D".to_string(), "src/example.rs".to_string(), None)],
            vec!["src/example.rs".to_string()],
        )?;

        assert_eq!(count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn load_diff_file_count_from_source_uses_summary_mode() -> Result<()> {
        let source = FakeDiffPathSource {
            entries: vec![("M".to_string(), "src/lib.rs".to_string(), None)],
            untracked: vec!["src/new.rs".to_string()],
        };

        let count = load_diff_file_count_from_source(&source, "base").await?;

        assert_eq!(count, 2);
        Ok(())
    }
}

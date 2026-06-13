use std::cmp::Ordering;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct CachedFileCompletions {
    pub cached_at: Instant,
    pub files: Arc<Vec<String>>,
}

pub async fn workspace_has_git_repo(root: impl AsRef<Path>) -> bool {
    ctx_fs::git::assert_git_repo(root).await.is_ok()
}

pub async fn list_host_git_files(root: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let root = root.as_ref();
    let tracked = ctx_fs::git::list_tracked_files(root).await?;
    let untracked = ctx_fs::git::list_untracked_files(root).await?;
    Ok(merge_and_sort_git_paths(tracked, untracked))
}

pub fn merge_and_sort_git_paths(mut tracked: Vec<String>, untracked: Vec<String>) -> Vec<String> {
    if !untracked.is_empty() {
        let mut seen: HashSet<String> = tracked.iter().cloned().collect();
        for path in untracked {
            if seen.insert(path.clone()) {
                tracked.push(path);
            }
        }
    }
    tracked.sort();
    tracked
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ScoredCandidate {
    score: i32,
    path_len: usize,
    path: String,
}

impl Ord for ScoredCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| other.path_len.cmp(&self.path_len))
            .then_with(|| other.path.cmp(&self.path))
    }
}

impl PartialOrd for ScoredCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn match_score(candidate: &str, query: &str) -> Option<i32> {
    let query = query.trim();
    if query.is_empty() {
        return Some(0);
    }

    let cand = candidate.to_lowercase();
    let q = query.to_lowercase();

    let filename = candidate.rsplit('/').next().unwrap_or(candidate);
    let filename_lower = filename.to_lowercase();

    if filename_lower == q {
        return Some(100_000);
    }

    if filename_lower.starts_with(&q) {
        let filename_len = i32::try_from(filename.len()).ok()?;
        return Some(50_000 - filename_len);
    }

    if let Some(idx) = filename_lower.find(&q) {
        let idx = i32::try_from(idx).ok()?;
        let filename_len = i32::try_from(filename.len()).ok()?;
        return Some(20_000 - idx * 10 - filename_len);
    }

    let path_parts: Vec<&str> = candidate.split('/').collect();
    if path_parts.len() > 1 {
        for (i, part) in path_parts.iter().enumerate() {
            if i == path_parts.len() - 1 {
                continue;
            }
            let part_lower = part.to_lowercase();
            if let Some(idx) = part_lower.find(&q) {
                let idx = i32::try_from(idx).ok()?;
                let path_len = i32::try_from(candidate.len()).ok()?;
                return Some(5_000 - idx * 10 - path_len);
            }
        }
    }

    if let Some(idx) = cand.find(&q) {
        let idx = i32::try_from(idx).ok()?;
        let path_len = i32::try_from(candidate.len()).ok()?;
        return Some(2_000 - idx * 10 - path_len);
    }

    None
}

pub fn filter_and_rank_paths(paths: &[String], query: &str, limit: usize) -> Vec<String> {
    let limit = limit.clamp(1, 200);
    let mut heap: BinaryHeap<Reverse<ScoredCandidate>> = BinaryHeap::with_capacity(limit + 1);

    for path in paths {
        let Some(score) = match_score(path, query) else {
            continue;
        };
        let candidate = ScoredCandidate {
            score,
            path_len: path.len(),
            path: path.clone(),
        };
        heap.push(Reverse(candidate));
        if heap.len() > limit {
            heap.pop();
        }
    }

    let mut out: Vec<ScoredCandidate> = heap.into_iter().map(|r| r.0).collect();
    out.sort_by(|a, b| b.cmp(a));
    out.into_iter().map(|c| c.path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_filename_matches_highest() {
        let paths = vec![
            "src/pages/SessionPage.tsx".to_string(),
            "src/pages/WorkbenchPage.tsx".to_string(),
            "README.md".to_string(),
        ];

        let out = filter_and_rank_paths(&paths, "sess", 10);
        assert_eq!(
            out.first().map(|s| s.as_str()),
            Some("src/pages/SessionPage.tsx")
        );
    }

    #[test]
    fn no_fuzzy_subsequence_matching() {
        let paths = vec![
            "scripts/supabase_local_start.sh".to_string(),
            "src/earth_model.ts".to_string(),
        ];

        let out = filter_and_rank_paths(&paths, "earth", 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out.first().map(|s| s.as_str()), Some("src/earth_model.ts"));
    }

    #[test]
    fn returns_deterministic_order_for_empty_query() {
        let paths = vec![
            "b.txt".to_string(),
            "a.txt".to_string(),
            "c.txt".to_string(),
        ];
        let out = filter_and_rank_paths(&paths, "", 2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn filters_out_non_matches() {
        let paths = vec!["src/main.rs".to_string(), "Cargo.toml".to_string()];
        let out = filter_and_rank_paths(&paths, "does-not-exist", 10);
        assert!(out.is_empty());
    }

    #[test]
    fn merge_and_sort_git_paths_dedupes_untracked_after_tracked() {
        let out = merge_and_sort_git_paths(
            vec!["src/main.rs".to_string(), "README.md".to_string()],
            vec!["src/main.rs".to_string(), "notes/todo.md".to_string()],
        );

        assert_eq!(
            out,
            vec![
                "README.md".to_string(),
                "notes/todo.md".to_string(),
                "src/main.rs".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn host_git_listing_returns_tracked_and_untracked_paths() {
        let repo = tempfile::tempdir().expect("repo");
        git(&["init"], repo.path());
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], repo.path());
        git(&["config", "user.email", "ctx@example.com"], repo.path());
        git(&["config", "user.name", "Ctx Test"], repo.path());
        std::fs::create_dir_all(repo.path().join("src")).expect("create src");
        std::fs::write(repo.path().join("src/lib.rs"), "pub fn ok() {}\n").expect("tracked file");
        git(&["add", "src/lib.rs"], repo.path());
        git(&["commit", "-m", "initial"], repo.path());
        std::fs::write(repo.path().join("README.md"), "hello\n").expect("untracked file");

        assert!(workspace_has_git_repo(repo.path()).await);
        let out = list_host_git_files(repo.path())
            .await
            .expect("list git files");

        assert_eq!(out, vec!["README.md".to_string(), "src/lib.rs".to_string()]);
    }

    fn git(args: &[&str], cwd: &Path) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
}

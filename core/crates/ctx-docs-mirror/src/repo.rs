use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use tempfile::TempDir;
use url::Url;

use crate::config::DocsMirrorConfig;
use crate::plan::PageRef;

pub(crate) struct RepoPlan {
    pub(crate) temp_dir: TempDir,
    pub(crate) docs_root: PathBuf,
    pub(crate) pages: Vec<PageRef>,
}

#[derive(Clone)]
pub(crate) struct RepoHint {
    pub(crate) repo_url: String,
    pub(crate) subpath: Option<String>,
}

pub(crate) fn infer_repo_hint_from_docs_url(docs_url: &str) -> Option<RepoHint> {
    let url = Url::parse(docs_url).ok()?;
    let host = url.host_str()?;
    let mut segments: Vec<&str> = url
        .path()
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if host == "raw.githubusercontent.com" {
        if segments.len() < 4 {
            return None;
        }
        let org = segments[0];
        let repo = segments[1];
        segments.drain(0..3);
        let subpath = derive_repo_subpath(&segments);
        return Some(RepoHint {
            repo_url: format!("https://github.com/{org}/{repo}.git"),
            subpath,
        });
    }
    if host == "github.com" {
        if segments.len() < 5 {
            return None;
        }
        let org = segments[0];
        let repo = segments[1];
        let kind = segments[2];
        if kind != "blob" && kind != "raw" {
            return None;
        }
        segments.drain(0..4);
        let subpath = derive_repo_subpath(&segments);
        return Some(RepoHint {
            repo_url: format!("https://github.com/{org}/{repo}.git"),
            subpath,
        });
    }
    None
}

fn derive_repo_subpath(segments: &[&str]) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    let last = segments.last().copied().unwrap_or_default();
    let has_extension = Path::new(last).extension().is_some();
    let end = if has_extension {
        segments.len().saturating_sub(1)
    } else {
        segments.len()
    };
    if end == 0 {
        return None;
    }
    let joined = segments[..end].join("/");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

pub(crate) fn mirror_repo_plan(
    cfg: &DocsMirrorConfig,
    repo_url: &str,
    override_subpath: Option<&str>,
) -> Result<RepoPlan> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path().join("repo");
    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1").arg("--no-tags");
    if let Some(rev) = cfg.revision.as_deref() {
        if !looks_like_sha(rev) {
            cmd.arg("--branch").arg(rev);
        }
    }
    cmd.arg(repo_url).arg(&root);
    let output = cmd.output().context("running git clone")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    if let Some(rev) = cfg.revision.as_deref() {
        if looks_like_sha(rev) {
            let fetch = Command::new("git")
                .arg("-C")
                .arg(&root)
                .arg("fetch")
                .arg("--depth")
                .arg("1")
                .arg("origin")
                .arg(rev)
                .output()
                .context("running git fetch")?;
            if !fetch.status.success() {
                return Err(anyhow!(
                    "git fetch failed: {}",
                    String::from_utf8_lossy(&fetch.stderr)
                ));
            }
            let checkout = Command::new("git")
                .arg("-C")
                .arg(&root)
                .arg("checkout")
                .arg(rev)
                .output()
                .context("running git checkout")?;
            if !checkout.status.success() {
                return Err(anyhow!(
                    "git checkout failed: {}",
                    String::from_utf8_lossy(&checkout.stderr)
                ));
            }
        }
    }

    let docs_root = if let Some(subpath) = override_subpath.or(cfg.repo_subpath.as_deref()) {
        root.join(subpath)
    } else {
        find_docs_root(&root).ok_or_else(|| anyhow!("no docs root found"))?
    };
    if !docs_root.exists() {
        return Err(anyhow!("docs root not found at {}", docs_root.display()));
    }

    let pages = collect_markdown_files(&docs_root)?;
    let page_refs = pages
        .into_iter()
        .map(|path| {
            let rel = path.strip_prefix(&docs_root).unwrap_or(&path).to_path_buf();
            PageRef {
                path: rel,
                url: None,
            }
        })
        .collect();

    Ok(RepoPlan {
        temp_dir,
        docs_root,
        pages: page_refs,
    })
}

fn find_docs_root(repo: &Path) -> Option<PathBuf> {
    let candidates = [
        "docs",
        "website/docs",
        "doc",
        "documentation",
        "site/docs",
        "docs/content",
        "content/docs",
        "content",
    ];
    for cand in candidates {
        let path = repo.join(cand);
        if path.exists() && path.is_dir() {
            return Some(path);
        }
    }
    if let Ok(entries) = fs::read_dir(repo) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("README.md") || name.eq_ignore_ascii_case("README.mdx") {
                return Some(repo.to_path_buf());
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext == "md" || ext == "mdx" {
                return Some(repo.to_path_buf());
            }
        }
    }
    None
}

fn collect_markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walk_dir(root)? {
        let path = entry;
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "md" || ext == "mdx" {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn walk_dir(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_dir(&path)?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

fn looks_like_sha(value: &str) -> bool {
    let len = value.len();
    if !(7..=40).contains(&len) {
        return false;
    }
    value.chars().all(|c| c.is_ascii_hexdigit())
}

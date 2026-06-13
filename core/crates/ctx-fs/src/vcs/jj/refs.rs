use super::*;

fn normalize_jj_revset(reference: &str) -> String {
    let reference = reference.trim();
    if reference == "HEAD" {
        return "@".to_string();
    }
    if let Some(stripped) = reference.strip_prefix("refs/heads/") {
        return stripped.to_string();
    }
    if let Some(stripped) = reference.strip_prefix("refs/remotes/") {
        return stripped.to_string();
    }
    reference.to_string()
}

async fn jj_revset_first_commit(root: &Path, revset: &str) -> Result<Option<String>> {
    let output = run_jj(
        root,
        &["log", "-r", revset, "--no-graph", "-T", "commit_id"],
    )
    .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.to_string()))
}

pub(super) async fn jj_rev_parse(root: &Path, rev: &str) -> Result<String> {
    let revset = normalize_jj_revset(rev);
    jj_revset_first_commit(root, &revset)
        .await?
        .ok_or_else(|| anyhow::anyhow!("jj log produced no revision output"))
}

pub(super) async fn jj_merge_base(root: &Path, a: &str, b: &str) -> Result<String> {
    let a_revset = normalize_jj_revset(a);
    let b_revset = normalize_jj_revset(b);
    let revset = format!("heads(ancestors({a_revset}) & ancestors({b_revset}))");
    jj_revset_first_commit(root, &revset)
        .await?
        .ok_or_else(|| anyhow::anyhow!("jj merge-base produced no common ancestor"))
}

pub(super) async fn jj_is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let ancestor_revset = normalize_jj_revset(ancestor);
    let descendant_revset = normalize_jj_revset(descendant);
    let revset = format!("({ancestor_revset}) & ancestors({descendant_revset})");
    Ok(jj_revset_first_commit(root, &revset).await?.is_some())
}

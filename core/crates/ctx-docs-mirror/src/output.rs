use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::config::{throttle, DocsMirrorConfig};
use crate::http::{fetch_bytes, fetch_text};
use crate::plan::{MirrorMethod, MirrorPlan};

pub(crate) async fn execute_plan(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    plan: &MirrorPlan,
    out_dir: &Path,
) -> Result<()> {
    match plan.method {
        MirrorMethod::Repo => mirror_repo_output(plan, out_dir),
        MirrorMethod::Llms | MirrorMethod::SitemapMd | MirrorMethod::EditLink => {
            mirror_url_output(client, cfg, plan, out_dir).await
        }
        MirrorMethod::Html => mirror_html_output(client, cfg, plan, out_dir).await,
    }
}

pub(crate) fn clean_output_dir(out: &Path) -> Result<()> {
    if !out.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(out)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn mirror_repo_output(plan: &MirrorPlan, out_dir: &Path) -> Result<()> {
    let _keep_temp = plan.repo_temp.as_ref();
    let Some(repo_root) = plan.repo_root.as_ref() else {
        return Err(anyhow!("repo_root missing"));
    };
    for page in &plan.pages {
        let src = repo_root.join(&page.path);
        let dest = out_dir.join(&page.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&src, &dest).with_context(|| format!("copying {}", src.display()))?;
    }
    Ok(())
}

async fn mirror_url_output(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    plan: &MirrorPlan,
    out_dir: &Path,
) -> Result<()> {
    for page in &plan.pages {
        let Some(url) = page.url.as_deref() else {
            continue;
        };
        let body = fetch_bytes(client, url).await?;
        let dest = out_dir.join(&page.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, &body).with_context(|| format!("writing {}", dest.display()))?;
        throttle(cfg).await;
    }
    Ok(())
}

async fn mirror_html_output(
    client: &reqwest::Client,
    cfg: &DocsMirrorConfig,
    plan: &MirrorPlan,
    out_dir: &Path,
) -> Result<()> {
    for page in &plan.pages {
        let Some(url) = page.url.as_deref() else {
            continue;
        };
        let html = fetch_text(client, url).await?;
        let md = html2md::parse_html(&html);
        let dest = out_dir.join(&page.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, md).with_context(|| format!("writing {}", dest.display()))?;
        throttle(cfg).await;
    }
    Ok(())
}

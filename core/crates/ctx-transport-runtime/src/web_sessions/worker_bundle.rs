use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::Mutex;

use super::{NodeRuntimeSpec, WorkerBundle};

const WORKER_PACKAGE_JSON: &str =
    include_str!("../../../../packages/web-session-worker/package.json");
const WORKER_SCRIPT: &str = include_str!("../../../../packages/web-session-worker/bin/worker.mjs");
const WORKER_AUTH_SCRIPT: &str =
    include_str!("../../../../packages/web-session-worker/bin/auth.mjs");

pub async fn ensure_worker_bundle(
    data_root: &Path,
    node: &NodeRuntimeSpec,
) -> Result<WorkerBundle> {
    if let Ok(worker_path) = std::env::var("CTX_WEB_SESSION_WORKER") {
        let node_modules_path = std::env::var("CTX_WEB_SESSION_NODE_PATH")
            .context("CTX_WEB_SESSION_NODE_PATH required with CTX_WEB_SESSION_WORKER")?;
        let worker_path = PathBuf::from(worker_path);
        let node_modules_path = PathBuf::from(node_modules_path);
        let auth_helper_path = worker_path
            .parent()
            .context("web session worker override must have a parent directory")?
            .join("auth.mjs");
        if !worker_path.exists() {
            anyhow::bail!("web session worker not found at {}", worker_path.display());
        }
        if !auth_helper_path.exists() {
            anyhow::bail!(
                "web session worker override must include auth.mjs at {}",
                auth_helper_path.display()
            );
        }
        if !node_modules_path.exists() {
            anyhow::bail!(
                "web session node_modules not found at {}",
                node_modules_path.display()
            );
        }
        return Ok(WorkerBundle {
            worker_path,
            node_modules_path,
        });
    }

    let version = worker_version()?;
    let root = data_root
        .join("tools")
        .join("web-session-worker")
        .join(&version);
    let bin_dir = root.join("bin");
    tokio::fs::create_dir_all(&bin_dir).await?;
    tokio::fs::write(root.join("package.json"), WORKER_PACKAGE_JSON).await?;
    tokio::fs::write(bin_dir.join("worker.mjs"), WORKER_SCRIPT).await?;
    tokio::fs::write(bin_dir.join("auth.mjs"), WORKER_AUTH_SCRIPT).await?;

    let node_modules = root.join("node_modules");
    let deps_ready = node_modules.join("playwright").exists() && node_modules.join("wrtc").exists();
    if !deps_ready {
        let _guard = worker_install_lock().lock().await;
        let deps_ready =
            node_modules.join("playwright").exists() && node_modules.join("wrtc").exists();
        if !deps_ready {
            install_worker_deps(node, &root).await?;
        }
    }

    Ok(WorkerBundle {
        worker_path: bin_dir.join("worker.mjs"),
        node_modules_path: node_modules,
    })
}

fn worker_install_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn worker_version() -> Result<String> {
    static VERSION: OnceLock<String> = OnceLock::new();
    if let Some(version) = VERSION.get() {
        return Ok(version.clone());
    }
    let value: serde_json::Value =
        serde_json::from_str(WORKER_PACKAGE_JSON).context("parsing worker package.json")?;
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .context("worker package.json missing version")?
        .to_string();
    let _ = VERSION.set(version.clone());
    Ok(version)
}

async fn install_worker_deps(node: &NodeRuntimeSpec, root: &Path) -> Result<()> {
    let mut cmd = if let Ok(pnpm) = which::which("pnpm") {
        let mut cmd = Command::new(pnpm);
        cmd.arg("install")
            .arg("--prod")
            .arg("--ignore-scripts")
            .arg("--reporter")
            .arg("silent")
            .current_dir(root);
        cmd
    } else {
        let mut cmd = Command::new(&node.node_bin);
        cmd.arg(&node.npm_cli_js)
            .arg("install")
            .arg("--omit=dev")
            .arg("--no-audit")
            .arg("--no-fund")
            .arg("--ignore-scripts")
            .current_dir(root)
            .env("npm_config_update_notifier", "false")
            .env("npm_config_ignore_scripts", "true");
        cmd
    };

    let output = cmd.output().await.context("running package install")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("package install failed: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_worker_bundle_writes_auth_helper() {
        let data_root = tempfile::tempdir().unwrap();
        let version = worker_version().unwrap();
        let node_modules = data_root
            .path()
            .join("tools")
            .join("web-session-worker")
            .join(&version)
            .join("node_modules");
        tokio::fs::create_dir_all(node_modules.join("playwright"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(node_modules.join("wrtc"))
            .await
            .unwrap();

        let node = NodeRuntimeSpec {
            node_bin: PathBuf::from("/usr/bin/node"),
            npm_cli_js: PathBuf::from("/usr/bin/npm"),
        };
        let bundle = ensure_worker_bundle(data_root.path(), &node).await.unwrap();
        let auth_path = bundle.worker_path.parent().unwrap().join("auth.mjs");
        assert!(auth_path.exists(), "expected bundled auth helper");
    }
}

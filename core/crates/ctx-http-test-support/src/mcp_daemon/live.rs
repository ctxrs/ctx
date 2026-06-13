use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use ctx_core::models::{Session, Task, Workspace};
use ctx_daemon::test_support::TestDaemon;
use ctx_providers::adapters::ProviderAdapter;
use ctx_store::StoreManager;
use serde_json::json;

use super::{git, router, DaemonBackedParentSession};

pub(crate) async fn setup_live_provider_parent_session(
    provider_id: &str,
    model_id: &str,
) -> Result<DaemonBackedParentSession> {
    let repo = git::init_git_repo().await?;
    let data_dir = tempfile::tempdir().context("create daemon data dir")?;
    let stores = StoreManager::open(data_dir.path())
        .await
        .context("open store manager")?;
    let (listener, base_url) = router::bind_loopback_listener().await?;

    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert(
        provider_id.to_string(),
        router::live_provider_adapter(provider_id)?,
    );
    let daemon = TestDaemon::new(
        data_dir.path().to_path_buf(),
        stores,
        providers,
        base_url.clone(),
        None,
    );
    router::spawn_router_for_daemon(listener, &daemon);

    let session = create_live_provider_session(&base_url, repo.path(), provider_id, model_id)
        .await
        .context("create live-provider parent session")?;

    Ok(DaemonBackedParentSession::new(
        repo,
        data_dir,
        daemon,
        base_url,
        session.id,
        String::new(),
    ))
}

async fn create_live_provider_session(
    base_url: &str,
    repo_path: &std::path::Path,
    provider_id: &str,
    model_id: &str,
) -> Result<Session> {
    let client = reqwest::Client::new();
    let workspace: Workspace = client
        .post(format!("{base_url}/api/workspaces"))
        .json(&json!({ "root_path": repo_path.to_string_lossy(), "name": "ws" }))
        .send()
        .await
        .context("create live-provider workspace request")?
        .error_for_status()
        .context("create live-provider workspace status")?
        .json()
        .await
        .context("decode live-provider workspace")?;
    let task: Task = client
        .post(format!(
            "{base_url}/api/workspaces/{}/tasks",
            workspace.id.0
        ))
        .json(&json!({ "title": "t1" }))
        .send()
        .await
        .context("create live-provider task request")?
        .error_for_status()
        .context("create live-provider task status")?
        .json()
        .await
        .context("decode live-provider task")?;
    client
        .post(format!("{base_url}/api/tasks/{}/sessions", task.id.0))
        .json(&json!({ "provider_id": provider_id, "model_id": model_id }))
        .send()
        .await
        .context("create live-provider session request")?
        .error_for_status()
        .context("create live-provider session status")?
        .json()
        .await
        .context("decode live-provider session")
}

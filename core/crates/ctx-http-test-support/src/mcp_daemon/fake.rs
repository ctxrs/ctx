use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use ctx_daemon::test_support::TestDaemon;
use ctx_providers::adapters::ProviderAdapter;
use ctx_providers::fake::FakeProviderAdapter;
use ctx_store::StoreManager;

use super::{git, router, DaemonBackedParentSession};

pub(crate) async fn setup_fake_provider_parent_session() -> Result<DaemonBackedParentSession> {
    let repo = git::init_git_repo().await?;
    let data_dir = tempfile::tempdir().context("create daemon data dir")?;
    let stores = StoreManager::open(data_dir.path())
        .await
        .context("open store manager")?;
    let (listener, base_url) = router::bind_loopback_listener().await?;

    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
    let daemon = TestDaemon::new(
        data_dir.path().to_path_buf(),
        stores,
        providers,
        base_url.clone(),
        Some("daemon-secret".to_string()),
    );
    daemon
        .upsert_provider_status(
            "fake".into(),
            FakeProviderAdapter::new()
                .inspect()
                .await
                .context("inspect fake provider")?,
        )
        .await;
    router::spawn_router_for_daemon(listener, &daemon);

    let base_commit = git::run_git_output(repo.path(), &["rev-parse", "HEAD"]).await?;
    let session = daemon
        .seed_mcp_parent_session_for_test(repo.path(), base_commit, "fake", "fake-model")
        .await
        .context("create parent session")?;

    let mcp_token = daemon
        .issue_provider_session_mcp_token(session.id, session.workspace_id, session.worktree_id)
        .await;

    Ok(DaemonBackedParentSession::new(
        repo, data_dir, daemon, base_url, session.id, mcp_token,
    ))
}

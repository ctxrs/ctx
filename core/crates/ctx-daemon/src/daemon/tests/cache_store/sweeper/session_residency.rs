use super::*;

#[tokio::test]
async fn sweeper_eviction_keeps_active_entries() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("fake".into(), Arc::new(FakeProviderAdapter::new()));
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores.clone(),
        providers,
        "http://localhost".to_string(),
        None,
    ));

    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let store = state.store_for_workspace(workspace.id).await.unwrap();
    let worktree = store
        .create_worktree(
            workspace.id,
            temp.path().to_string_lossy().to_string(),
            "deadbeef".to_string(),
            None,
        )
        .await
        .unwrap();
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".to_string(),
            "model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    {
        let mut cache = state.sessions.session_head_cache.lock().await;
        cache.insert(
            session.id,
            ctx_session_runtime::runtime::TimedEntry::new(HashMap::new()),
        );
    }
    let _ = state.sessions.get_broadcaster(session.id).await;
    let _ = state
        .sessions
        .subscribe_session_event_head(session.id)
        .await;
    let _ = state
        .session_scheduler_worker_host
        .ensure_scheduler(&state.sessions, session.clone())
        .await;

    let now = Instant::now();
    {
        let mut cache = state.sessions.session_head_cache.lock().await;
        if let Some(entry) = cache.get_mut(&session.id) {
            entry.last_access = now - Duration::from_secs(3600);
        }
    }
    {
        let mut map = state.sessions.broadcasters.lock().await;
        if let Some(entry) = map.get_mut(&session.id) {
            entry.last_access = now;
        }
    }
    {
        let mut map = state.sessions.schedulers.lock().await;
        if let Some(entry) = map.get_mut(&session.id) {
            entry.last_access = now;
        }
    }

    let config = CacheSweepConfig {
        session_ttl: Duration::from_secs(60),
        workspace_ttl: Duration::from_secs(365 * 24 * 60 * 60),
        interval: Duration::from_secs(1),
    };
    let stats = state.sweep_idle_caches(now, config).await;
    assert_eq!(stats.session_head_evicted, 1);
    assert!(state
        .sessions
        .session_head_cache
        .lock()
        .await
        .get(&session.id)
        .is_none());
    assert!(state
        .sessions
        .broadcasters
        .lock()
        .await
        .get(&session.id)
        .is_some());
    assert!(state
        .sessions
        .schedulers
        .lock()
        .await
        .get(&session.id)
        .is_some());
}

use super::*;

#[tokio::test]
async fn startup_provider_status_refresh_runs_in_background() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let adapter = Arc::new(BlockingInspectAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("blocking".into(), adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));

    spawn_startup_provider_status_refresh(ProviderStatusHandle::new(
        temp.path().to_path_buf(),
        Arc::clone(&state.providers),
        Arc::clone(&state.plugins),
        state.telemetry.ops_events.clone(),
    ));

    tokio::time::timeout(Duration::from_secs(1), async {
        while !adapter.inspect_started.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup refresh should begin inspect work");

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        state.providers.provider_status_count().await == 0,
        "provider status refresh should no longer block startup"
    );

    adapter.release_inspect.notify_waiters();

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if state.providers.has_provider_status("blocking").await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background provider status refresh should complete after inspect unblocks");
}

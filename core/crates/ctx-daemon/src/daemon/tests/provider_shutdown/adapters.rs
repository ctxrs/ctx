use super::*;

#[tokio::test]
async fn collect_provider_adapters_for_shutdown_includes_root_and_target_adapters() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let root_adapter = Arc::new(RecordingProviderAdapter::default());
    let target_adapter = Arc::new(RecordingProviderAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("root".into(), root_adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    state
        .providers
        .upsert_target_provider_adapter("root@host".into(), target_adapter.clone())
        .await;

    let adapters = state
        .providers
        .provider_worker_adapters_for_shutdown()
        .await;
    let mut ids = adapters.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    ids.sort();

    assert_eq!(ids, vec!["root".to_string(), "root@host".to_string()]);
}

#[tokio::test]
async fn shutdown_provider_adapters_requests_immediate_restart_for_all_adapters() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let root_adapter = Arc::new(RecordingProviderAdapter::default());
    let target_adapter = Arc::new(RecordingProviderAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("root".into(), root_adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    state
        .providers
        .upsert_target_provider_adapter("root@host".into(), target_adapter.clone())
        .await;

    state
        .providers
        .shutdown_provider_adapters("test shutdown")
        .await;

    assert_eq!(
        root_adapter.restart_calls(),
        vec![("test shutdown".to_string(), ProviderRestartMode::Immediate)]
    );
    assert_eq!(
        target_adapter.restart_calls(),
        vec![("test shutdown".to_string(), ProviderRestartMode::Immediate)]
    );
}

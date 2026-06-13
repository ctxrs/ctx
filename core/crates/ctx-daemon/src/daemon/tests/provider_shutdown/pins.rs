use super::*;

#[tokio::test]
async fn running_state_updates_provider_worker_pin_once_per_transition() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let adapter = Arc::new(RecordingProviderAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("root".into(), adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    let session_id = ctx_core::ids::SessionId(uuid::Uuid::new_v4());

    state
        .task_session_cleanup
        .set_running(session_id, true)
        .await;
    state
        .task_session_cleanup
        .set_running(session_id, true)
        .await;
    state
        .task_session_cleanup
        .set_running(session_id, false)
        .await;
    state
        .task_session_cleanup
        .set_running(session_id, false)
        .await;

    assert_eq!(
        adapter.pin_calls(),
        vec![
            (session_id.0.to_string(), true),
            (session_id.0.to_string(), false),
        ]
    );
}

#[tokio::test]
async fn attachment_state_updates_provider_worker_pin_once_per_connection_lifecycle() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let adapter = Arc::new(RecordingProviderAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("root".into(), adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    let session_id = ctx_core::ids::SessionId(uuid::Uuid::new_v4());

    state.task_session_cleanup.attach_session(session_id).await;
    state.task_session_cleanup.attach_session(session_id).await;
    state.task_session_cleanup.detach_session(session_id).await;
    state.task_session_cleanup.detach_session(session_id).await;

    assert_eq!(
        adapter.pin_calls(),
        vec![
            (session_id.0.to_string(), true),
            (session_id.0.to_string(), false),
        ]
    );
}

#[tokio::test]
async fn running_and_attachment_leases_share_one_provider_pin_state() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let adapter = Arc::new(RecordingProviderAdapter::default());
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("root".into(), adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    let session_id = ctx_core::ids::SessionId(uuid::Uuid::new_v4());

    state
        .task_session_cleanup
        .set_running(session_id, true)
        .await;
    state.task_session_cleanup.attach_session(session_id).await;
    state
        .task_session_cleanup
        .set_running(session_id, false)
        .await;
    state.task_session_cleanup.detach_session(session_id).await;

    assert_eq!(
        adapter.pin_calls(),
        vec![
            (session_id.0.to_string(), true),
            (session_id.0.to_string(), false),
        ]
    );
}

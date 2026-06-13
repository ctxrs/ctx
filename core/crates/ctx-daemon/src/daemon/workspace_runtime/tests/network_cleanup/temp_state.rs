use super::*;

#[test]
fn sandbox_machine_temp_state_paths_match_expected_names() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let paths = sandbox_machine_temp_state_paths(data_root.path(), "ctx");
    let rendered: Vec<String> = paths
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let expected_tmp_prefix = sandbox_machine_temp_root(data_root.path())
        .join("sandbox-cli")
        .to_string_lossy()
        .to_string();
    assert!(rendered.iter().any(|p| p.starts_with(&expected_tmp_prefix)));
    assert!(rendered
        .iter()
        .any(|p| p.ends_with("sandbox-cli/gvproxy.pid")));
    assert!(rendered
        .iter()
        .any(|p| p.ends_with("sandbox-cli/ctx-api.sock")));
    assert!(rendered
        .iter()
        .any(|p| p.ends_with("sandbox-cli/ctx-gvproxy.sock")));
    assert!(rendered.iter().any(|p| p.ends_with("sandbox-cli/ctx.sock")));
    assert!(rendered
        .iter()
        .any(|p| p.ends_with("home/.sandbox-cli/ctx-api.sock")));
    assert!(rendered
        .iter()
        .any(|p| p.ends_with("home/.sandbox-cli/ctx-gvproxy.sock")));
}

#[tokio::test]
async fn sandbox_machine_singleflight_lock_reuses_lock_for_same_machine() {
    let first = sandbox_machine_singleflight_lock("ctx-machine-a");
    let second = sandbox_machine_singleflight_lock("ctx-machine-a");
    assert!(Arc::ptr_eq(&first, &second));

    let guard = first.lock().await;
    assert!(second.try_lock().is_err());
    drop(guard);
    assert!(second.try_lock().is_ok());
}

#[tokio::test]
async fn sandbox_machine_singleflight_lock_isolated_by_machine_name() {
    let first = sandbox_machine_singleflight_lock("ctx-machine-b");
    let second = sandbox_machine_singleflight_lock("ctx-machine-c");
    assert!(!Arc::ptr_eq(&first, &second));

    let _guard = first.lock().await;
    assert!(second.try_lock().is_ok());
}

use super::*;

fn refresh_request(mode: &str) -> Value {
    json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "mode": mode,
        "operation": "refresh",
    })
}

fn queued_scope(coordinator: &CoreRefreshEngine, response: &Value) -> SourceBackedRefreshScope {
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .expect("queued request ID");
    let state = coordinator.lock_state();
    find_attempt(&state, request_id)
        .expect("queued refresh attempt")
        .refresh_scope
        .clone()
}

#[test]
fn repeated_default_background_searches_schedule_zero_healthy_route_scans() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();

    for _ in 0..3 {
        let response = coordinator
            .handle_ipc_request(temp.path(), &refresh_request("background"))
            .unwrap()
            .expect("maintenance wake response");
        assert_eq!(response["maintenance_wake"], true);
        assert_eq!(response["progress"]["phase"], "maintenance_wake");
        assert!(!coordinator.has_pending_request());
    }
}

#[test]
fn explicit_refresh_wait_retains_full_frontier_scope() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();
    let response = coordinator
        .handle_ipc_request(temp.path(), &refresh_request("wait"))
        .unwrap()
        .expect("wait response");

    assert!(matches!(
        queued_scope(&coordinator, &response),
        SourceBackedRefreshScope::All
    ));
}

#[test]
fn explicit_import_wait_retains_full_frontier_scope() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let coordinator = CoreRefreshEngine::new();
    let request = json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "mode": "wait",
        "operation": "import",
        "explicit_source_catalog": catalog.to_json(),
    });
    let response = coordinator
        .handle_ipc_request(temp.path(), &request)
        .unwrap()
        .expect("import response");

    assert!(matches!(
        queued_scope(&coordinator, &response),
        SourceBackedRefreshScope::All
    ));
}

#[test]
fn source_refresh_operation_is_required_protocol_authority() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();
    let mut request = refresh_request("background");
    request.as_object_mut().unwrap().remove("operation");

    let error = coordinator
        .handle_ipc_request(temp.path(), &request)
        .unwrap_err();
    assert!(format!("{error:#}").contains("operation is missing"));
}

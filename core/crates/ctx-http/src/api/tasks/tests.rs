use serde_json::json;

use ctx_route_contracts::tasks::{CreateTaskRouteRequest, CreateTaskSessionRouteRequest};

#[test]
fn create_session_req_accepts_execution_environment() {
    serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "execution_environment": "sandbox",
    }))
    .unwrap();
}

#[test]
fn create_session_req_rejects_legacy_env_target_alias() {
    let err = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "env_target": "local"
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn create_session_req_rejects_legacy_execution_environment_values() {
    let err = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "execution_environment": "worktree"
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown execution environment"));
}

#[test]
fn create_session_req_rejects_empty_model_id() {
    let err = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "   "
    }))
    .unwrap_err();

    assert!(err.to_string().contains("model_id must not be empty"));
}

#[test]
fn create_session_req_rejects_default_placeholder_model_id() {
    let err = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "default"
    }))
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("model_id must be a concrete model id"));
}

#[test]
fn create_session_req_rejects_empty_provider_id() {
    let err = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "   ",
        "model_id": "fake-model"
    }))
    .unwrap_err();

    assert!(err.to_string().contains("provider_id must not be empty"));
}

#[test]
fn create_task_req_rejects_legacy_default_session_flag() {
    let err = serde_json::from_value::<CreateTaskRouteRequest>(json!({
        "title": "task",
        "create_default_session": false
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn create_task_req_accepts_default_session_options() {
    serde_json::from_value::<CreateTaskRouteRequest>(json!({
        "title": "task",
        "default_session": {
            "provider_id": "fake",
            "model_id": "fake-model",
            "execution_environment": "host"
        }
    }))
    .unwrap();
}

use ctx_daemon_cli::SemanticCompletionError;
use serde_json::{json, Value};

pub(super) fn structured(error: &SemanticCompletionError) -> Value {
    let detail = error.to_string();
    let mut structured = json!({
        "error": detail,
        "error_code": "semantic_completion_failed",
        "reason": error.code(),
        "generation_id": error.generation_id(),
        "core_published": true,
        "retryable": error.retryable(),
        "detail": detail,
    });
    let fields = structured
        .as_object_mut()
        .expect("semantic completion error JSON must be an object");
    match error {
        SemanticCompletionError::CoreSuperseded {
            active_generation_id,
            ..
        } => {
            fields.insert(
                "active_generation_id".to_owned(),
                Value::String(active_generation_id.clone()),
            );
        }
        SemanticCompletionError::DaemonJobFailed {
            failure_class: Some(failure_class),
            ..
        } => {
            fields.insert(
                "failure_class".to_owned(),
                Value::String(failure_class.clone()),
            );
        }
        _ => {}
    }
    structured
}

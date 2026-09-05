use super::*;

pub(super) fn structured_producer_error(value: &Value) -> Option<AgentHistoryError> {
    let code = value
        .get("error_code")?
        .as_str()
        .filter(|code| !code.is_empty())?;
    let retryable = match value.get("retryable") {
        Some(value) => value.as_bool()?,
        None => false,
    };
    let broad_code = serde_json::from_value(Value::String(code.to_owned()))
        .unwrap_or(AgentHistoryErrorCode::AdapterError);
    let message = value
        .get("detail")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .unwrap_or(code);
    let mut error = AgentHistoryError::new(broad_code, message, retryable);
    error.body.details = Some(BTreeMap::from([(
        "producerError".to_owned(),
        value.clone(),
    )]));
    Some(error)
}

pub(super) fn cli_failure(stderr: &str) -> AgentHistoryError {
    exact_json::parse_json_value_exact(stderr.as_bytes())
        .ok()
        .and_then(|value| structured_producer_error(&value))
        .unwrap_or_else(|| AgentHistoryError::new(classify_stderr(stderr), stderr.trim(), false))
        .with_cause(stderr)
}

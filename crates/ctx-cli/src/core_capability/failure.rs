use super::*;

pub(super) fn produce_response(
    input: Vec<u8>,
    execute_request: impl FnOnce(Request) -> Result<Value>,
) -> Result<(Vec<u8>, Option<anyhow::Error>)> {
    let request = parse_frame(input)?;
    let operation = request.operation;
    let (response, terminal_error) = match execute_request(request) {
        Ok(response) => (response, None),
        Err(error) => match terminal_failure_response(operation, &error) {
            Some(response) => (response, Some(error)),
            None => return Err(error),
        },
    };
    let bytes = canonical(&response)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(anyhow!("response exceeds bound"));
    }
    Ok((bytes, terminal_error))
}

fn terminal_failure_response(operation: Operation, error: &anyhow::Error) -> Option<Value> {
    use super::progress_events::terminal_details;

    let terminal = error.chain().find_map(|cause| {
        cause.downcast_ref::<crate::semantic::SourceBackedRefreshTerminalError>()
    })?;
    let outcome = terminal.outcome();
    if !outcome.code().is_failure() {
        return None;
    }
    // Keep arbitrary display/detail text behind the boundary. Only the
    // terminal type's validated structured fields enter the failure frame.
    Some(json!({
        "details": terminal_details(outcome),
        "error_code": outcome.code().as_str(),
        "ok": false,
        "operation": operation.name(),
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "retryable": outcome.retryable(),
        "schema_version": 1,
    }))
}

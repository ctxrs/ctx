//! Newline-delimited MCP application delivery, independent of process setup.

use std::{
    collections::HashSet,
    io::{BufRead, Write},
    time::{Duration, Instant},
};

use anyhow::Error;
use ctx_agent_integrations::{
    mcp::{
        encode_response_line, error_response, handle_protocol_message, read_mcp_input_line,
        McpInputLine, McpServerIdentity, McpToolKind, McpUsage, RequestDescriptor,
        MCP_MAX_LINE_BYTES, MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    tool_backend::{
        OpaqueMcpDeliveryOutcome, OpaqueMcpProxyError, ToolBackend, ToolSearchFailurePhase,
        ToolUsageFacts,
    },
};
use ctx_client_observability::analytics::{McpErrorClassV1, McpStopReasonV1, Outcome};
use serde_json::{json, Value};

mod telemetry;
mod text;

pub use telemetry::McpTelemetry;
pub use text::render_generic_tool_text;

/// Product-owned MCP identity. The application crate never derives it from its
/// own package metadata, because wire identity belongs to the executable.
#[derive(Debug, Clone, Copy)]
pub struct ProductIdentity<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// Product-owned compact text projection for MCP tool results.
pub trait McpTextPort {
    fn render_tool_text(&self, value: &Value) -> String;
}

impl<F> McpTextPort for F
where
    F: Fn(&Value) -> String,
{
    fn render_tool_text(&self, value: &Value) -> String {
        self(value)
    }
}

/// Product-owned local usage persistence. The application invokes this once,
/// only after the exact JSON-RPC response has been written and flushed.
pub trait McpUsagePort {
    fn record_delivered(
        &mut self,
        operation: McpToolKind,
        usage: ToolUsageFacts,
        response: &Value,
        encoded_response_bytes: usize,
        duration: Duration,
    );
}

/// A transport failure classified for content-free MCP lifecycle telemetry.
#[derive(Debug)]
pub struct McpServeFailure {
    pub reason: McpStopReasonV1,
    error: Error,
}

impl McpServeFailure {
    pub fn into_error(self) -> Error {
        self.error
    }
}

/// Serves MCP on already-opened stdio ports. Process setup, configuration,
/// daemon policy, concrete backends, usage storage, and error presentation stay
/// with the product adapter.
pub fn serve_stdio<B, T, U>(
    stdin: &mut impl BufRead,
    stdout: &mut impl Write,
    identity: ProductIdentity<'_>,
    backend: &B,
    text: &T,
    usage: &mut U,
    mut telemetry: McpTelemetry,
) -> Result<(), McpServeFailure>
where
    B: ToolBackend,
    T: McpTextPort,
    U: McpUsagePort,
{
    let started = Instant::now();
    let mut initialized = false;
    let result = serve_stdio_loop(
        stdin,
        stdout,
        identity,
        backend,
        text,
        usage,
        &mut telemetry,
        &mut initialized,
    );
    let (reason, outcome) = match &result {
        Ok(()) => (McpStopReasonV1::Eof, Outcome::Success),
        Err(failure) => (failure.reason, Outcome::Failure),
    };
    telemetry.stop(reason, outcome, started.elapsed());
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ports preserve the one-pass stdio loop"
)]
fn serve_stdio_loop<B, T, U>(
    stdin: &mut impl BufRead,
    stdout: &mut impl Write,
    identity: ProductIdentity<'_>,
    backend: &B,
    text: &T,
    usage_port: &mut U,
    telemetry: &mut McpTelemetry,
    initialized: &mut bool,
) -> Result<(), McpServeFailure>
where
    B: ToolBackend,
    T: McpTextPort,
    U: McpUsagePort,
{
    loop {
        let input = read_mcp_input_line(stdin).map_err(|error| McpServeFailure {
            reason: McpStopReasonV1::StdinReadError,
            error,
        })?;
        let Some(input) = input else {
            return Ok(());
        };
        let request_started = Instant::now();
        let mut companion_completion = None;
        let (handled, descriptor) = match input {
            McpInputLine::Line(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(message) => {
                        let descriptor = RequestDescriptor::from_message(&message);
                        if *initialized
                            && matches!(
                                descriptor,
                                RequestDescriptor::ToolCall { operation }
                                    if operation.is_companion_owned()
                            )
                        {
                            let response = match backend.proxy_companion_mcp(line.as_bytes()) {
                                Ok(response) => response,
                                Err(error) => {
                                    let encoded = encode_response_line(
                                        &companion_proxy_error_response(&message, error),
                                    )
                                    .map_err(|error| McpServeFailure {
                                        reason: McpStopReasonV1::ResponseSerializeError,
                                        error: error.into(),
                                    })?;
                                    stdout.write_all(encoded.as_bytes()).map_err(|error| {
                                        McpServeFailure {
                                            reason: McpStopReasonV1::StdoutWriteError,
                                            error: error.into(),
                                        }
                                    })?;
                                    stdout.flush().map_err(|error| McpServeFailure {
                                        reason: McpStopReasonV1::StdoutFlushError,
                                        error: error.into(),
                                    })?;
                                    telemetry.record_delivered(
                                        descriptor,
                                        None,
                                        None,
                                        request_started.elapsed(),
                                    );
                                    continue;
                                }
                            };
                            if let Err(error) = stdout.write_all(response.response_bytes()) {
                                response.finish(OpaqueMcpDeliveryOutcome::OutputFailed);
                                return Err(McpServeFailure {
                                    reason: McpStopReasonV1::StdoutWriteError,
                                    error: error.into(),
                                });
                            }
                            if let Err(error) = stdout.flush() {
                                response.finish(OpaqueMcpDeliveryOutcome::OutputFailed);
                                return Err(McpServeFailure {
                                    reason: McpStopReasonV1::StdoutFlushError,
                                    error: error.into(),
                                });
                            }
                            response.finish(OpaqueMcpDeliveryOutcome::WrittenAndFlushed);
                            telemetry.record_delivered(
                                descriptor,
                                None,
                                None,
                                request_started.elapsed(),
                            );
                            continue;
                        }
                        let mut handled = handle_protocol_message(
                            message.clone(),
                            descriptor,
                            initialized,
                            McpServerIdentity {
                                name: identity.name,
                                version: identity.version,
                            },
                            backend,
                            |value| text.render_tool_text(value),
                        );
                        if *initialized && descriptor == RequestDescriptor::ToolsList {
                            if let Some(response) = handled.value.as_mut() {
                                companion_completion = append_companion_tool_definitions(
                                    &message,
                                    line.as_bytes(),
                                    response,
                                    backend,
                                );
                            }
                        }
                        (handled, descriptor)
                    }
                    Err(error) => (
                        ctx_agent_integrations::mcp::McpHandled::plain(Some(error_response(
                            Value::Null,
                            -32700,
                            "Parse error",
                            Some(json!({ "error": error.to_string() })),
                        ))),
                        RequestDescriptor::InvalidJson,
                    ),
                }
            }
            McpInputLine::InvalidUtf8 => (
                ctx_agent_integrations::mcp::McpHandled::plain(Some(error_response(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({ "error": "MCP message is not valid UTF-8" })),
                ))),
                RequestDescriptor::InvalidUtf8,
            ),
            McpInputLine::TooLarge => (
                ctx_agent_integrations::mcp::McpHandled::plain(Some(error_response(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({
                        "error": format!("MCP message exceeds max line bytes ({MCP_MAX_LINE_BYTES})")
                    })),
                ))),
                RequestDescriptor::LineTooLarge,
            ),
        };
        let response = handled.value;
        let mut delivered_usage = handled.usage;
        if let Some(response) = response {
            let encoded = match encode_response_line(&response) {
                Ok(encoded) => encoded,
                Err(error) => {
                    mark_search_failure(&mut delivered_usage, ToolSearchFailurePhase::Render, None);
                    telemetry.record_response_failure(
                        descriptor,
                        request_started.elapsed(),
                        McpErrorClassV1::ResponseSerialize,
                        delivered_usage.as_ref().map(|usage| &usage.facts),
                    );
                    return Err(McpServeFailure {
                        reason: McpStopReasonV1::ResponseSerializeError,
                        error: error.into(),
                    });
                }
            };
            let output_started = Instant::now();
            if let Err(error) = stdout.write_all(encoded.as_bytes()) {
                if let Some(completion) = companion_completion.take() {
                    completion.finish(OpaqueMcpDeliveryOutcome::OutputFailed);
                }
                mark_search_failure(
                    &mut delivered_usage,
                    ToolSearchFailurePhase::Output,
                    Some(output_started.elapsed()),
                );
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseWrite,
                    delivered_usage.as_ref().map(|usage| &usage.facts),
                );
                return Err(McpServeFailure {
                    reason: McpStopReasonV1::StdoutWriteError,
                    error: error.into(),
                });
            }
            if let Err(error) = stdout.flush() {
                if let Some(completion) = companion_completion.take() {
                    completion.finish(OpaqueMcpDeliveryOutcome::OutputFailed);
                }
                mark_search_failure(
                    &mut delivered_usage,
                    ToolSearchFailurePhase::Output,
                    Some(output_started.elapsed()),
                );
                telemetry.record_response_failure(
                    descriptor,
                    request_started.elapsed(),
                    McpErrorClassV1::ResponseFlush,
                    delivered_usage.as_ref().map(|usage| &usage.facts),
                );
                return Err(McpServeFailure {
                    reason: McpStopReasonV1::StdoutFlushError,
                    error: error.into(),
                });
            }
            if let Some(completion) = companion_completion.take() {
                completion.finish(OpaqueMcpDeliveryOutcome::WrittenAndFlushed);
            }
            mark_search_output_completed(&mut delivered_usage, output_started.elapsed());
            let duration = request_started.elapsed();
            let telemetry_usage = delivered_usage.as_ref().map(|usage| usage.facts);
            if let Some(McpUsage { operation, facts }) = delivered_usage {
                usage_port.record_delivered(operation, facts, &response, encoded.len(), duration);
            }
            telemetry.record_delivered(
                descriptor,
                Some(&response),
                telemetry_usage.as_ref(),
                duration,
            );
        } else {
            telemetry.record_delivered(descriptor, None, None, request_started.elapsed());
        }
    }
}

fn mark_search_failure(
    usage: &mut Option<McpUsage>,
    phase: ToolSearchFailurePhase,
    output_duration: Option<Duration>,
) {
    let Some(search) = usage
        .as_mut()
        .and_then(|usage| usage.facts.search_execution.as_mut())
    else {
        return;
    };
    search.output_served = Some(false);
    if let Some(duration) = output_duration {
        search.output_duration = Some(duration);
    }
    search.failure_phase = Some(phase);
}

fn mark_search_output_completed(usage: &mut Option<McpUsage>, output_duration: Duration) {
    let Some(search) = usage
        .as_mut()
        .and_then(|usage| usage.facts.search_execution.as_mut())
    else {
        return;
    };
    search.output_duration = Some(output_duration);
    search.output_served = Some(true);
}

fn append_companion_tool_definitions<B: ToolBackend>(
    message: &Value,
    raw_request: &[u8],
    response: &mut Value,
    backend: &B,
) -> Option<ctx_agent_integrations::tool_backend::OpaqueMcpProxyResponse> {
    let core_tools = response
        .pointer("/result/tools")
        .and_then(Value::as_array)?;
    let mut merged_tools = validated_core_tools(core_tools)?;
    let Ok(companion_response) = backend.proxy_companion_mcp(raw_request) else {
        return None;
    };
    let companion_tools = validated_companion_tools(
        companion_response.response_bytes(),
        message.get("id"),
        &merged_tools,
    )?;
    merged_tools.extend(companion_tools);

    let mut merged_response = response.clone();
    let tools = merged_response
        .pointer_mut("/result/tools")
        .and_then(Value::as_array_mut)?;
    *tools = merged_tools;
    let within_bound = serde_json::to_vec(&merged_response)
        .is_ok_and(|encoded| encoded.len().saturating_add(1) <= MCP_PRESENTATION_MAX_OUTPUT_BYTES);
    if within_bound {
        *response = merged_response;
        Some(companion_response)
    } else {
        None
    }
}

fn validated_core_tools(tools: &[Value]) -> Option<Vec<Value>> {
    let mut names = HashSet::new();
    for tool in tools {
        let name = tool.as_object()?.get("name")?.as_str()?;
        if name.is_empty() || !names.insert(name.to_owned()) {
            return None;
        }
    }
    Some(tools.to_vec())
}

fn validated_companion_tools(
    encoded: &[u8],
    expected_id: Option<&Value>,
    core_tools: &[Value],
) -> Option<Vec<Value>> {
    if encoded.is_empty() || encoded.len() > MCP_MAX_LINE_BYTES {
        return None;
    }
    let frame = encoded.strip_suffix(b"\n")?;
    if frame.is_empty() || frame.contains(&b'\n') || frame.ends_with(b"\r") {
        return None;
    }
    let response: Value = serde_json::from_slice(frame).ok()?;
    let object = response.as_object()?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id") != expected_id
        || object.contains_key("error")
    {
        return None;
    }
    let tools = object
        .get("result")?
        .as_object()?
        .get("tools")?
        .as_array()?;
    let mut names = core_tools
        .iter()
        .map(|tool| tool.get("name")?.as_str().map(ToOwned::to_owned))
        .collect::<Option<HashSet<_>>>()?;
    let mut validated = Vec::with_capacity(tools.len());
    for tool in tools {
        let name = tool.as_object()?.get("name")?.as_str()?;
        if name.is_empty()
            || !McpToolKind::from_tool_name(Some(name)).is_companion_owned()
            || !names.insert(name.to_owned())
        {
            return None;
        }
        validated.push(tool.clone());
    }
    Some(validated)
}

fn companion_proxy_error_response(message: &Value, error: OpaqueMcpProxyError) -> Value {
    let (code, retryable) = match error {
        OpaqueMcpProxyError::CompanionUnavailable => ("companion_unavailable", true),
        OpaqueMcpProxyError::CompanionIncompatible => ("companion_incompatible", false),
    };
    let result = json!({
        "isError": true,
        "content": [{"type": "text", "text": code}],
        "structuredContent": {
            "error": code,
            "error_code": code,
            "retryable": retryable,
        },
    });
    json!({
        "jsonrpc": "2.0",
        "id": message.get("id").cloned().unwrap_or(Value::Null),
        "result": result,
    })
}

#[cfg(test)]
mod tests;

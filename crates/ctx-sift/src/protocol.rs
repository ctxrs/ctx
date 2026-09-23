// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use crate::{command_view, hooks, jev, semantic, state};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sift::{CompactResult, Compactor, Encoding};
use std::io::{BufRead, Read, Write};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u64,
    text: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default = "default_complete")]
    complete: bool,
    #[serde(default)]
    tokenizer: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    id: u64,
    request: Request,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    delivered_view: bool,
    #[serde(default)]
    selection: Option<serde_json::Value>,
}

type ProtocolRecord = Option<(state::Event, String, bool)>;

#[derive(Serialize)]
struct SessionResponse {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: Option<bool>,
    #[serde(flatten)]
    response: Response,
}

fn default_complete() -> bool {
    true
}

#[derive(Serialize)]
struct Response {
    version: u8,
    #[serde(flatten)]
    result: CompactResult,
}

pub(crate) fn protocol(
    input: impl BufRead,
    mut output: impl Write,
    source: Option<&str>,
    tool: Option<&str>,
    session: u8,
) -> Result<bool> {
    let compactor = Compactor::new().context("cannot initialize tokenizer")?;
    let settings = source
        .filter(|_| session == 0)
        .map(|_| state::Settings::load())
        .transpose()?;
    let project = (session == 2)
        .then(|| {
            std::env::current_dir()
                .ok()
                .and_then(|path| state::project_at(&path).ok())
        })
        .flatten();
    let mut jev = jev::Client::new();
    if session != 0 {
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({"version":1,"session":session}),
        )?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    let mut input = input;
    let mut line = Vec::new();
    let mut failed = false;
    let mut last_id = 0;
    loop {
        line.clear();
        let read = if session != 0 {
            // JSON escaping can expand an 8 MiB text by six; bound framing too.
            let read = input
                .by_ref()
                .take(48 * 1024 * 1024 + 1025)
                .read_until(b'\n', &mut line)?;
            ensure!(
                line.len() <= 48 * 1024 * 1024 + 1024,
                "session request too large"
            );
            read
        } else {
            input.read_until(b'\n', &mut line)?
        };
        if read == 0 {
            break;
        }
        let mut id = None;
        let mut semantic = false;
        let result = (|| -> Result<(Response, ProtocolRecord)> {
            let invalid = |error: serde_json::Error| {
                anyhow::anyhow!(
                    "invalid JSON request at line {}, column {}",
                    error.line(),
                    error.column()
                )
            };
            let mut raw = false;
            let mut semantic_argv = None;
            let mut semantic_request = None;
            let mut request_tool = tool.map(str::to_owned);
            let request: Request = if session != 0 {
                let envelope: SessionRequest = serde_json::from_slice(&line).map_err(invalid)?;
                ensure!(
                    envelope.id > last_id && envelope.id <= 9_007_199_254_740_991,
                    "session ID must increase within the safe integer range"
                );
                last_id = envelope.id;
                id = Some(envelope.id);
                ensure!(
                    envelope.request.text.len() + envelope.command.as_ref().map_or(0, String::len)
                        <= 8 * 1024 * 1024,
                    "session command and text too large"
                );
                let argv = envelope.command.as_deref().and_then(hooks::literal_argv);
                raw = argv.as_deref().is_some_and(hooks::explicit_raw);
                if envelope.delivered_view {
                    semantic_argv = argv;
                }
                ensure!(
                    session == 2 || envelope.selection.is_none(),
                    "session-v1 does not accept semantic selection"
                );
                ensure!(
                    session == 2 || envelope.tool.is_none(),
                    "session-v1 does not accept a per-request tool"
                );
                if session == 2 {
                    ensure!(
                        matches!(envelope.tool.as_deref(), Some("bash" | "grep")),
                        "session-v2 requires a supported per-request tool"
                    );
                    request_tool.clone_from(&envelope.tool);
                }
                semantic_request = (request_tool.as_deref() == Some("grep"))
                    .then_some(envelope.selection)
                    .flatten();
                envelope.request
            } else {
                serde_json::from_slice(&line).map_err(invalid)?
            };
            let fresh_settings = if session != 0 {
                source.map(|_| state::Settings::load()).transpose()?
            } else {
                None
            };
            let settings = if session != 0 {
                &fresh_settings
            } else {
                &settings
            };
            ensure!(
                request.version == 1,
                "unsupported protocol version; expected 1"
            );
            ensure!(
                request
                    .tokenizer
                    .as_deref()
                    .is_none_or(|name| name == "o200k_base"),
                "unsupported tokenizer; expected o200k_base"
            );
            // Error/completion flags never authorize omissions or establish an
            // exit status. Only the explicit Pi session contract permits a view.
            let _ = (request.is_error, request.complete);
            let started = std::time::Instant::now();
            let mut result = if !raw
                && settings.as_ref().is_none_or(|s| {
                    s.enabled && request_tool.as_deref().is_none_or(|name| !s.excludes(name))
                }) {
                let mut original = compactor.compact(&request.text);
                if let Some(proposal) = semantic_argv
                    .as_deref()
                    .and_then(|argv| command_view::delivered_candidate(argv, &request.text))
                {
                    let selected = compactor.compact(&proposal);
                    if selected.output_tokens < original.output_tokens {
                        original = CompactResult {
                            input_tokens: original.input_tokens,
                            ..selected
                        };
                        semantic = true;
                    }
                }
                original
            } else {
                let tokens = compactor.count_tokens(&request.text);
                CompactResult {
                    text: request.text.clone(),
                    encoding: Encoding::Raw,
                    input_tokens: tokens,
                    output_tokens: tokens,
                }
            };
            let ordinary = result.clone();
            let mut jev_selected = false;
            if !raw
                && session == 2
                && settings.as_ref().is_some_and(|s| {
                    s.enabled && request_tool.as_deref().is_none_or(|name| !s.excludes(name))
                })
                && let Some(selection) = semantic_request
                && let Some(selected) = semantic::apply(
                    selection,
                    &request.text,
                    &ordinary,
                    settings.as_ref().unwrap(),
                    project.as_deref(),
                    &compactor,
                    &mut jev,
                )
            {
                result = selected;
                semantic = true;
                jev_selected = true;
            }
            let accounted = if jev_selected { &ordinary } else { &result };
            let record = source.filter(|_| !raw).map(|source| {
                let event = state::Event {
                    unix_millis: state::unix_millis(),
                    command: request_tool
                        .as_deref()
                        .or(tool)
                        .unwrap_or("tool-output")
                        .into(),
                    input_tokens: Some(accounted.input_tokens as u64),
                    output_tokens: Some(accounted.output_tokens as u64),
                    input_bytes: request.text.len() as u64,
                    output_bytes: accounted.text.len() as u64,
                    duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    exit_code: None,
                    source: Some(source.into()),
                    original_id: None,
                };
                (event, request.text, jev_selected)
            });
            Ok((Response { version: 1, result }, record))
        })();
        let record = match result {
            Ok((response, record)) => {
                if let Some(id) = id {
                    serde_json::to_writer(
                        &mut output,
                        &SessionResponse {
                            id,
                            semantic: semantic.then_some(true),
                            response,
                        },
                    )?;
                } else {
                    serde_json::to_writer(&mut output, &response)?;
                }
                record
            }
            Err(error) => {
                failed = true;
                serde_json::to_writer(
                    &mut output,
                    &serde_json::json!({"version": 1, "error": error.to_string()}),
                )?;
                None
            }
        };
        output.write_all(b"\n")?;
        output.flush()?;
        if let Some((event, original, jev_selected)) = record {
            // Count only successfully delivered responses. Optional storage
            // cannot replace output or turn successful delivery into failure.
            let _ = state::record(event, (!jev_selected).then_some((original.as_bytes(), &[])));
        }
        if session != 0 {
            // Completion acknowledges the recording attempt, never storage or
            // provider delivery. Errors retire the session before another request.
            ensure!(!failed, "session request failed");
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({"version":1,"id":id,"done":true}),
            )?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(failed)
}

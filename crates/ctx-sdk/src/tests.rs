use super::*;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::process::Child;
#[cfg(unix)]
use std::{thread, time::Instant};

#[cfg(unix)]
fn spawn_json_shell(body: &str) -> Child {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", body])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_configured(&mut command).unwrap()
}

#[cfg(unix)]
fn spawn_mcp_shell(body: &str) -> Child {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", body])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_configured(&mut command).unwrap()
}

#[cfg(unix)]
fn make_test_executable(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    // Bazel's sandbox can briefly report ETXTBSY immediately after publishing
    // a generated executable. Keep that filesystem race out of subprocess tests.
    thread::sleep(Duration::from_millis(25));
}

#[cfg(unix)]
fn run_json_shell(body: &str, timeout: Duration) -> Result<Value, AgentHistoryError> {
    run_ctx_json(
        &LocalBackendConfig {
            ctx_binary: PathBuf::from("/bin/sh"),
            data_root: None,
            env: BTreeMap::new(),
            timeout,
        },
        &["-c".to_owned(), body.to_owned()],
    )
}

#[test]
fn reads_shared_search_fixture() {
    let value: AgentHistoryEnvelope = serde_json::from_str(include_str!(
        "../../../contracts/agent-history-v1/fixtures/search.results.json"
    ))
    .unwrap();
    assert_eq!(value.contract_version, CONTRACT_VERSION);
    assert_eq!(value.operation, AgentHistoryOperation::Search);
    let search = value.search.unwrap();
    assert_eq!(search.query.as_deref(), Some("local agent history"));
    assert_eq!(search.results.len(), 1);
    assert_eq!(
        search.result_window,
        Some(SearchResultWindow {
            limit: 20,
            returned: 1,
            more_available: false,
            extra: JsonObject::new(),
        })
    );
    assert_eq!(search.pagination.as_ref().unwrap()["limit"], 20);
    assert_eq!(search.pagination.as_ref().unwrap()["hasMore"], false);
    assert_eq!(
        search.results[0].ctx_event_id.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(
        search.results[0].provider_session_id.as_deref(),
        Some("codex-fixture-session")
    );
    assert_eq!(
        search.results[0].source_format.as_deref(),
        Some("codex_session_jsonl")
    );
}

#[test]
fn show_normalization_exposes_core_identity_and_content() {
    let event = normalize_event(&json!({
        "event": {
            "ctx_event_id": "event-1",
            "ctx_session_id": "session-1",
            "provider": "codex",
            "provider_session_id": "codex-resume-uuid",
            "source_format": "codex_session_jsonl",
            "text": "complete body",
            "content": {
                "complete": true,
                "policy_status": "selected"
            }
        },
        "events": []
    }))
    .unwrap();
    let selected = event.event.expect("selected event");
    assert_eq!(selected.provider.as_deref(), Some("codex"));
    assert_eq!(
        selected.provider_session_id.as_deref(),
        Some("codex-resume-uuid")
    );
    assert_eq!(
        selected.source_format.as_deref(),
        Some("codex_session_jsonl")
    );
    assert_eq!(selected.text.as_deref(), Some("complete body"));
    let content = selected.content.expect("typed Core content metadata");
    assert!(content.complete);
    assert_eq!(content.policy_status, CoreContentPolicyStatus::Selected);

    let session = normalize_session(&json!({
        "session": {
            "ctx_session_id": "session-1",
            "provider": "codex",
            "provider_session_id": "codex-resume-uuid",
            "source_format": "codex_session_jsonl"
        },
        "events": [],
        "mode": "lite",
        "format": "json"
    }))
    .unwrap();
    let summary = session.session.expect("typed session summary");
    assert_eq!(summary.provider.as_deref(), Some("codex"));
    assert_eq!(
        summary.provider_session_id.as_deref(),
        Some("codex-resume-uuid")
    );
    assert_eq!(
        summary.source_format.as_deref(),
        Some("codex_session_jsonl")
    );
}

#[test]
fn show_normalization_exposes_typed_mcp_tool_call_metadata() {
    let canonical: AgentHistoryEnvelope = serde_json::from_str(include_str!(
        "../../../contracts/agent-history-v1/fixtures/show-event.mcp-tool-call.json"
    ))
    .unwrap();
    let canonical_result = canonical.event.unwrap();
    let canonical_selected = canonical_result.event.unwrap();
    let canonical_call = canonical_selected.mcp_tool_call.unwrap();
    assert_eq!(canonical_call.server, "mcp-サーバー-🦀");
    assert_eq!(canonical_call.tool, "検索/工具/🛠️");
    assert_eq!(
        canonical_selected.extra["futureEventField"]["preserved"],
        true
    );
    assert!(canonical_result.events[0].mcp_tool_call.is_none());

    let normalized = normalize_event(&json!({
        "event": {
            "ctx_event_id": "event-1",
            "mcp_tool_call": {
                "server": "mcp-サーバー-🦀",
                "tool": "検索/工具/🛠️"
            },
            "future_event_field": {"preserved": true}
        },
        "events": [{"ctx_event_id": "event-2"}]
    }))
    .unwrap();
    let selected = normalized.event.unwrap();
    let call = selected.mcp_tool_call.unwrap();
    assert_eq!(call.server, "mcp-サーバー-🦀");
    assert_eq!(call.tool, "検索/工具/🛠️");
    assert_eq!(selected.extra["futureEventField"]["preserved"], true);
    assert!(normalized.events[0].mcp_tool_call.is_none());

    for invalid in [
        json!({"server": "server", "tool": "tool", "future_label": true}),
        json!({"server": "", "tool": "tool"}),
        json!({"server": "server", "tool": "a".repeat(MAX_MCP_TOOL_CALL_COMPONENT_BYTES + 1)}),
    ] {
        assert!(
            normalize_event(&json!({"event": {"mcp_tool_call": invalid}, "events": []})).is_err()
        );
    }
}

#[test]
fn show_normalization_exposes_lossless_typed_mcp_exchange_content() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../contracts/agent-history-v1/fixtures/show-event.mcp-tool-call.json"
    ))
    .unwrap();
    let canonical = normalize_event(&fixture["event"]).unwrap();
    let canonical_selected = canonical.event.unwrap();
    let canonical_exchange = canonical_selected.mcp_exchange.as_ref().unwrap();
    assert_eq!(
        canonical_exchange.provider_call_id,
        "native-call-呼び出し-🦀"
    );
    let canonical_arguments = match &canonical_exchange.invocation.as_ref().unwrap().arguments {
        McpJsonCapture::Present { value } => value,
        other => panic!("expected present canonical arguments, got {other:?}"),
    };
    assert!(canonical_arguments["snake_key"][1].is_null());
    assert!(canonical_arguments["nested"]["items"][1]["deep_null"].is_null());

    let raw_arguments = json!({
        "snake_key": ["雪", null, {"camelKey": true}],
        "nested_object": {"deep_null": null, "items": [1, false]}
    });
    let raw_payload = json!({
        "result_key": ["完了", null, {"mixedCase": [false, 3]}]
    });
    let normalized = normalize_event(&json!({
        "event": {
            "ctx_event_id": "event-1",
            "text": "normalized response body",
            "mcp_exchange": {
                "provider_call_id": "call-呼び出し-🦀",
                "invocation": {
                    "server": "mcp-サーバー",
                    "tool": "検索-tool",
                    "arguments": {
                        "capture_status": "present",
                        "value": raw_arguments.clone()
                    }
                },
                "response": {
                    "status": "succeeded",
                    "duration_ns": 42,
                    "text": {"capture_status": "normalized_body"},
                    "payload": {
                        "capture_status": "present",
                        "value": raw_payload.clone()
                    }
                }
            }
        },
        "events": [
            {
                "mcp_exchange": {
                    "provider_call_id": "capture-states",
                    "invocation": {
                        "server": "server",
                        "tool": "tool",
                        "arguments": {"capture_status": "absent"}
                    },
                    "response": {
                        "status": "cancelled",
                        "text": {"capture_status": "unavailable"},
                        "payload": {
                            "capture_status": "omitted",
                            "reason": "size_limit",
                            "observed_encoded_bytes": 70000
                        }
                    }
                }
            },
            {"ctx_event_id": "event-without-exchange"}
        ]
    }))
    .unwrap();
    let selected = normalized.event.unwrap();
    let exchange = selected.mcp_exchange.as_ref().unwrap();
    assert_eq!(exchange.provider_call_id, "call-呼び出し-🦀");
    let McpJsonCapture::Present { value: arguments } =
        &exchange.invocation.as_ref().unwrap().arguments
    else {
        panic!("normalized arguments must be present");
    };
    assert_eq!(arguments, &raw_arguments);
    let response = exchange.response.as_ref().unwrap();
    assert_eq!(response.status, McpResponseStatus::Succeeded);
    assert_eq!(response.duration_ns, Some(42));
    let McpJsonCapture::Present { value: payload } = &response.payload else {
        panic!("normalized payload must be present");
    };
    assert_eq!(payload, &raw_payload);

    let encoded = serde_json::to_value(&selected).unwrap();
    assert!(encoded.get("mcp_exchange").is_none());
    assert_eq!(
        encoded["mcpExchange"]["invocation"]["arguments"]["value"],
        raw_arguments
    );
    assert_eq!(
        encoded["mcpExchange"]["response"]["payload"]["value"],
        raw_payload
    );

    let states = normalized.events[0].mcp_exchange.as_ref().unwrap();
    assert_eq!(
        states.invocation.as_ref().unwrap().arguments,
        McpJsonCapture::Absent
    );
    assert_eq!(
        states.response.as_ref().unwrap().text,
        McpTextCapture::Unavailable
    );
    assert_eq!(
        states.response.as_ref().unwrap().payload,
        McpJsonCapture::Omitted {
            reason: McpPayloadOmissionReason::SizeLimit,
            observed_encoded_bytes: Some(70_000),
        }
    );
    assert!(normalized.events[1].mcp_exchange.is_none());
}

#[test]
fn mcp_exchange_normalization_rejects_null_unknown_and_alias_ambiguity() {
    for fixture in [
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-explicit-null.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-unknown-field.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-outer-alias-collision.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-normalized-body-missing-event-text.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-normalized-body-empty-event-text.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-unsafe-duration-ns.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-exchange-unsafe-observed-encoded-bytes.json"
        )
        .as_slice(),
    ] {
        let raw = decode_json_value_exact(fixture, "failed to decode ctx JSON").unwrap();
        let error = normalize_event(&raw).unwrap_err();
        assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
        assert!(
            error.body.message.to_ascii_lowercase().contains("mcp")
                || error.body.cause.as_deref().is_some_and(|cause| {
                    cause.to_ascii_lowercase().contains("mcp")
                }),
            "missing MCP decode context: message={:?}, cause={:?}",
            error.body.message,
            error.body.cause
        );
    }

    for invalid in [
        json!({
            "event": {
                "mcp_exchange_": {
                    "provider_call_id": "forged",
                    "response": {
                        "status": "succeeded",
                        "text": {"capture_status": "absent"},
                        "payload": {"capture_status": "absent"}
                    }
                }
            },
            "events": []
        }),
        json!({
            "event": {
                "mcp_exchange": {
                    "provider_call_id": "snake",
                    "providerCallId": "camel",
                    "response": {
                        "status": "succeeded",
                        "text": {"capture_status": "absent"},
                        "payload": {"capture_status": "absent"}
                    }
                }
            },
            "events": []
        }),
        json!({
            "event": {
                "mcp_exchange": {
                    "provider_call_id": "call",
                    "response": {
                        "status": "succeeded",
                        "text": {"capture_status": "absent"},
                        "payload": {
                            "capture_status": "omitted",
                            "captureStatus": "omitted",
                            "reason": "size_limit"
                        }
                    }
                }
            },
            "events": []
        }),
    ] {
        let error = normalize_event(&invalid).unwrap_err();
        assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
    }
}

#[cfg(unix)]
#[test]
fn show_event_local_cli_drains_max_valid_mcp_attribution() {
    assert_eq!(MAX_MCP_TOOL_CALL_COMPONENT_BYTES, 65_536);

    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-fake");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
emit_component() {
  awk -v byte="$1" 'BEGIN { for (i = 0; i < 65536; i++) printf "%s", byte }'
}
printf '%s' '{"event":{"ctx_event_id":"event-1","ctx_session_id":"session-1","mcp_tool_call":{"server":"'
emit_component s
printf '%s' '","tool":"'
emit_component t
printf '%s\n' '"}},"events":[]}'
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: None,
        env: BTreeMap::new(),
        timeout: Duration::from_secs(5),
    });
    let selected = client
        .show_event("event-1", ShowEventOptions::default())
        .unwrap()
        .event
        .unwrap()
        .event
        .unwrap();
    let call = selected.mcp_tool_call.unwrap();
    assert_eq!(call.server.len(), MAX_MCP_TOOL_CALL_COMPONENT_BYTES);
    assert_eq!(call.tool.len(), MAX_MCP_TOOL_CALL_COMPONENT_BYTES);
    assert!(call.server.bytes().all(|byte| byte == b's'));
    assert!(call.tool.bytes().all(|byte| byte == b't'));
}

#[cfg(unix)]
#[test]
fn local_cli_drains_large_stderr_while_collecting_stdout() {
    let raw = run_json_shell(
        r#"awk 'BEGIN { for (i = 0; i < 262144; i++) printf "e" }' >&2
printf '%s\n' '{"initialized":true,"local_only":true}'"#,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(raw["initialized"], true);
}

#[cfg(unix)]
#[test]
fn local_cli_preserves_exit_utf8_and_exact_decode_errors() {
    let error = run_json_shell(
        "printf '%s\\n' 'not found: café 🦀' >&2\nexit 7",
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert_eq!(error.body.code, AgentHistoryErrorCode::NotFound);
    assert_eq!(error.body.message, "not found: café 🦀");
    assert!(!error.body.retryable);

    let error = run_json_shell(
        r#"printf '%s\n' '{"event":{"mcp_tool_call":{"server":"first","server":"second","tool":"one"}},"events":[]}'"#,
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
    assert!(error
        .body
        .cause
        .as_deref()
        .is_some_and(|cause| cause.contains("duplicate JSON object member")));
}

#[test]
fn raw_json_decode_rejects_duplicate_members_without_scanning_string_contents() {
    assert!(serde_json::from_str::<AgentHistoryEvent>(
        r#"{"mcpToolCall":{"server":"first","tool":"one"},"mcpToolCall":{"server":"second","tool":"two"}}"#
    )
    .is_err());
    assert!(serde_json::from_str::<AgentHistoryEvent>(
        r#"{"mcpToolCall":{"server":"first","server":"second","tool":"one"}}"#
    )
    .is_err());
    assert!(serde_json::from_str::<AgentHistoryEvent>(
        r#"{"mcpExchange":{"providerCallId":"first","response":{"status":"succeeded","text":{"captureStatus":"absent"},"payload":{"captureStatus":"absent"}}},"mcpExchange":{"providerCallId":"second","response":{"status":"succeeded","text":{"captureStatus":"absent"},"payload":{"captureStatus":"absent"}}}}"#
    )
    .is_err());
    assert!(serde_json::from_str::<AgentHistoryEvent>(
        r#"{"mcpExchange":{"providerCallId":"first","providerCallId":"second","response":{"status":"succeeded","text":{"captureStatus":"absent"},"payload":{"captureStatus":"absent"}}}}"#
    )
    .is_err());

    for duplicate in [
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/duplicate-event-mcp-tool-call-snake.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/duplicate-event-mcp-tool-call-camel.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/duplicate-mcp-tool-call-server.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/duplicate-mcp-tool-call-tool.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/duplicate-event-mcp-exchange-snake.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/duplicate-mcp-exchange-captured-value.json"
        )
        .as_slice(),
    ] {
        let error = decode_json_value_exact(duplicate, "failed to decode ctx JSON").unwrap_err();
        assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
        assert!(error
            .body
            .cause
            .as_deref()
            .is_some_and(|cause| cause.contains("duplicate JSON object member")));
    }

    for transformed in [
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-transformed-server.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-transformed-tool.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-transformed-collision.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-outer-alias-collision.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-outer-mixed-case.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-outer-repeated-separator.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-outer-trailing-separator.json"
        )
        .as_slice(),
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/invalid-mcp-tool-call-outer-camel-snake.json"
        )
        .as_slice(),
    ] {
        let raw = decode_json_value_exact(transformed, "failed to decode ctx JSON").unwrap();
        let error = normalize_event(&raw).unwrap_err();
        assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
    }

    let repeated = decode_json_value_exact(
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/valid-repeated-string-contents.json"
        ),
        "failed to decode ctx JSON",
    )
    .unwrap();
    let normalized = normalize_event(&repeated).unwrap();
    let call = normalized.event.unwrap().mcp_tool_call.unwrap();
    assert_eq!(call.server, "server server");
    assert_eq!(call.tool, "tool tool");

    let aliases = decode_json_value_exact(
        include_bytes!(
            "../../../contracts/agent-history-v1/fixtures/adversarial/valid-mcp-tool-call-outer-aliases.json"
        ),
        "failed to decode ctx JSON",
    )
    .unwrap();
    let normalized = normalize_event(&aliases).unwrap();
    let primary = normalized.event.unwrap();
    assert_eq!(primary.mcp_tool_call.unwrap().server, "snake-server");
    assert_eq!(
        primary.extra.get("futureEventField"),
        Some(&json!("snake-extra"))
    );
    assert_eq!(
        normalized.events[0].mcp_tool_call.as_ref().unwrap().server,
        "camel-server"
    );
    assert_eq!(
        normalized.events[0].extra.get("futureEventField"),
        Some(&json!("camel-extra"))
    );
}

#[test]
fn show_session_defaults_to_unbounded_cli_streaming() {
    let defaults = ShowSessionOptions::default();
    assert_eq!(defaults.mode, "lite");
    assert_eq!(defaults.limit, None);
    assert_eq!(defaults.cursor, None);

    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-fake");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$CTX_DATA_ROOT/argv.txt"
printf '%s\n' '{"session":{"ctx_session_id":"session-1"},"events":[],"mode":"lite","format":"json"}'
"#,
    )
    .unwrap();
    #[cfg(unix)]
    make_test_executable(&script);

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(temp.path().to_path_buf()),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(5),
    });
    client
        .show_session("session-1", ShowSessionOptions::default())
        .unwrap();

    let argv = fs::read_to_string(temp.path().join("argv.txt")).unwrap();
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        vec![
            "show",
            "session",
            "session-1",
            "--mode",
            "lite",
            "--format",
            "json"
        ]
    );
}

#[cfg(unix)]
#[test]
fn local_json_collector_drains_complete_stdout_beyond_pipe_capacity() {
    const PADDING_BYTES: usize = 2 * 1024 * 1024;
    const CHUNK_BYTES: usize = 4 * 1024;

    let chunk = "x".repeat(CHUNK_BYTES);
    let body = format!(
        r#"printf '%s' '{{"session":{{"ctx_session_id":"session-1"}},"events":[{{"ctx_event_id":"event-1","ctx_session_id":"session-1","text":"'
chunk='{chunk}'
i=0
while [ "$i" -lt {} ]; do
  printf '%s' "$chunk"
  i=$((i + 1))
done
printf '%s\n' '"}}],"mode":"lite","format":"json"}}'"#,
        PADDING_BYTES / CHUNK_BYTES
    );
    let raw = collect_ctx_json(spawn_json_shell(&body), Duration::from_secs(5)).unwrap();
    assert_eq!(
        raw["events"][0]["text"].as_str().unwrap().len(),
        PADDING_BYTES
    );
    let session = normalize(
        AgentHistoryOperation::ShowSession,
        BackendInfo::local(None),
        raw,
    )
    .unwrap()
    .session
    .unwrap();
    assert_eq!(session.events.len(), 1);
    assert_eq!(
        session.events[0].text.as_ref().unwrap().len(),
        PADDING_BYTES
    );
}

#[cfg(unix)]
#[test]
fn local_json_collector_drains_large_stderr_with_bounded_retention() {
    const CHUNK_BYTES: usize = 4 * 1024;

    let chunk = "diagnostic".repeat(CHUNK_BYTES / "diagnostic".len());
    let body = format!(
        r#"chunk='{chunk}'
i=0
while [ "$i" -lt {} ]; do
  printf '%s' "$chunk" >&2
  i=$((i + 1))
done
printf '%s\n' '{{"initialized":true,"local_only":true}}'"#,
        (MAX_RETAINED_SUBPROCESS_STDERR_BYTES * 4) / chunk.len()
    );
    let raw = collect_ctx_json(spawn_json_shell(&body), Duration::from_secs(5)).unwrap();
    assert_eq!(raw["initialized"], true);
    let retained = read_bounded_pipe(
        std::io::Cursor::new(vec![b'x'; MAX_RETAINED_SUBPROCESS_STDERR_BYTES * 2]),
        MAX_RETAINED_SUBPROCESS_STDERR_BYTES,
    )
    .unwrap();
    assert_eq!(retained.len(), MAX_RETAINED_SUBPROCESS_STDERR_BYTES);
}

#[cfg(unix)]
#[test]
fn mcp_collector_rejects_oversized_stdout_without_partial_json() {
    const CHUNK_BYTES: usize = 4 * 1024;

    let chunk = "x".repeat(CHUNK_BYTES);
    let body = format!(
        r#"cat >/dev/null
printf '%s' '{{"jsonrpc":"2.0","id":2,"result":{{"padding":"'
chunk='{chunk}'
i=0
while [ "$i" -lt {} ]; do
  printf '%s' "$chunk"
  i=$((i + 1))
done
printf '%s\n' '"}}}}'"#,
        MAX_RETAINED_MCP_STDOUT_BYTES / CHUNK_BYTES + 1
    );
    let error = collect_ctx_mcp_output(spawn_mcp_shell(&body), Vec::new(), Duration::from_secs(10))
        .unwrap_err();

    assert_eq!(error.body.code, AgentHistoryErrorCode::AdapterError);
    assert_eq!(
        error.body.message,
        format!(
            "ctx MCP stdout exceeded the {}-byte response limit",
            MAX_RETAINED_MCP_STDOUT_BYTES
        )
    );
    assert!(error.body.cause.is_none());
}

#[cfg(unix)]
#[test]
fn mcp_collector_drains_oversized_stderr_with_bounded_retention() {
    const CHUNK_BYTES: usize = 4 * 1024;

    let chunk = "e".repeat(CHUNK_BYTES);
    let body = format!(
        r#"cat >/dev/null
chunk='{chunk}'
i=0
while [ "$i" -lt {} ]; do
  printf '%s' "$chunk" >&2
  i=$((i + 1))
done
exit 7"#,
        (MAX_RETAINED_SUBPROCESS_STDERR_BYTES * 4) / CHUNK_BYTES
    );
    let output =
        collect_ctx_mcp_output(spawn_mcp_shell(&body), Vec::new(), Duration::from_secs(5)).unwrap();

    assert!(!output.status.success());
    assert_eq!(output.stderr.len(), MAX_RETAINED_SUBPROCESS_STDERR_BYTES);
    assert!(output.stderr.iter().all(|byte| *byte == b'e'));
}

#[cfg(unix)]
#[test]
fn mcp_nonzero_exit_prefers_typed_stderr_to_invalid_stdout() {
    const CHUNK_BYTES: usize = 4 * 1024;

    let chunk = "x".repeat(CHUNK_BYTES);
    let oversized = format!(
        r#"cat >/dev/null
printf '%s' '{{"jsonrpc":"2.0","id":2,"result":{{"padding":"'
chunk='{chunk}'
i=0
while [ "$i" -lt {} ]; do
  printf '%s' "$chunk"
  i=$((i + 1))
done
printf '%s\n' '"}}}}'
printf '%s\n' 'not found: fixture session' >&2
exit 7"#,
        MAX_RETAINED_MCP_STDOUT_BYTES / CHUNK_BYTES + 1
    );
    let cases = [
        (
            "malformed",
            "cat >/dev/null\nprintf '%s\\n' '{\"jsonrpc\":'\nprintf '%s\\n' 'not found: fixture session' >&2\nexit 7"
                .to_owned(),
        ),
        ("oversized", oversized),
    ];

    for (case, body) in cases {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join(format!("ctx-mcp-{case}"));
        fs::write(&script, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        make_test_executable(&script);
        let client = AgentHistoryClient::local(LocalBackendConfig {
            ctx_binary: script,
            data_root: Some(temp.path().to_path_buf()),
            env: BTreeMap::new(),
            timeout: Duration::from_secs(10),
        });

        let error = client
            .show_session(
                "session-1",
                ShowSessionOptions {
                    mode: "log".to_owned(),
                    limit: Some(1),
                    cursor: None,
                },
            )
            .unwrap_err();

        assert_eq!(error.body.code, AgentHistoryErrorCode::NotFound, "{case}");
        assert_eq!(error.body.message, "not found: fixture session", "{case}");
        assert!(!error.body.retryable, "{case}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn local_json_timeout_kills_and_reaps_the_child() {
    let child = spawn_json_shell("while :; do :; done");
    let pid = child.id().to_string();
    let error = collect_ctx_json(child, Duration::from_millis(100)).unwrap_err();
    assert_eq!(error.body.code, AgentHistoryErrorCode::Timeout);
    assert!(
        !Path::new("/proc").join(&pid).exists(),
        "timed-out ctx child {pid} was not reaped"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn local_json_ordinary_completion_reaps_descendants_on_success_and_failure() {
    let cases = [
        (
            "success",
            "printf '%s\\n' '{\"initialized\":true,\"local_only\":true}'",
            true,
        ),
        (
            "nonzero",
            "printf '%s\\n' 'not found: fixture session' >&2\nexit 7",
            false,
        ),
    ];

    for (case, completion, succeeds) in cases {
        let temp = tempfile::tempdir().unwrap();
        let body = format!(
            r#"set -eu
sleep 30 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!" > "$CTX_DATA_ROOT/descendant.pid"
{completion}"#
        );
        let child = spawn_tree_shell(&body, temp.path(), false);
        let wrapper_pid = child.id();
        let mut cleanup = ProcessGroupCleanup(Some(wrapper_pid));

        let result = collect_ctx_json(child, Duration::from_secs(5));
        assert_eq!(result.is_ok(), succeeds, "{case}: {result:?}");
        assert_ordinary_process_tree_reaped(temp.path(), wrapper_pid);
        cleanup.0 = None;
    }
}

#[cfg(target_os = "linux")]
#[test]
fn mcp_ordinary_completion_reaps_descendants_on_success_and_failure() {
    let cases = [
        (
            "success",
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}'",
            true,
        ),
        (
            "nonzero",
            "printf '%s\\n' 'not found: fixture session' >&2\nexit 7",
            false,
        ),
    ];

    for (case, completion, succeeds) in cases {
        let temp = tempfile::tempdir().unwrap();
        let body = format!(
            r#"set -eu
sleep 30 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!" > "$CTX_DATA_ROOT/descendant.pid"
cat >/dev/null
{completion}"#
        );
        let child = spawn_tree_shell(&body, temp.path(), true);
        let wrapper_pid = child.id();
        let mut cleanup = ProcessGroupCleanup(Some(wrapper_pid));

        let output =
            collect_ctx_mcp_output(child, b"fixture request\n".to_vec(), Duration::from_secs(5))
                .unwrap();
        assert_eq!(output.status.success(), succeeds, "{case}");
        assert_ordinary_process_tree_reaped(temp.path(), wrapper_pid);
        cleanup.0 = None;
    }
}

#[cfg(target_os = "linux")]
fn spawn_tree_shell(body: &str, data_root: &Path, mcp: bool) -> Child {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", body])
        .env("CTX_DATA_ROOT", data_root)
        .stdin(if mcp { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_configured(&mut command).unwrap()
}

#[cfg(target_os = "linux")]
fn assert_ordinary_process_tree_reaped(data_root: &Path, wrapper_pid: u32) {
    let descendant_pid = wait_for_test_pid(&data_root.join("descendant.pid"));
    wait_for_process_termination(wrapper_pid);
    wait_for_process_termination(descendant_pid);
}

#[cfg(target_os = "linux")]
#[test]
fn local_json_wrapper_with_inherited_stream_descendant_is_bounded_and_cleaned() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-inherited-streams");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$$" > "$CTX_DATA_ROOT/wrapper.pid"
sleep 30 &
printf '%s\n' "$!" > "$CTX_DATA_ROOT/descendant.pid"
printf '%s\n' '{"initialized":true,"local_only":true}'
"#,
    )
    .unwrap();
    make_test_executable(&script);

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(temp.path().to_path_buf()),
        env: BTreeMap::new(),
        timeout: Duration::from_millis(200),
    });
    assert_inherited_stream_timeout(temp.path(), move || client.status());
}

#[cfg(target_os = "linux")]
#[test]
fn mcp_wrapper_with_inherited_stream_descendant_is_bounded_and_cleaned() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-mcp-inherited-streams");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$$" > "$CTX_DATA_ROOT/wrapper.pid"
sleep 30 &
printf '%s\n' "$!" > "$CTX_DATA_ROOT/descendant.pid"
cat >/dev/null
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"ctx"}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[],"structuredContent":{"schema_version":1,"target":"session","payload_type":"session_transcript","ctx_session_id":"session-1","mode":"log","format":"json","session":{"ctx_session_id":"session-1"},"events":[]}}}'
"#,
    )
    .unwrap();
    make_test_executable(&script);

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(temp.path().to_path_buf()),
        env: BTreeMap::new(),
        timeout: Duration::from_millis(200),
    });
    assert_inherited_stream_timeout(temp.path(), move || {
        client.show_session(
            "session-1",
            ShowSessionOptions {
                mode: "log".to_owned(),
                limit: Some(1),
                cursor: None,
            },
        )
    });
}

#[cfg(target_os = "linux")]
fn assert_inherited_stream_timeout(
    data_root: &Path,
    operation: impl FnOnce() -> Result<AgentHistoryEnvelope, AgentHistoryError> + Send + 'static,
) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let started = Instant::now();
        let result = operation();
        sender.send((started.elapsed(), result)).unwrap();
    });

    let wrapper_pid = wait_for_test_pid(&data_root.join("wrapper.pid"));
    let mut cleanup = ProcessGroupCleanup(Some(wrapper_pid));
    let descendant_pid = wait_for_test_pid(&data_root.join("descendant.pid"));
    let (elapsed, result) = match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(err) => {
            drop(cleanup);
            let _ = receiver.recv_timeout(Duration::from_secs(2));
            drop(worker);
            panic!("SDK collection exceeded its bounded deadline: {err}");
        }
    };
    worker.join().unwrap();

    let error = result.unwrap_err();
    assert_eq!(error.body.code, AgentHistoryErrorCode::Timeout);
    assert!(
        elapsed < Duration::from_secs(2),
        "inherited stream handles delayed return for {elapsed:?}"
    );
    wait_for_process_termination(wrapper_pid);
    wait_for_process_termination(descendant_pid);
    cleanup.0 = None;
}

#[cfg(target_os = "linux")]
struct ProcessGroupCleanup(Option<u32>);

#[cfg(target_os = "linux")]
impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        let Some(process_group) = self.0 else {
            return;
        };
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{process_group}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(target_os = "linux")]
fn wait_for_test_pid(path: &Path) -> u32 {
    let started = Instant::now();
    loop {
        if let Ok(raw) = fs::read_to_string(path) {
            return raw.trim().parse().unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_process_termination(pid: u32) {
    let process_status = Path::new("/proc").join(pid.to_string()).join("stat");
    let started = Instant::now();
    while fs::read_to_string(&process_status)
        .ok()
        .and_then(|status| {
            status
                .rsplit_once(") ")
                .and_then(|(_, fields)| fields.chars().next())
        })
        .is_some_and(|state| !matches!(state, 'Z' | 'X'))
    {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "task-owned process {pid} remained live after collection cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn local_json_nonzero_exit_preserves_typed_error() {
    let child = spawn_json_shell("printf '%s\\n' 'not found: fixture session' >&2\nexit 7");
    let error = collect_ctx_json(child, Duration::from_secs(5)).unwrap_err();
    assert_eq!(error.body.code, AgentHistoryErrorCode::NotFound);
    assert_eq!(error.body.message, "not found: fixture session");
    assert!(!error.body.retryable);
}

#[cfg(unix)]
#[test]
fn local_json_malformed_stdout_preserves_decode_error() {
    let child = spawn_json_shell("printf '%s\\n' '{\"initialized\":'");
    let error = collect_ctx_json(child, Duration::from_secs(5)).unwrap_err();
    assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
    assert_eq!(error.body.message, "failed to decode ctx JSON");
    assert!(error.body.cause.is_some());
    assert!(!error.body.retryable);
}

#[test]
fn show_session_limit_and_cursor_use_existing_mcp_paging_contract() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-fake");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$CTX_DATA_ROOT/argv.txt"
cat > "$CTX_DATA_ROOT/stdin.jsonl"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"ctx"}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[],"structuredContent":{"schema_version":1,"target":"session","payload_type":"session_transcript","ctx_session_id":"session-1","mode":"log","format":"json","session":{"ctx_session_id":"session-1"},"events":[],"pagination":{"limit":7,"returned":0,"has_more":true,"next_cursor":"page-2"}}}}'
"#,
    )
    .unwrap();
    #[cfg(unix)]
    make_test_executable(&script);

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(temp.path().to_path_buf()),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(5),
    });
    let envelope = client
        .show_session(
            "session-1",
            ShowSessionOptions {
                mode: "log".to_owned(),
                limit: Some(7),
                cursor: Some("page-1".to_owned()),
            },
        )
        .unwrap();

    let argv = fs::read_to_string(temp.path().join("argv.txt")).unwrap();
    assert_eq!(argv.lines().collect::<Vec<_>>(), vec!["mcp", "serve"]);
    let requests = fs::read_to_string(temp.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["method"], "initialize");
    assert_eq!(requests[1]["method"], "tools/call");
    assert_eq!(requests[1]["params"]["name"], "show_session");
    assert_eq!(
        requests[1]["params"]["arguments"],
        json!({
            "ctx_session_id": "session-1",
            "mode": "log",
            "limit": 7,
            "cursor": "page-1"
        })
    );

    let session = envelope.session.unwrap();
    assert_eq!(
        session.extra["pagination"],
        json!({
            "limit": 7,
            "returned": 0,
            "hasMore": true,
            "nextCursor": "page-2"
        })
    );
}

#[test]
fn sources_and_import_preserve_legitimate_nested_source_semantics() {
    let sources = normalize(
        AgentHistoryOperation::Sources,
        BackendInfo::local(None),
        json!({
            "sources": [{
                "provider": "trae",
                "path": "/configured/root",
                "status": "unknown",
                "status_reason": "blocked_auth_or_encryption",
                "importable": false,
                "acquisition": {
                    "source": "local_scan",
                    "cursor": "opaque-checkpoint"
                }
            }]
        }),
    )
    .unwrap();
    let source = &sources.sources.as_ref().unwrap()[0];
    assert_eq!(
        source.status_reason.as_deref(),
        Some("blocked_auth_or_encryption")
    );
    assert_eq!(source.extra["acquisition"]["source"], "local_scan");
    assert_eq!(source.extra["acquisition"]["cursor"], "opaque-checkpoint");

    let imported = normalize_import(&json!({
        "resume": false,
        "totals": {},
        "sources": [{
            "source": {
                "provider": "codex",
                "cursor": "provider-checkpoint"
            }
        }]
    }))
    .unwrap();
    assert_eq!(imported.sources[0]["source"]["provider"], "codex");
    assert_eq!(
        imported.sources[0]["source"]["cursor"],
        "provider-checkpoint"
    );
}

#[test]
fn search_normalization_bridges_result_window_to_published_pagination() {
    let search = normalize_search(&json!({
        "query": "bounded",
        "results": [{"result_scope": "event"}],
        "result_window": {
            "limit": 1,
            "returned": 1,
            "more_available": true
        }
    }))
    .unwrap();

    assert_eq!(search.result_window.as_ref().unwrap().limit, 1);
    assert_eq!(search.result_window.as_ref().unwrap().returned, 1);
    assert!(search.result_window.as_ref().unwrap().more_available);
    assert_eq!(search.pagination.as_ref().unwrap()["limit"], 1);
    assert_eq!(search.pagination.as_ref().unwrap()["hasMore"], true);
    assert!(search
        .pagination
        .as_ref()
        .unwrap()
        .get("nextCursor")
        .is_none());
}

#[test]
fn init_normalizes_real_setup_json_into_status_contract() {
    let envelope = normalize(
        AgentHistoryOperation::Init,
        BackendInfo::local(Some("/tmp/ctx".to_owned())),
        json!({
            "schema_version": 2,
            "initialized": true,
            "data_root": "/tmp/ctx",
            "config_path": "/tmp/ctx/config.toml",
            "mode": "ready",
            "indexed_items": 2_147_483_648_u64,
            "indexed_sessions": 2_147_483_649_u64,
            "indexed_events": 2_147_483_650_u64,
            "indexed_sources": 2_147_483_651_u64,
            "network_required": false,
            "lexical": {
                "status": "ready",
                "generation_id": "generation-1"
            },
            "refresh": {"status": "ready"},
            "import": {"resume": false, "totals": {}}
        }),
    )
    .unwrap();

    assert_eq!(envelope.operation, AgentHistoryOperation::Init);
    let status = envelope.status.unwrap();
    assert!(status.initialized);
    assert!(status.local_only);
    assert_eq!(status.data_root.as_deref(), Some("/tmp/ctx"));
    assert_eq!(status.indexed_items, Some(2_147_483_648));
    assert_eq!(status.indexed_sessions, Some(2_147_483_649));
    assert_eq!(status.indexed_events, Some(2_147_483_650));
    assert_eq!(status.indexed_sources, Some(2_147_483_651));
    assert_eq!(status.lexical.as_ref().unwrap()["status"], "ready");
    assert_eq!(status.refresh.as_ref().unwrap()["status"], "ready");
    assert!(status.extra.is_empty());
}

#[test]
fn status_normalization_rejects_counters_outside_the_exact_json_domain() {
    for rejected in [9_007_199_254_740_993_u64, u64::MAX] {
        let error = normalize(
            AgentHistoryOperation::Status,
            BackendInfo::local(Some("/tmp/ctx".to_owned())),
            json!({
                "initialized": true,
                "indexed_items": rejected
            }),
        )
        .unwrap_err();

        assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
        assert!(error
            .body
            .cause
            .as_deref()
            .is_some_and(|cause| cause.contains("status counter exceeds maximum")));
    }
}

#[test]
fn hosted_backend_returns_structured_error() {
    let client = AgentHistoryClient::hosted(HostedBackendConfig {
        base_url: "https://ctx.example.invalid".to_owned(),
        timeout: Duration::from_secs(1),
    });
    let err = client.status().unwrap_err();
    assert_eq!(err.body.code, AgentHistoryErrorCode::NotSupported);
    assert!(!err.body.retryable);
}

#[test]
fn local_backend_forces_analytics_off_after_ambient_and_user_env() {
    const HELPER_ENV: &str = "CTX_SDK_ANALYTICS_PRIVACY_TEST";

    if std::env::var_os(HELPER_ENV).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("local_backend_forces_analytics_off_after_ambient_and_user_env")
            .arg("--nocapture")
            .env(HELPER_ENV, "1")
            .env("CTX_ANALYTICS_ENABLED", "true")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "strict-local helper failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert_eq!(
        std::env::var("CTX_ANALYTICS_ENABLED").as_deref(),
        Ok("true")
    );
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-fake");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
if [ "${CTX_ANALYTICS_ENABLED:-}" != "false" ]; then
  echo "network analytics were enabled" >&2
  exit 99
fi
printf '%s\n' '{"initialized":true,"local_only":true}'
"#,
    )
    .unwrap();
    #[cfg(unix)]
    make_test_executable(&script);

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: None,
        env: BTreeMap::from([("CTX_ANALYTICS_ENABLED".to_owned(), "true".to_owned())]),
        timeout: Duration::from_secs(5),
    });

    let status = client.status().unwrap().status.unwrap();
    assert!(status.local_only);
}

#[test]
fn builds_search_cli_arguments_without_running_for_public_options() {
    let options = SearchOptions {
        query: Some("agent history".to_owned()),
        terms: vec!["ctx".to_owned()],
        limit: 3,
        backend: Some("hybrid".to_owned()),
        semantic_weight: Some(0.35),
        provider: Some("codex".to_owned()),
        refresh: SearchRefresh::Off,
        events: true,
        ..SearchOptions::default()
    };
    assert_eq!(options.refresh.as_arg(), "off");
    assert_eq!(options.terms, vec!["ctx"]);
    assert_eq!(options.backend.as_deref(), Some("hybrid"));
    assert_eq!(options.semantic_weight, Some(0.35));
    assert!(SearchOptions::default().backend.is_none());
    assert!(SearchOptions::default().semantic_weight.is_none());
    assert!(SearchOptions::default().content_scope.is_none());
    assert!(SearchOptions::default().event_type.is_none());
    assert_eq!(SearchContentScope::All.as_arg(), "all");
    assert_eq!(SearchContentScope::Transcript.as_arg(), "transcript");
    assert_eq!(SearchContentScope::Calls.as_arg(), "calls");
    assert_eq!(SearchContentScope::Outputs.as_arg(), "outputs");
}

#[test]
fn search_options_map_retrieval_and_class_filters_to_cli_flags() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-fake");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$CTX_DATA_ROOT/argv.txt"
if [ "$1" = "search" ]; then
  printf '%s\n' '{"query":"agent history","results":[]}'
  exit 0
fi
echo "unexpected command: $*" >&2
exit 2
"#,
    )
    .unwrap();
    #[cfg(unix)]
    make_test_executable(&script);

    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(temp.path().to_path_buf()),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(5),
    });

    client
        .search(SearchOptions {
            query: Some("agent history".to_owned()),
            limit: 7,
            backend: Some("hybrid".to_owned()),
            semantic_weight: Some(0.625),
            content_scope: Some(SearchContentScope::Calls),
            refresh: SearchRefresh::Off,
            ..SearchOptions::default()
        })
        .unwrap();

    let argv = fs::read_to_string(temp.path().join("argv.txt")).unwrap();
    let argv = argv.lines().collect::<Vec<_>>();
    assert_eq!(
        argv,
        vec![
            "search",
            "agent history",
            "--limit",
            "7",
            "--backend",
            "hybrid",
            "--semantic-weight",
            "0.625",
            "--content-scope",
            "calls",
            "--refresh",
            "off",
            "--format=json",
        ]
    );

    client
        .search(SearchOptions {
            query: Some("agent history".to_owned()),
            limit: 4,
            event_type: Some("tool_call".to_owned()),
            refresh: SearchRefresh::Wait,
            ..SearchOptions::default()
        })
        .unwrap();

    let argv = fs::read_to_string(temp.path().join("argv.txt")).unwrap();
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        vec![
            "search",
            "agent history",
            "--limit",
            "4",
            "--event-type",
            "tool_call",
            "--refresh",
            "wait",
            "--format=json",
        ]
    );
}

#[test]
fn search_normalization_camelizes_retrieval_json() {
    let envelope = normalize(
        AgentHistoryOperation::Search,
        BackendInfo::local(None),
        json!({
            "query": "semantic defaults",
            "generated_at": "2026-07-05T00:00:00Z",
            "retrieval": {
                "requested_mode": "hybrid",
                "effective_mode": "lexical",
                "semantic_weight": 0.0,
                "semantic_fallback_code": "semantic_retrieval_failed",
                "semantic_fallback": "semantic_retrieval_failed",
                "coverage": {"embedded_items": 4, "indexed_now": 1},
                "diagnostics": {"query_embed_ms": 2}
            },
            "results": [{
                "ctx_event_id": "event-1",
                "ctx_session_id": "session-1",
                "result_scope": "event",
                "snippet": "semantic match",
            }],
        }),
    )
    .unwrap();

    let search = envelope.search.unwrap();
    let retrieval = search.retrieval.unwrap();
    assert_eq!(retrieval.requested_mode.as_deref(), Some("hybrid"));
    assert_eq!(retrieval.effective_mode.as_deref(), Some("lexical"));
    assert_eq!(retrieval.semantic_weight, Some(0.0));
    assert_eq!(
        retrieval.semantic_fallback_code.as_deref(),
        Some("semantic_retrieval_failed")
    );
    assert_eq!(
        retrieval.semantic_fallback.as_deref(),
        Some("semantic_retrieval_failed")
    );
    assert_eq!(retrieval.coverage.as_ref().unwrap().embedded_items, Some(4));
    assert_eq!(
        retrieval.diagnostics.as_ref().unwrap().get("queryEmbedMs"),
        Some(&json!(2))
    );
    assert!(
        !search.extra.contains_key("retrieval"),
        "top-level retrieval should be typed, not left in extra"
    );
    assert_eq!(
        search.results[0].extra.get("retrieval"),
        None,
        "per-hit retrieval is not part of the canonical SDK search hit shape"
    );
}

#[test]
fn search_requires_query_term_or_file_before_cli() {
    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: PathBuf::from("/definitely/missing/ctx"),
        data_root: None,
        env: BTreeMap::new(),
        timeout: Duration::from_secs(1),
    });

    for options in [
        SearchOptions::default(),
        SearchOptions {
            refresh: SearchRefresh::Off,
            ..SearchOptions::default()
        },
        SearchOptions {
            query: Some("   ".to_owned()),
            terms: vec!["".to_owned(), "   ".to_owned()],
            ..SearchOptions::default()
        },
    ] {
        let err = client.search(options).unwrap_err();
        assert_eq!(err.body.code, AgentHistoryErrorCode::InvalidRequest);
    }
}

#[test]
fn search_rejects_content_scope_and_event_type_before_cli() {
    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: PathBuf::from("/definitely/missing/ctx"),
        data_root: None,
        env: BTreeMap::new(),
        timeout: Duration::from_secs(1),
    });

    let err = client
        .search(SearchOptions {
            query: Some("agent history".to_owned()),
            content_scope: Some(SearchContentScope::All),
            event_type: Some("message".to_owned()),
            ..SearchOptions::default()
        })
        .unwrap_err();

    assert_eq!(err.body.code, AgentHistoryErrorCode::InvalidRequest);
    assert!(err.body.message.contains("content_scope"));
    assert!(err.body.message.contains("event_type"));
}

#[test]
fn local_client_can_dogfood_fake_ctx_without_private_history() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ctx-fake");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
if [ "$1" = "status" ]; then
  printf '%s\n' '{"initialized":true,"local_only":true,"data_root":"'"$CTX_DATA_ROOT"'","indexed_items":2}'
  exit 0
fi
if [ "$1" = "search" ]; then
  printf '%s\n' '{"query":"rust sdk","generated_at":"2026-07-01T12:00:00Z","results":[{"ctx_event_id":"event-1","ctx_session_id":"session-1","result_scope":"event","snippet":"typed ergonomics"}]}'
  exit 0
fi
echo "unexpected command: $*" >&2
exit 2
"#,
    )
    .unwrap();
    #[cfg(unix)]
    make_test_executable(&script);

    let data_root = temp.path().join("data-root");
    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(data_root.clone()),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(5),
    });

    let status = client.status().unwrap();
    let status_body = status.status.unwrap();
    assert!(status_body.initialized);
    assert!(status_body.local_only);
    assert_eq!(
        status_body.data_root.as_deref(),
        Some(data_root.to_string_lossy().as_ref())
    );
    assert_eq!(status_body.indexed_items, Some(2));

    let search = client
        .search(SearchOptions {
            query: Some("rust sdk".to_owned()),
            refresh: SearchRefresh::Off,
            limit: 1,
            ..SearchOptions::default()
        })
        .unwrap();
    let search_body = search.search.unwrap();
    assert_eq!(search_body.results.len(), 1);
    assert_eq!(search_body.results[0].result_scope, "event");
    assert_eq!(
        search_body.results[0].snippet.as_deref(),
        Some("typed ergonomics")
    );
}

use super::*;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::process::Child;
#[cfg(unix)]
use std::thread;

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
fn generic_status_omits_retired_flags_without_changing_nested_semantics() {
    for operation in [AgentHistoryOperation::Status, AgentHistoryOperation::Init] {
        for flags in [
            json!({}),
            json!({"local_only": true}),
            json!({"localOnly": false}),
            json!({"local_only": null, "localOnly": "legacy"}),
        ] {
            let mut raw = flags;
            raw["schema_version"] = json!(2);
            raw["lexical"] = json!({"generation_id": "ready"});
            raw["semantic"] = json!({"local_only": false, "diagnostics": {"localOnly": null}});
            let output =
                serde_json::to_value(normalize(operation.clone(), BackendInfo::local(None), raw).unwrap())
                    .unwrap();
            let status = &output["status"];
            assert!(status.get("localOnly").is_none(), "{status}");
            assert!(status.get("local_only").is_none(), "{status}");
            assert_eq!(status["initialized"], true);
            assert_eq!(status["semantic"]["localOnly"], false);
            assert_eq!(
                status["semantic"]["diagnostics"].get("localOnly"),
                Some(&Value::Null)
            );
        }
    }
    let fallback = serde_json::to_value(normalize_status(&json!({})).unwrap()).unwrap();
    assert_eq!(fallback, json!({"initialized": false}));
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

mod additional;

#[cfg(unix)]
mod fidelity;

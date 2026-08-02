use super::*;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

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
                "provider": "codex",
                "path": "/configured/root",
                "status": "available",
                "importable": true,
                "acquisition": {
                    "source": "local_scan",
                    "cursor": "opaque-checkpoint"
                }
            }]
        }),
    )
    .unwrap();
    let source = &sources.sources.as_ref().unwrap()[0];
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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

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
}

#[test]
fn search_options_map_retrieval_controls_to_cli_flags() {
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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

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
            "--refresh",
            "off",
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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

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

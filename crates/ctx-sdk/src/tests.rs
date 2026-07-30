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
            "schema_version": 1,
            "data_root": "/tmp/ctx",
            "database_path": "/tmp/ctx/history.sqlite3",
            "config_path": "/tmp/ctx/config.toml",
            "mode": "ready",
            "indexed_items": 12,
            "network_required": false,
            "catalog": {"cataloged_sessions": 4},
            "import": {"resume": false, "totals": {}}
        }),
    )
    .unwrap();

    assert_eq!(envelope.operation, AgentHistoryOperation::Init);
    let status = envelope.status.unwrap();
    assert!(status.initialized);
    assert!(status.local_only);
    assert_eq!(status.data_root.as_deref(), Some("/tmp/ctx"));
    assert_eq!(status.indexed_items, Some(12));
    assert!(status.extra.contains_key("mode"));
    assert!(status.extra.contains_key("networkRequired"));
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

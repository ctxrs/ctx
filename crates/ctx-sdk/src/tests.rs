use std::{fs, path::PathBuf, time::Duration};

use ctx_protocol::{
    AgentHistoryEnvelope, AgentHistoryErrorCode, AgentHistoryOperation, BackendInfo,
    CONTRACT_VERSION,
};
use serde_json::json;

use super::*;
use crate::normalize::normalize;

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
        search.results[0].ctx_event_id.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
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
fn builds_search_cli_arguments_without_running_for_public_options() {
    let options = SearchOptions {
        query: Some("agent history".to_owned()),
        terms: vec!["ctx".to_owned()],
        limit: 3,
        provider: Some("codex".to_owned()),
        refresh: SearchRefresh::Off,
        events: true,
        ..SearchOptions::default()
    };
    assert_eq!(options.refresh.as_arg(), "off");
    assert_eq!(options.terms, vec!["ctx"]);
}

#[test]
fn search_requires_query_term_or_file_before_cli() {
    let client = AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: PathBuf::from("/definitely/missing/ctx"),
        data_root: None,
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

use super::*;

fn event_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../contracts/agent-history-v1/fixtures/cli/opaque-event.json"
    ))
    .unwrap()
}

fn fixture_client(root: &Path, raw: &Value, mcp: bool, is_error: bool) -> AgentHistoryClient {
    let output = if mcp {
        json!({"jsonrpc":"2.0","id":2,"result":{"isError":is_error,"structuredContent":raw}})
    } else {
        raw.clone()
    };
    fs::write(root.join("response.json"), output.to_string()).unwrap();
    let script = root.join("ctx-fixture");
    let body = if mcp {
        "cat >/dev/null\ncat \"$CTX_DATA_ROOT/response.json\""
    } else if is_error {
        "cat \"$CTX_DATA_ROOT/response.json\" >&2\nexit 1"
    } else {
        "printf '%s\\n' \"$@\" > \"$CTX_DATA_ROOT/argv\"\ncat \"$CTX_DATA_ROOT/response.json\""
    };
    fs::write(&script, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    make_test_executable(&script);
    AgentHistoryClient::local(LocalBackendConfig {
        ctx_binary: script,
        data_root: Some(root.to_owned()),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(10),
    })
}

#[test]
fn current_payloads_survive_complete_and_paged_public_operations() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = event_fixture();
    for content in [
        Some(fixture["structured_content"].clone()),
        Some(Value::Null),
        None,
    ] {
        let mut event = fixture.clone();
        event.as_object_mut().unwrap().remove("structured_content");
        if let Some(value) = &content {
            event["structured_content"] = value.clone();
        }
        let raw = json!({"event":event,"events":[event],"session":{"ctx_session_id":"session-1"}});
        for paged in [false, true] {
            let client = fixture_client(temp.path(), &raw, paged, false);
            let options = ShowSessionOptions {
                limit: paged.then_some(1),
                ..Default::default()
            };
            let response = client.show_session("session-1", options).unwrap();
            let actual = &response.session.unwrap().events[0];
            assert_eq!(actual.structured_content, content);
            assert_eq!(actual.extra["activity"], fixture["activity"]);
            let encoded = serde_json::to_value(actual).unwrap();
            assert_eq!(encoded.get("structuredContent"), content.as_ref());
            if !paged {
                let actual = client
                    .show_event("event-1", Default::default())
                    .unwrap()
                    .event
                    .unwrap()
                    .event
                    .unwrap();
                assert_eq!(actual.structured_content, content);
                assert_eq!(actual.extra["activity"], fixture["activity"]);
            }
        }
    }
}

#[test]
fn typed_errors_survive_cli_and_mcp() {
    let errors: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../contracts/agent-history-v1/fixtures/cli/producer-errors.json"
    ))
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    for producer in errors {
        for mcp in [false, true] {
            let client = fixture_client(temp.path(), &producer, mcp, true);
            let result = if mcp {
                client.show_session(
                    "session-1",
                    ShowSessionOptions {
                        limit: Some(1),
                        ..Default::default()
                    },
                )
            } else {
                client.show_event("event-1", Default::default())
            };
            let error = result.unwrap_err();
            assert_eq!(
                error.body.retryable,
                producer["retryable"].as_bool().unwrap()
            );
            assert_eq!(error.body.details.unwrap()["producerError"], producer);
        }
    }
}

#[test]
fn literal_search_query_follows_all_options() {
    let temp = tempfile::tempdir().unwrap();
    for query in ["--help", "--refresh=off", "-needle", "two words", "a'雪"] {
        let client = fixture_client(temp.path(), &json!({"results":[]}), false, false);
        client
            .search(SearchOptions {
                query: Some(query.to_owned()),
                terms: vec!["--help".to_owned()],
                ..Default::default()
            })
            .unwrap();
        let argv = fs::read_to_string(temp.path().join("argv")).unwrap();
        let args = argv.lines().collect::<Vec<_>>();
        assert_eq!(&args[args.len() - 2..], &["--", query]);
        assert!(args.contains(&"--term=--help"));
    }
}

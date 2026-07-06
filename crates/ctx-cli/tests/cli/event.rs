#[allow(unused_imports)]
use super::*;

pub(crate) fn analytics_event_properties(event: &Value) -> &serde_json::Map<String, Value> {
    event["events"][0]["properties"].as_object().unwrap()
}

pub(crate) fn analytics_cli_event(event: &Value) -> &Value {
    &event["events"][0]
}

pub(crate) fn assert_provider_citations(result: &Value, provider: &str) {
    let citations = result["citations"].as_array().unwrap();
    assert!(!citations.is_empty(), "missing citations in {result:#}");
    for citation in citations {
        assert!(
            citation["ctx_event_id"].is_string() || citation["ctx_session_id"].is_string(),
            "citation needs a ctx-owned event or session id in {citation:#}"
        );
        assert_eq!(citation["provider"], provider, "citation provider failed");
        assert_eq!(
            citation["source_exists"], true,
            "citation source_exists failed"
        );
        assert!(citation["source_path"].is_string());
        assert!(citation["cursor"].is_string());
    }
}

pub(crate) fn assert_event_suggested_next_commands(result: &Value) {
    let commands = result["suggested_next_commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .all(|command| !command.as_str().unwrap_or("").contains("--mode lite")),
        "lite default should not be restated in suggestions: {result:#}"
    );
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx show event ")),
        "missing show event suggestion in {result:#}"
    );
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx show session ")),
        "missing show session suggestion in {result:#}"
    );
    assert!(
        !commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx export session ")),
        "search should not suggest exporting transcripts by default in {result:#}"
    );
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx locate event ")),
        "missing locate event suggestion in {result:#}"
    );
}

#[test]
pub(crate) fn hosted_install_marker_enriches_analytics_event_without_properties_leak() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let binary = copied_ctx_binary(&temp);
    let install_attempt_id = "attempt_01JZCTXHOSTED";
    let marker_secret = "marker-secret-must-not-leak";
    fs::write(
        hosted_install_marker_path(&binary),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "install_attempt_id": install_attempt_id,
            "installer_private_note": marker_secret,
        }))
        .unwrap(),
    )
    .unwrap();

    ctx_from_binary(&temp, &binary)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let cli_event = analytics_cli_event(&events[0]);
    assert_eq!(cli_event["install_attempt_id"], install_attempt_id);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["install_manager"], "ctx-hosted-installer");
    assert!(
        properties.get("install_attempt_id").is_none(),
        "raw marker id must stay out of analytics properties: {properties:#?}"
    );
    assert_no_json_string_contains(
        &Value::Object(properties.clone()),
        &[install_attempt_id, marker_secret],
    );
}

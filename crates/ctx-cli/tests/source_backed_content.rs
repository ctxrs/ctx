mod support;

use ctx_history_capture::ingest_codex_source_backed_v0;
use support::*;

const BEGIN_SENTINEL: &str = "CTX_HYDRATION_BEGIN-";
const END_SENTINEL: &str = "-CTX_HYDRATION_END";
const SOURCE_INDEX_QUERY: &str = "sourceindexsentinel";
const SOURCE_INDEX_PROVIDER_SESSION_ID: &str = "019fa000-0000-7000-8000-000000000091";

struct SourceIndexedMessage {
    temp: TempDir,
    source_root: PathBuf,
    source: PathBuf,
    index_root: PathBuf,
    event_id: String,
    session_id: String,
    complete_text: String,
}

fn source_indexed_codex_message() -> SourceIndexedMessage {
    let temp = tempdir();
    let source_root = temp.path().join(".codex/sessions");
    let source = source_root.join(format!(
        "2026/07/28/rollout-{SOURCE_INDEX_PROVIDER_SESSION_ID}.jsonl"
    ));
    let index_root = temp.path().join("source-backed-lexical-v0");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let complete_text = format!(
        "{SOURCE_INDEX_QUERY} {BEGIN_SENTINEL}{}{END_SENTINEL}",
        "y".repeat(20_000)
    );
    let records = [
        json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": SOURCE_INDEX_PROVIDER_SESSION_ID,
                "timestamp": "2026-07-28T12:00:00Z",
                "cwd": "/workspace/source-index",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "model_provider": "openai"
            }
        }),
        json!({
            "timestamp": "2026-07-28T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": complete_text
                }],
                "phase": "final_answer"
            }
        }),
    ];
    let transcript = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect::<String>();
    fs::write(&source, transcript).unwrap();
    ingest_codex_source_backed_v0(&source_root, &index_root).unwrap();

    let bootstrap = json_output(ctx(&temp).args([
        "search",
        SOURCE_INDEX_QUERY,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(bootstrap["payload_type"], "search_results");
    assert_eq!(bootstrap["retrieval"]["index"], "source_backed");
    let result = bootstrap["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("source-backed bootstrap search result");
    assert_eq!(result["provider"], "codex");
    assert_eq!(
        result["provider_session_id"],
        SOURCE_INDEX_PROVIDER_SESSION_ID
    );

    SourceIndexedMessage {
        temp,
        source_root,
        source,
        index_root,
        event_id: result["ctx_event_id"].as_str().unwrap().to_owned(),
        session_id: result["ctx_session_id"].as_str().unwrap().to_owned(),
        complete_text,
    }
}

fn assert_no_legacy_store(fixture: &SourceIndexedMessage) {
    assert!(
        !fixture.temp.path().join("work.sqlite").exists(),
        "source-generation show must not initialize or read the legacy Store"
    );
}

fn mcp_initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": "init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    })
}

fn mcp_show_event(fixture: &SourceIndexedMessage, content: &str) -> Value {
    let responses = mcp_roundtrip(
        &fixture.temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": "show-event",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {
                        "ctx_event_id": &fixture.event_id[..8],
                        "content": content
                    }
                }
            }),
        ],
    );
    responses[1].clone()
}

fn mcp_show_session(fixture: &SourceIndexedMessage, content: &str) -> Value {
    let responses = mcp_roundtrip(
        &fixture.temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": "show-session",
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": &fixture.session_id[..8],
                        "mode": "full",
                        "content": content
                    }
                }
            }),
        ],
    );
    responses[1].clone()
}

#[test]
fn cli_show_uses_the_generation_preview_and_exact_provider_hydration() {
    let fixture = source_indexed_codex_message();
    let event_prefix = &fixture.event_id[..8];

    let indexed = json_output(ctx(&fixture.temp).args([
        "show",
        "event",
        event_prefix,
        "--content",
        "indexed",
        "--format=json",
    ]));
    assert_eq!(indexed["payload_type"], "event_window");
    assert_eq!(indexed["event"]["ctx_event_id"], fixture.event_id);
    assert_eq!(indexed["event"]["content"]["requested"], "indexed");
    assert_eq!(indexed["event"]["content"]["complete"], false);
    assert_eq!(indexed["event"]["content"]["origin"], "ctx_index");
    assert_eq!(indexed["event"]["content"]["source_verified"], false);
    assert_eq!(
        indexed["event"]["content"]["complete_content_available"],
        true
    );
    assert!(indexed["event"]["text"]
        .as_str()
        .unwrap()
        .starts_with(SOURCE_INDEX_QUERY));
    assert!(!indexed["event"]["text"]
        .as_str()
        .unwrap()
        .contains(END_SENTINEL));

    let complete = json_output(ctx(&fixture.temp).args([
        "show",
        "event",
        event_prefix,
        "--content",
        "complete",
        "--format=json",
    ]));
    assert_eq!(complete["event"]["text"], fixture.complete_text);
    assert_eq!(complete["event"]["content"]["requested"], "complete");
    assert_eq!(complete["event"]["content"]["complete"], true);
    assert_eq!(complete["event"]["content"]["origin"], "provider_source");
    assert_eq!(complete["event"]["content"]["source_verified"], true);

    let session = json_output(ctx(&fixture.temp).args([
        "show",
        "session",
        &fixture.session_id[..8],
        "--mode",
        "full",
        "--content",
        "complete",
        "--format=json",
    ]));
    assert_eq!(session["payload_type"], "session_transcript");
    assert_eq!(session["ctx_session_id"], fixture.session_id);
    assert_eq!(session["provider"], "codex");
    assert_eq!(
        session["provider_session_id"],
        SOURCE_INDEX_PROVIDER_SESSION_ID
    );
    assert_eq!(session["events"][0]["text"], fixture.complete_text);

    let text = ctx(&fixture.temp)
        .args(["show", "event", event_prefix])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(text)
        .unwrap()
        .contains(SOURCE_INDEX_QUERY));

    let export_path = fixture.temp.path().join("nested/transcript.md");
    ctx(&fixture.temp)
        .args([
            "show",
            "session",
            &fixture.session_id,
            "--mode",
            "full",
            "--format",
            "markdown",
            "--out",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let exported = fs::read_to_string(export_path).unwrap();
    assert!(exported.contains(&format!(
        "# codex session {SOURCE_INDEX_PROVIDER_SESSION_ID}"
    )));
    assert!(exported.contains(SOURCE_INDEX_QUERY));

    let jsonl = ctx(&fixture.temp)
        .args([
            "show",
            "session",
            &fixture.session_id,
            "--mode",
            "full",
            "--format=jsonl",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jsonl: Value = serde_json::from_slice(&jsonl).unwrap();
    assert_eq!(jsonl["payload_type"], "session_transcript_event");
    assert_eq!(jsonl["event"]["ctx_event_id"], fixture.event_id);
    assert_no_legacy_store(&fixture);
}

#[test]
fn mcp_show_uses_the_same_generation_and_provider_only_route() {
    let fixture = source_indexed_codex_message();

    let event_response = mcp_show_event(&fixture, "complete");
    let event = &event_response["result"]["structuredContent"];
    assert_eq!(event["payload_type"], "event_window");
    assert_eq!(event["event"]["ctx_event_id"], fixture.event_id);
    assert_eq!(event["event"]["text"], fixture.complete_text);
    assert_eq!(event["event"]["content"]["origin"], "provider_source");
    assert_eq!(event["event"]["content"]["source_verified"], true);
    assert_useful_mcp_text(
        &event_response["result"],
        &["ctx show event", &fixture.event_id, BEGIN_SENTINEL],
    );

    let session_response = mcp_show_session(&fixture, "indexed");
    let session = &session_response["result"]["structuredContent"];
    assert_eq!(session["payload_type"], "session_transcript");
    assert_eq!(session["ctx_session_id"], fixture.session_id);
    assert_eq!(session["events"][0]["content"]["complete"], false);
    assert_eq!(session["events"][0]["content"]["origin"], "ctx_index");
    assert_useful_mcp_text(
        &session_response["result"],
        &["ctx show session", &fixture.session_id, "provider: codex"],
    );
    assert_no_legacy_store(&fixture);
}

#[test]
fn stale_locator_hydration_fails_closed_while_indexed_preview_remains_bounded() {
    let fixture = source_indexed_codex_message();
    let original = fs::read_to_string(&fixture.source).unwrap();
    let changed = original.replacen(BEGIN_SENTINEL, "STALE_LOCATOR_BEGIN-", 1);
    assert_ne!(changed, original);
    fs::write(&fixture.source, changed).unwrap();

    let indexed = json_output(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id,
        "--content",
        "indexed",
        "--format=json",
    ]));
    assert!(indexed["event"]["text"]
        .as_str()
        .unwrap()
        .starts_with(SOURCE_INDEX_QUERY));
    assert!(!indexed["event"]["text"]
        .as_str()
        .unwrap()
        .contains(END_SENTINEL));

    let stderr = failure_stderr(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id,
        "--content",
        "complete",
        "--format=json",
    ]));
    assert!(stderr.contains("StaleRecordEvidence"), "{stderr}");
    assert!(!stderr.contains(END_SENTINEL));

    let response = mcp_show_event(&fixture, "complete");
    assert_eq!(response["result"]["isError"], true);
    let error = response["result"]["structuredContent"]["error"]
        .as_str()
        .unwrap();
    assert!(error.contains("StaleRecordEvidence"), "{error}");
    assert!(!error.contains(END_SENTINEL));
    assert_no_legacy_store(&fixture);
}

#[test]
fn confirmed_source_deletion_retires_show_without_store_fallback() {
    let fixture = source_indexed_codex_message();
    fs::remove_file(&fixture.source).unwrap();
    ingest_codex_source_backed_v0(&fixture.source_root, &fixture.index_root).unwrap();

    let stderr = failure_stderr(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id,
        "--format=json",
    ]));
    assert!(
        stderr.contains("was not found in the source-backed Core generation"),
        "{stderr}"
    );

    let response = mcp_show_event(&fixture, "indexed");
    assert_eq!(response["result"]["isError"], true);
    let error = response["result"]["structuredContent"]["error"]
        .as_str()
        .unwrap();
    assert!(
        error.contains("was not found in the source-backed Core generation"),
        "{error}"
    );
    assert_no_legacy_store(&fixture);
}

#[test]
fn show_requires_a_source_generation_without_initializing_the_store() {
    let temp = tempdir();
    let event_id = "019fa000-0000-7000-8000-000000000099";

    let stderr = failure_stderr(ctx(&temp).args(["show", "event", event_id]));
    assert!(
        stderr.contains("source-backed Core index is not initialized"),
        "{stderr}"
    );

    let responses = mcp_roundtrip(
        &temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": "show-event",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {"ctx_event_id": event_id}
                }
            }),
        ],
    );
    assert_eq!(responses[1]["result"]["isError"], true);
    assert!(responses[1]["result"]["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("source-backed Core index is not initialized"));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "missing source generations must fail without initializing the Store"
    );
}

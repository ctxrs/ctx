mod support;

use ctx_history_capture::complete_content::{
    VerifiedContentLocatorsV1, VerifiedContentRole, COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS,
    VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use support::*;

const BEGIN_SENTINEL: &str = "CTX_HYDRATION_BEGIN-";
const END_SENTINEL: &str = "-CTX_HYDRATION_END";
const PROVIDER_SESSION_ID: &str = "source-backed-regression-session";
const SOURCE_INDEX_QUERY: &str = "sourceindexsentinel";
const SOURCE_INDEX_PROVIDER_SESSION_ID: &str = "019fa000-0000-7000-8000-000000000091";

struct ImportedMessage {
    temp: TempDir,
    source: PathBuf,
    event_id: String,
    complete_text: String,
}

struct SourceIndexedMessage {
    temp: TempDir,
    source: PathBuf,
    event_id: String,
    session_id: String,
    complete_text: String,
}

fn source_indexed_codex_message() -> SourceIndexedMessage {
    let temp = tempdir();
    let source = temp.path().join(format!(
        ".codex/sessions/2026/07/28/rollout-{SOURCE_INDEX_PROVIDER_SESSION_ID}.jsonl"
    ));
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

    let bootstrap = json_output(ctx(&temp).args([
        "search",
        SOURCE_INDEX_QUERY,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(bootstrap["payload_type"], "search_results");
    assert_eq!(bootstrap["retrieval"]["index"], "source_backed");
    assert_eq!(bootstrap["retrieval"]["effective_mode"], "lexical");
    let result = bootstrap["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("source-backed bootstrap search result");
    assert_eq!(result["provider"], "codex");
    assert_eq!(
        result["provider_session_id"],
        SOURCE_INDEX_PROVIDER_SESSION_ID
    );
    let event_id = result["ctx_event_id"].as_str().unwrap().to_owned();
    let session_id = result["ctx_session_id"].as_str().unwrap().to_owned();

    SourceIndexedMessage {
        temp,
        source,
        event_id,
        session_id,
        complete_text,
    }
}

fn import_truncated_codex_message() -> ImportedMessage {
    let temp = tempdir();
    let source = temp
        .path()
        .join(".codex/sessions/2026/07/23/source-backed.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let complete_text = format!("{BEGIN_SENTINEL}{}{END_SENTINEL}", "x".repeat(20_000));
    let records = [
        json!({
            "timestamp": "2026-07-23T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": PROVIDER_SESSION_ID,
                "timestamp": "2026-07-23T00:00:00Z",
                "cwd": "/workspace/project",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-23T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "msg_source_backed_regression",
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

    let report = json_output(
        ctx(&temp)
            .arg("import")
            .args(["--provider", "codex", "--path"])
            .arg(&source)
            .args(["--no-daemon", "--format=json", "--progress", "none"]),
    );
    assert_eq!(report["totals"]["failed_sources"], 0);
    assert_eq!(report["totals"]["imported_events"], 1);

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let (event_id, payload, metadata): (String, String, String) = conn
        .query_row(
            "SELECT id, payload_json, metadata_json
             FROM events
             WHERE event_type = 'message' AND role = 'assistant'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    let indexed_text = payload["body"]["text"].as_str().unwrap();
    assert_eq!(payload["provider"], "codex");
    assert_eq!(payload["body"]["item_type"], "message");
    assert_eq!(payload["body"]["truncated"], true);
    assert_eq!(
        indexed_text.chars().count(),
        COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS
    );
    assert!(indexed_text.starts_with(BEGIN_SENTINEL));
    assert!(!indexed_text.contains(END_SENTINEL));
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .expect("verified content locators");
    let locator = locators
        .locator(VerifiedContentRole::MessageBody)
        .expect("message-body locator");
    assert_eq!(locator.kind(), "jsonl-range-v1");

    ImportedMessage {
        temp,
        source,
        event_id,
        complete_text,
    }
}

fn locate_event(fixture: &ImportedMessage) -> Value {
    json_output(ctx(&fixture.temp).args(["locate", "event", &fixture.event_id, "--format=json"]))
}

fn show_event(fixture: &ImportedMessage, content: &str) -> Value {
    json_output(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id,
        "--content",
        content,
        "--format=json",
    ]))
}

fn show_session(fixture: &ImportedMessage, content: &str) -> Value {
    json_output(ctx(&fixture.temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        PROVIDER_SESSION_ID,
        "--mode",
        "full",
        "--content",
        content,
        "--format=json",
    ]))
}

fn assert_complete_failure(fixture: &ImportedMessage, expected_error: &str) {
    let output = ctx(&fixture.temp)
        .args([
            "show",
            "event",
            &fixture.event_id,
            "--content",
            "complete",
            "--format=json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "failed hydration wrote partial stdout"
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"], expected_error);
    assert_eq!(error["error_code"], expected_error);
    assert_eq!(error["ctx_event_id"], fixture.event_id);
    assert_eq!(
        output.stderr.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(END_SENTINEL));
}

#[test]
fn source_generation_is_the_normal_core_search_and_show_route() {
    let fixture = source_indexed_codex_message();

    let search =
        json_output(ctx(&fixture.temp).args(["search", SOURCE_INDEX_QUERY, "--format=json"]));
    assert_eq!(search["payload_type"], "search_results");
    assert_eq!(search["freshness"]["mode"], "background");
    assert_eq!(search["freshness"]["status"], "existing_generation");
    assert_eq!(search["retrieval"]["requested_mode"], "lexical");
    assert_eq!(search["retrieval"]["effective_mode"], "lexical");
    assert_eq!(search["retrieval"]["index"], "source_backed");
    assert_eq!(search["results"][0]["result_type"], "session_result");
    assert_eq!(search["results"][0]["ctx_event_id"], fixture.event_id);
    assert_eq!(search["results"][0]["ctx_session_id"], fixture.session_id);
    assert_eq!(search["results"][0]["agent_type"], "primary");
    assert_eq!(search["results"][0]["is_primary"], true);

    let filtered = json_output(ctx(&fixture.temp).args([
        "search",
        SOURCE_INDEX_QUERY,
        "--provider",
        "codex",
        "--source-format",
        "codex_session_jsonl_tree",
        "--workspace",
        "SOURCE-INDEX",
        "--since",
        "2026-07-28T12:00:00Z",
        "--event-type",
        "message",
        "--primary-only",
        "--format=json",
    ]));
    assert_eq!(filtered["results"][0]["ctx_event_id"], fixture.event_id);
    assert_eq!(filtered["filters"]["provider"], "codex");
    assert_eq!(
        filtered["filters"]["source_format"],
        "codex_session_jsonl_tree"
    );
    assert_eq!(filtered["filters"]["workspace"], "SOURCE-INDEX");
    assert_eq!(filtered["filters"]["event_type"], "message");
    assert_eq!(filtered["filters"]["primary_only"], true);

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
    let markdown = ctx(&fixture.temp)
        .args(["show", "event", event_prefix, "--format=markdown"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(markdown)
        .unwrap()
        .contains(&format!("# Event {}", fixture.event_id)));
    let jsonl = ctx(&fixture.temp)
        .args([
            "show",
            "session",
            &fixture.session_id[..8],
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

    assert!(
        !fixture.temp.path().join("work.sqlite").exists(),
        "normal source-index routing must not initialize the legacy store"
    );
}

#[test]
fn source_generation_rejects_unsupported_routes_without_legacy_fallback() {
    let fixture = source_indexed_codex_message();

    let backend_error = failure_stderr(ctx(&fixture.temp).args([
        "search",
        SOURCE_INDEX_QUERY,
        "--backend",
        "semantic",
        "--format=json",
    ]));
    assert!(backend_error.contains("supports lexical retrieval only"));
    assert!(backend_error.contains("no legacy backend was used"));

    let provider_session_error = failure_stderr(ctx(&fixture.temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        SOURCE_INDEX_PROVIDER_SESSION_ID,
        "--format=json",
    ]));
    assert!(provider_session_error.contains("provider-session lookup is not yet exposed"));
    assert!(
        !fixture.temp.path().join("work.sqlite").exists(),
        "unsupported source-index requests must not initialize the legacy store"
    );
}

#[test]
fn source_generation_routes_search_and_prefix_show_over_mcp() {
    let fixture = source_indexed_codex_message();
    let responses = mcp_roundtrip(
        &fixture.temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": SOURCE_INDEX_QUERY,
                        "provider": "codex",
                        "source_format": "codex_session_jsonl_tree",
                        "workspace": "SOURCE-INDEX",
                        "since": "2026-07-28T12:00:00Z",
                        "event_type": "message",
                        "primary_only": true
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {
                        "ctx_event_id": &fixture.event_id[..8],
                        "content": "complete"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": &fixture.session_id[..8],
                        "mode": "full",
                        "content": "indexed"
                    }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 4);
    let search = &responses[1]["result"]["structuredContent"];
    assert_eq!(search["payload_type"], "search_results");
    assert_eq!(search["retrieval"]["index"], "source_backed");
    assert_eq!(search["results"][0]["ctx_event_id"], fixture.event_id);
    assert_useful_mcp_text(
        &responses[1]["result"],
        &["ctx search", SOURCE_INDEX_QUERY, "provider: codex"],
    );

    let event = &responses[2]["result"]["structuredContent"];
    assert_eq!(event["payload_type"], "event_window");
    assert_eq!(event["event"]["text"], fixture.complete_text);
    assert_eq!(event["event"]["content"]["source_verified"], true);
    assert_useful_mcp_text(
        &responses[2]["result"],
        &["ctx show event", &fixture.event_id, BEGIN_SENTINEL],
    );

    let session = &responses[3]["result"]["structuredContent"];
    assert_eq!(session["payload_type"], "session_transcript");
    assert_eq!(session["ctx_session_id"], fixture.session_id);
    assert_eq!(session["events"][0]["content"]["complete"], false);
    assert_useful_mcp_text(
        &responses[3]["result"],
        &["ctx show session", &fixture.session_id, "provider: codex"],
    );
    assert!(
        !fixture.temp.path().join("work.sqlite").exists(),
        "source-index MCP routing must not initialize the legacy store"
    );
}

#[test]
fn codex_message_locates_and_hydrates_verified_complete_content() {
    let fixture = import_truncated_codex_message();

    let location = locate_event(&fixture);
    assert_eq!(location["complete_content"]["available"], true);
    assert_eq!(location["complete_content"]["source_family"], "jsonl");
    assert_eq!(
        location["complete_content"]["locator_kind"],
        "jsonl-range-v1"
    );
    assert_eq!(location["source"]["exists"], true);
    assert_eq!(
        location["source"]["path"],
        fixture.source.to_string_lossy().as_ref()
    );

    let indexed = show_event(&fixture, "indexed");
    assert_eq!(
        indexed["event"]["text"].as_str().unwrap().chars().count(),
        COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS
    );
    assert_eq!(indexed["event"]["content"]["requested"], "indexed");
    assert_eq!(indexed["event"]["content"]["complete"], false);
    assert_eq!(indexed["event"]["content"]["origin"], "ctx_index");
    assert_eq!(indexed["event"]["content"]["stored_truncated"], true);
    assert_eq!(indexed["event"]["content"]["source_verified"], false);
    assert!(!indexed["event"]["text"]
        .as_str()
        .unwrap()
        .contains(END_SENTINEL));

    let complete = show_event(&fixture, "complete");
    assert_eq!(complete["event"]["text"], fixture.complete_text);
    assert_eq!(complete["event"]["content"]["requested"], "complete");
    assert_eq!(complete["event"]["content"]["complete"], true);
    assert_eq!(complete["event"]["content"]["origin"], "provider_source");
    assert_eq!(complete["event"]["content"]["stored_truncated"], true);
    assert_eq!(complete["event"]["content"]["source_verified"], true);
    assert!(complete["event"]["text"]
        .as_str()
        .unwrap()
        .ends_with(END_SENTINEL));
}

#[test]
fn codex_session_reopens_verified_complete_content() {
    let fixture = import_truncated_codex_message();

    let complete = show_session(&fixture, "complete");
    assert_eq!(complete["content_policy"], "complete");
    assert_eq!(complete["provider"], "codex");
    assert_eq!(complete["provider_session_id"], PROVIDER_SESSION_ID);
    let event = complete["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["ctx_event_id"] == fixture.event_id)
        .unwrap();
    assert_eq!(event["text"], fixture.complete_text);
    assert_eq!(event["content"]["requested"], "complete");
    assert_eq!(event["content"]["complete"], true);
    assert_eq!(event["content"]["origin"], "provider_source");
    assert_eq!(event["content"]["stored_truncated"], true);
    assert_eq!(event["content"]["source_verified"], true);
}

#[test]
fn modified_codex_source_fails_closed_without_partial_content() {
    let fixture = import_truncated_codex_message();
    let original = fs::read_to_string(&fixture.source).unwrap();
    let changed = original.replacen(BEGIN_SENTINEL, "MTX_HYDRATION_BEGIN-", 1);
    assert_ne!(changed, original);
    fs::write(&fixture.source, changed).unwrap();

    assert_complete_failure(&fixture, "source_changed");
}

#[test]
fn missing_codex_source_fails_closed_without_partial_content() {
    let fixture = import_truncated_codex_message();
    fs::remove_file(&fixture.source).unwrap();

    let location = locate_event(&fixture);
    assert_eq!(location["source"]["exists"], false);
    assert_eq!(location["complete_content"]["available"], true);
    assert_complete_failure(&fixture, "source_missing");
}

fn proto_varint(mut value: u64) -> Vec<u8> {
    let mut encoded = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        encoded.push(byte);
        if value == 0 {
            return encoded;
        }
    }
}

fn proto_field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut encoded = proto_varint(u64::from(number) << 3 | 2);
    encoded.extend(proto_varint(payload.len() as u64));
    encoded.extend_from_slice(payload);
    encoded
}

fn warp_message_task(body: &str) -> Vec<u8> {
    let user = proto_field(1, body.as_bytes());
    let mut message = proto_field(1, b"warp-source-backed-message");
    message.extend(proto_field(2, &user));
    message.extend(proto_field(11, b"warp-source-backed-task"));
    let mut task = proto_field(1, b"warp-source-backed-task");
    task.extend(proto_field(5, &message));
    task
}

fn create_warp_source(path: &Path, body: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma user_version = 1;
             create table agent_conversations (
                 id integer primary key,
                 conversation_id text not null unique,
                 conversation_data text not null,
                 last_modified_at text not null
             );
             create table agent_tasks (
                 id integer primary key,
                 conversation_id text not null,
                 task_id text not null unique,
                 task blob not null,
                 last_modified_at text not null
             );
             create table ai_queries (
                 id integer primary key,
                 exchange_id text not null unique,
                 conversation_id text not null,
                 start_ts text not null,
                 input text not null,
                 working_directory text,
                 output_status text not null,
                 model_id text not null,
                 planning_model_id text not null default '',
                 coding_model_id text not null default ''
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into agent_conversations
             (conversation_id, conversation_data, last_modified_at)
             values ('warp-source-backed-session',
                     '{\"agent_name\":\"Warp\",\"run_id\":\"warp-source-backed-run\"}',
                     '2026-07-26 12:00:00')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "insert into agent_tasks
             (conversation_id, task_id, task, last_modified_at)
             values ('warp-source-backed-session', 'warp-source-backed-task', ?1,
                     '2026-07-26 12:00:01')",
            params![warp_message_task(body)],
        )
        .unwrap();
}

fn import_truncated_warp_message() -> ImportedMessage {
    let temp = tempdir();
    let source = temp.path().join("warp-source-backed.sqlite");
    let complete_text = format!("{BEGIN_SENTINEL}{}{END_SENTINEL}", "w".repeat(20_000));
    create_warp_source(&source, &complete_text);
    let report = json_output(
        ctx(&temp)
            .arg("import")
            .args(["--provider", "warp", "--path"])
            .arg(&source)
            .args(["--no-daemon", "--format=json", "--progress", "none"]),
    );
    assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
    assert_eq!(report["totals"]["imported_events"], 1, "{report:#}");

    let connection = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let (event_id, payload, metadata): (String, String, String) = connection
        .query_row(
            "select id, payload_json, metadata_json from events where event_type = 'message'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(payload["provider"], "warp");
    assert_eq!(payload["text_retention"]["mode"], "bounded");
    assert_eq!(payload["text_retention"]["truncated"], true);
    assert_eq!(
        payload["text"].as_str().unwrap().chars().count(),
        COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS
    );
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    assert_eq!(
        locators
            .locator(VerifiedContentRole::MessageBody)
            .unwrap()
            .kind(),
        "warp-task-message-v1"
    );
    ImportedMessage {
        temp,
        source,
        event_id,
        complete_text,
    }
}

#[test]
fn warp_message_hydrates_complete_content_and_fails_closed_after_mutation() {
    let fixture = import_truncated_warp_message();
    let complete = show_event(&fixture, "complete");
    assert_eq!(complete["event"]["text"], fixture.complete_text);
    assert_eq!(complete["event"]["content"]["origin"], "provider_source");
    assert_eq!(complete["event"]["content"]["source_verified"], true);

    Connection::open(&fixture.source)
        .unwrap()
        .execute(
            "update agent_tasks set task = ?1, last_modified_at = '2026-07-26 12:00:02'",
            params![warp_message_task("mutated Warp body")],
        )
        .unwrap();
    assert_complete_failure(&fixture, "content_verification_failed");
}

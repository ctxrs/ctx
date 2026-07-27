mod support;

use ctx_history_capture::complete_content::{
    VerifiedContentLocatorsV1, VerifiedContentRole, COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS,
    VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use support::*;

const BEGIN_SENTINEL: &str = "CTX_HYDRATION_BEGIN-";
const END_SENTINEL: &str = "-CTX_HYDRATION_END";

struct ImportedMessage {
    temp: TempDir,
    source: PathBuf,
    event_id: String,
    complete_text: String,
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
                "id": "source-backed-regression-session",
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
            .args(["--no-daemon", "--json", "--progress", "none"]),
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
    json_output(ctx(&fixture.temp).args(["locate", "event", &fixture.event_id, "--json"]))
}

fn show_event(fixture: &ImportedMessage, content: &str) -> Value {
    json_output(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id,
        "--content",
        content,
        "--json",
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
            "--json",
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

//! Authored v3.2 contract regressions, independent of private source data.
use super::*;

#[test]
fn unsafe_values_name_the_displaced_value_and_preserve_native_format() {
    use ctx_history_asr_core::{blobs, numbers};
    let dir = tempfile::tempdir().unwrap();
    let native = numbers::decode(r#"{"overflow":1e309,"wide":18446744073709551617}"#).unwrap();
    let mut record = json!({"type":"example/native","native":{"format":"synthetic-json","record":native},"_meta":{"com.openai.codex/remainingFields":{"counter":u64::MAX}}});
    blobs::numbers(dir.path(), &mut record, Provider::Codex).unwrap();
    assert_eq!(record["native"]["format"], "synthetic-json");
    assert!(record["native"].get("record").is_none());
    let digest = record["native"]["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("blobs/sha256").join(digest)).unwrap(),
        r#"{"overflow":1e309,"wide":18446744073709551617}"#
    );
    assert_eq!(
        record["_meta"]["com.openai.codex/jsonText"]["/_meta/com.openai.codex~1remainingFields"],
        r#"{"counter":18446744073709551615}"#
    );
    let mut required = json!({"seq":u64::MAX});
    assert!(blobs::numbers(dir.path(), &mut required, Provider::Codex).is_err());
    assert!(numbers::decode(r#"{"a":1e309,"a":2}"#).is_none());
}

#[test]
fn kimi_main_and_installation_scope_survive_repeated_exports() {
    let (_dir, mut options) = super::cli_journals::journal_fixture(Provider::Kimi);
    options.namespace = Some("installation:one".into());
    export(&options).unwrap();
    let before = records(&options);
    assert_eq!(before[0]["sessionId"], "main");
    assert_eq!(
        before[0]["namespace"],
        r#"["installation:one","kimi-test"]"#
    );
    options.namespace = None;
    assert_eq!(export(&options).unwrap().unchanged, 1);
    assert_eq!(records(&options), before);
    let mut references = json!({"lineage":{"spawnedBy":{"session":"com.moonshot.kimi-code:parent:main"}},"title":"com.moonshot.kimi-code:ordinary-text","_meta":{"com.moonshot.kimi-code/remainingFields":{"reference":"com.moonshot.kimi-code:native-text"}}});
    ctx_history_asr_core::scope::qualify(&mut references, Provider::Kimi, "install").unwrap();
    assert_eq!(
        references["lineage"]["spawnedBy"]["session"],
        "com.moonshot.kimi-code:%5B%22install%22%2C%22parent%22%5D:main"
    );
    assert_eq!(references["title"], "com.moonshot.kimi-code:ordinary-text");
}

#[test]
fn placeholder_usage_is_native_and_reported_zero_remains_a_value() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"assistant","sessionId":"test","uuid":"placeholder","message":{"id":"placeholder","model":"<synthetic>","content":[],"usage":{"input_tokens":0,"output_tokens":0}}}),
            json!({"type":"assistant","sessionId":"test","uuid":"reported","message":{"id":"reported","model":"served","content":[],"stop_reason":"end_turn","usage":{"input_tokens":0,"output_tokens":0}}}),
        ],
    );
    export(&options).unwrap();
    let rows = records(&options);
    let placeholder = rows
        .iter()
        .find(|r| r["inferenceId"] == "placeholder" && r["type"] == "inference")
        .unwrap();
    assert!(placeholder.get("usage").is_none());
    assert_eq!(
        placeholder["_meta"]["com.anthropic.claude-code/remainingFields"]["usage"]["output_tokens"],
        0
    );
    let reported = rows
        .iter()
        .find(|r| r["inferenceId"] == "reported" && r["type"] == "inference")
        .unwrap();
    assert_eq!(reported["usage"]["output"]["total"], 0);
}

#[test]
fn claude_copies_reference_projected_entities_and_keep_native_rows() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"assistant","sessionId":"copy","uuid":"new-row","forkedFrom":{"sessionId":"parent","messageUuid":"old-row"},"message":{"id":"response","role":"assistant","content":[{"type":"tool_use","id":"call","name":"read","input":{}}],"usage":{"input_tokens":1,"output_tokens":1}}}),
            json!({"type":"user","sessionId":"copy","uuid":"new-result","forkedFrom":{"sessionId":"parent","messageUuid":"old-result"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"done"}]}}),
        ],
    );
    export(&options).unwrap();
    let rows = records(&options);
    for (kind, expected) in [
        ("message", "messageId=response"),
        ("inference", "inferenceId=response"),
        ("tool_call", "callId=call"),
    ] {
        let r = rows.iter().find(|r| r["type"] == kind).unwrap();
        assert_eq!(r["copiedFrom"]["record"], expected);
    }
    let result = rows.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert!(result["copiedFrom"].get("record").is_none());
    assert_eq!(
        result["_meta"]["com.anthropic.claude-code/remainingFields"]["forkedFrom"]["messageUuid"],
        "old-result"
    );
}

#[test]
fn stored_results_keep_native_shape_and_external_raw_capture() {
    use ctx_history_asr_core::records::Writer;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("records.jsonl");
    let mut writer = Writer::new(
        std::fs::File::create(&file).unwrap(),
        Provider::Goose,
        "test".into(),
    );
    writer.emit(json!({"type":"tool_result","linkage":"none","status":"unknown","content":[{"type":"text","text":"normalized"}],"capture":{"raw":{"state":"external","blob":{"blob":format!("sha256:{}","a".repeat(64)),"size":123}}}}), &Value::Null).unwrap();
    drop(writer);
    let record: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert!(record.get("raw").is_none());
    assert!(record.get("content").is_none());
    assert_eq!(record["capture"]["raw"]["state"], "external");
    let native = json!([{"type":"text","text":"stored","nativeFlag":true}]);
    let (_dir, options) = fixture(
        Provider::Pi,
        &[
            json!({"type":"session","id":"test","version":3}),
            json!({"type":"message","id":"result","parentId":null,"message":{"role":"toolResult","toolCallId":"unseen","content":native}}),
        ],
    );
    export(&options).unwrap();
    let rows = records(&options);
    let result = rows.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["raw"], native);
    assert!(result.get("content").is_none());
    assert_eq!(result["nativeCallId"], "unseen");
}

#[test]
fn registry_destinations_do_not_rewrite_native_data_parts() {
    let data = json!({"_meta":{"com.openai.codex/custom":{"value":1}}});
    let mut record = json!({"type":"message","content":[{"type":"data","data":data,"_meta":{"com.openai.codex/customPart":"fact"}}]});
    ctx_history_asr_core::registry::normalize(&mut record, Provider::Codex);
    assert_eq!(record["content"][0]["data"], data);
    assert_eq!(
        record["content"][0]["_meta"]["com.openai.codex/remainingFields"]["customPart"],
        "fact"
    );
}

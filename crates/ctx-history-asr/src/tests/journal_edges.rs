//! Authored boundary cases, independent of native release qualification.
use super::journals::journal_fixture;
use super::*;
use std::io::Write;

fn append(o: &Options, v: Value) {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap();
    writeln!(f, "{v}").unwrap();
}
#[test]
fn copilot_reused_requests_cannot_disappear_behind_one_start() {
    let (_dir, o) = journal_fixture(Provider::Copilot);
    append(
        &o,
        json!({"type":"assistant.message","id":"repeated-request","data":{"messageId":"repeated","content":"","toolRequests":[{"toolCallId":"call","name":"mcp__ambiguous__name","arguments":"malformed json"}]}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter()
            .filter(|r| r["type"] == "tool_call" && r["callId"] == "call")
            .count(),
        2
    );
    assert_eq!(
        r.iter().find(|r| r["type"] == "tool_result").unwrap()["linkage"],
        "ambiguous"
    );
    assert!(r.iter().any(|r| r["inputText"] == "malformed json"));
}
#[test]
fn native_request_secrets_are_absent_even_from_vendor_payloads() {
    let (_dir, o) = journal_fixture(Provider::Copilot);
    append(
        &o,
        json!({"type":"user_input.requested","id":"question","data":{"requestId":"secret-request","question":"Authored sensitive input","secret":true}}),
    );
    append(
        &o,
        json!({"type":"user_input.completed","id":"answer-secret","data":{"requestId":"secret-request","answer":"authored-sensitive-value","wasFreeform":true}}),
    );
    append(
        &o,
        json!({"type":"future","id":"vendor-secret","data":{"copied":"authored-sensitive-value"}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert!(!serde_json::to_string(&r)
        .unwrap()
        .contains("authored-sensitive-value"));
    let request = r
        .iter()
        .rev()
        .find(|r| r["requestId"] == "secret-request")
        .unwrap();
    assert_eq!(request["status"], "answered");
    assert!(request.get("answers").is_none());
}
#[test]
fn finalized_response_parts_fold_with_bounded_content() {
    for p in [Provider::Copilot, Provider::Kimi] {
        let (_dir, mut o) = journal_fixture(p);
        o.max_record_bytes = 1024;
        for i in 0..8 {
            append(
                &o,
                if p == Provider::Copilot {
                    json!({"type":"assistant.message","id":format!("chunk-{i}"),"data":{"messageId":"chunked","content":"x".repeat(300),"chunkIndex":i,"chunkCount":8}})
                } else {
                    json!({"type":"context.append_loop_event","event":{"type":"content.part","uuid":format!("chunk-{i}"),"stepUuid":"chunked-step","turnId":"2","step":1,"part":{"type":"text","text":"x".repeat(300)}}})
                },
            );
        }
        export(&o).unwrap();
        let r = records(&o);
        let id = if p == Provider::Copilot {
            "chunked"
        } else {
            "step:chunked-step"
        };
        let m: Vec<_> = r.iter().filter(|r| r["messageId"] == id).collect();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["capture"]["content"]["state"], "external");
        let key = m[0]["capture"]["content"]["blob"]["blob"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap();
        let parts: Value =
            serde_json::from_slice(&std::fs::read(o.out.join("blobs/sha256").join(key)).unwrap())
                .unwrap();
        assert_eq!(parts.as_array().unwrap().len(), 8);
        assert_eq!(export(&o).unwrap().unchanged, 1);
    }
}
#[test]
fn droid_same_row_duplicate_ids_and_missing_result_ids() {
    let (_dir, o) = journal_fixture(Provider::Droid);
    append(
        &o,
        json!({"type":"message","id":"duplicate-calls","role":"assistant","content":[{"type":"tool_use","id":"same-row","name":"Read","input":{}},{"type":"tool_use","id":"same-row","name":"Read","input":{}}]}),
    );
    append(
        &o,
        json!({"type":"message","id":"results","role":"user","content":[{"type":"tool_result","tool_use_id":"same-row","content":"one"},{"type":"tool_result","content":"orphan"}]}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter()
            .find(|r| r["type"] == "tool_result"
                && r["_meta"]["ai.factory.droid/nativeCallId"] == "same-row")
            .unwrap()["linkage"],
        "ambiguous"
    );
    let orphan = r
        .iter()
        .find(|r| r["type"] == "tool_result" && r["linkage"] == "none")
        .unwrap();
    assert!(orphan.get("callId").is_none());
    assert_eq!(orphan["status"], "unknown");
}
#[test]
fn kimi_namespace_and_fork_marker_preserve_copy_accounting() {
    let (dir, mut o) = journal_fixture(Provider::Kimi);
    append(&o, json!({"type":"forked"}));
    std::fs::write(
        dir.path().join("sessions/work/kimi-test/state.json"),
        json!({"forkedFrom":"prior-session","agents":{"main":{}}}).to_string(),
    )
    .unwrap();
    o.session = Some("kimi-test".to_owned());
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r[0]["lineage"]["forkedFrom"]["session"],
        "com.moonshot.kimi-code:prior-session:main"
    );
    assert_eq!(r[0]["lineage"]["inherits"]["mode"], "copied");
    assert!(r[0]["lineage"]["inherits"]["localThroughId"].is_string());
}

#[test]
fn journal_upserts_replace_call_indexes() {
    let (_dir, o) = journal_fixture(Provider::Copilot);
    append(
        &o,
        json!({"type":"tool.execution_start","id":"corrected","data":{"toolCallId":"call","toolName":"view","arguments":{}}}),
    );
    append(
        &o,
        json!({"type":"tool.execution_start","id":"corrected","data":{"toolCallId":"different","toolName":"view","arguments":{}}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter()
            .find(|r| r["type"] == "tool_result" && r["callId"] == "call")
            .unwrap()["linkage"],
        "exact"
    );
}

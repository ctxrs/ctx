//! Authored ASR semantics supplement the existing, separately classified native fixtures.
use super::*;
mod edge;
fn pi(id: &str, parent: Value, message: Value) -> Value {
    json!({"type":"message","id":id,"parentId":parent,"message":message})
}
fn grok(id: &str, update: Value) -> Value {
    json!({"method":"session/update","params":{"sessionId":"g","_meta":{"eventId":id,"agentTimestampMs":1700000000000_i64},"update":update}})
}
fn ds(seq: u64, kind: &str, data: Value) -> Value {
    json!({"type":kind,"seq":seq,"time":1700000000000_u64+seq,"data":data})
}
fn pi_header() -> Value {
    json!({"type":"session","version":3,"id":"pi-session","cwd":"/workspace"})
}
fn ds_header() -> Value {
    json!({"type":"session","version":0,"id":"ds-session","createdAt":1700000000000_i64,"seedLength":0,"delegationDepth":0})
}
#[test]
fn pi_model_usage_tool_units_and_real_branches() {
    let (_dir, o) = fixture(
        Provider::Pi,
        &[
            pi_header(),
            pi(
                "u",
                Value::Null,
                json!({"role":"user","content":"question\n"}),
            ),
            pi(
                "a",
                json!("u"),
                json!({"role":"assistant","responseId":"response","model":"alias","responseModel":"served","provider":"test","usage":{"input":10,"output":3,"cacheRead":4,"cacheWrite":2,"totalTokens":19,"cost":{"total":0.0}},"stopReason":"toolUse","content":[{"type":"thinking","thinking":"reason","thinkingSignature":"sig"},{"type":"toolCall","id":"c","name":"read","arguments":{"path":"f"}}]}),
            ),
            pi(
                "r",
                json!("a"),
                json!({"role":"toolResult","toolCallId":"c","isError":false,"content":[{"type":"text","text":"bytes"}]}),
            ),
            pi(
                "b",
                json!("u"),
                json!({"role":"assistant","responseId":"retry","content":[{"type":"text","text":"new answer"}]}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let first = r.iter().find(|r| r["messageId"] == "u").unwrap();
    assert!(first.get("author").is_none());
    let inf = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(inf["usage"]["input"]["uncached"], 10);
    assert_eq!(inf["model"]["id"], "served");
    assert_eq!(inf["cost"]["amount"], 0.0);
    let message = r.iter().find(|r| r["messageId"] == "a").unwrap();
    assert_eq!(message["content"][0]["signature"], "sig");
    assert_eq!(message["content"].as_array().unwrap().len(), 1);
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "exact");
    assert!(result.get("branchesFrom").is_none());
    let branch = r.iter().find(|r| r["messageId"] == "b").unwrap();
    assert_eq!(branch["branchesFrom"]["record"], first["id"]);
}
#[test]
fn pi_missing_parent_is_unknown_even_at_start_and_paths_are_not_ids() {
    let (_dir, o) = fixture(
        Provider::Pi,
        &[
            json!({"type":"session","version":3,"id":"child","parentSession":"/missing/parent.jsonl"}),
            pi(
                "m",
                json!("not-retained"),
                json!({"role":"user","content":"hello"}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["sessionId"], "child");
    assert!(r[0].get("lineage").is_none());
    assert_eq!(r[1]["branchesFrom"]["state"], "unknown");
}
#[test]
fn pi_path_parent_is_resolved_by_header_even_with_child_filter() {
    let (dir, mut o) = fixture(Provider::Pi, &[pi_header()]);
    let parent = dir.path().join("unrelated-filename.jsonl");
    std::fs::write(
        &parent,
        format!(
            "{{\"type\":\"title\",\"v\":1,\"title\":\"\",\"pad\":\"\"}}\n{}\n",
            json!({"type":"session","version":3,"id":"actual-parent"})
        ),
    )
    .unwrap();
    std::fs::write(
        &o.inputs[0].path,
        format!(
            "{}\n",
            json!({"type":"session","version":3,"id":"child","parentSession":parent})
        ),
    )
    .unwrap();
    o.session = Some("child".into());
    export(&o).unwrap();
    assert_eq!(
        records(&o)[0]["lineage"]["forkedFrom"]["session"],
        "pi:actual-parent"
    );
}
#[test]
fn grok_folded_chunks_tool_patches_and_failed_diff() {
    let (_dir, o) = fixture(
        Provider::GrokBuild,
        &[
            grok(
                "1",
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"a"},"_meta":{"promptIndex":0}}),
            ),
            grok(
                "2",
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":" b\n"},"_meta":{"promptIndex":0}}),
            ),
            grok(
                "3",
                json!({"sessionUpdate":"tool_call","toolCallId":"c","title":"Edit it","rawInput":{"path":"a"},"_meta":{"x.ai/tool":{"name":"edit","kind":"edit","namespace":"grok_build"}}}),
            ),
            grok(
                "4",
                json!({"sessionUpdate":"tool_call_update","toolCallId":"c","rawInput":{"path":"b"},"content":[{"type":"diff","path":"/work/b","oldText":"before","newText":"after"}]}),
            ),
            grok(
                "5",
                json!({"sessionUpdate":"tool_call_update","toolCallId":"c","status":"failed","content":[{"type":"content","content":{"type":"text","text":"failure"}}],"rawOutput":{"type":"SearchReplace"}}),
            ),
            grok(
                "6",
                json!({"sessionUpdate":"rewind_marker","target_prompt_index":0}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "message").count(), 1);
    assert_eq!(r[1]["content"][0]["text"], "a b\n");
    let calls: Vec<_> = r.iter().filter(|r| r["type"] == "tool_call").collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["supersedes"], calls[0]["id"]);
    assert_eq!(calls[1]["input"]["path"], "b");
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert!(result.get("content").is_none());
    assert_eq!(result["displayContent"][0]["text"], "failure");
    assert_eq!(result["capture"]["content"]["state"], "absent");
    let changes: Vec<_> = r.iter().filter(|r| r["type"] == "file_change").collect();
    assert_eq!(changes[0]["status"], "proposed");
    assert_eq!(changes[1]["status"], "failed");
    assert_eq!(changes[1]["supersedes"], changes[0]["id"]);
    assert_eq!(r.last().unwrap()["type"], "rewind");
    assert!(r.last().unwrap()["to"].is_null());
}
#[test]
fn grok_chunks_spill_without_truncating_or_inventing_inference() {
    let rows=(0..20).map(|n|grok(&n.to_string(),json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x".repeat(200)}}))).collect::<Vec<_>>();
    let (_dir, mut o) = fixture(Provider::GrokBuild, &rows);
    o.max_record_bytes = 1024;
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.len(), 2);
    assert_eq!(r[1]["capture"]["content"]["state"], "external");
    let hash = r[1]["capture"]["content"]["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let blob: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("blobs/sha256").join(hash)).unwrap())
            .unwrap();
    assert_eq!(blob[0]["text"].as_str().unwrap().len(), 4000);
}
#[test]
fn deepseek_provenance_request_usage_and_replacement() {
    let mut replace = ds(
        7,
        "user/message",
        json!({"id":"summary","role":"user","source":{"kind":"plugin","plugin":"compaction"},"content":[{"type":"text","text":"summary"}]}),
    );
    replace["surfaceOp"] = json!({"op":"replace","start":2,"end":5});
    let (_dir, o) = fixture(
        Provider::DeepSeekHarness,
        &[
            ds_header(),
            ds(0, "turn/start", json!({"turn":1})),
            ds(1, "step/start", json!({"turn":1,"step":1})),
            ds(
                2,
                "user/message",
                json!({"id":"u","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"hello"}]}),
            ),
            ds(
                3,
                "request/header",
                json!({"reason":"initial","header":{"config":{"provider":"p","model":"requested","reasoningEffort":"high"},"system":"system","tools":[]}}),
            ),
            ds(
                4,
                "request/context",
                json!({"provider":"p","model":"requested","contextWindow":1000}),
            ),
            ds(
                5,
                "assistant/message",
                json!({"turn":1,"step":1,"usage":{"inputTokens":5,"outputTokens":3,"cacheReadTokens":2},"message":{"id":"a","role":"assistant","source":{"kind":"model","provider":"p","model":"served","replayState":{"opaque":"state"}},"content":[{"type":"reasoning","text":"reason"},{"type":"text","text":"answer"}]}}),
            ),
            ds(
                6,
                "compaction/summary",
                json!({"compactionId":"compact","shadowedSeqs":[2,5],"summary":[{"type":"text","text":"summary"}],"provider":"p","model":"served"}),
            ),
            replace,
            ds(
                8,
                "user/message",
                json!({"id":"peer","role":"user","source":{"kind":"coordinator","senderSessionId":"parent","form":"relay"},"content":[{"type":"text","text":"follow up"}]}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "u").unwrap()["author"],
        "human"
    );
    let inf = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(inf["usage"]["input"]["uncached"], 5);
    assert_eq!(inf["model"]["id"], "served");
    assert_eq!(inf["model"]["requested"], "requested");
    assert_eq!(inf["context"]["complete"], false);
    assert!(inf["context"].get("entries").is_none());
    let comp = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert_eq!(comp["replaces"]["ids"].as_array().unwrap().len(), 2);
    assert!(r.iter().all(|r| r["messageId"] != "summary"));
    let peer = r.iter().find(|r| r["messageId"] == "peer").unwrap();
    assert_eq!(peer["author"], "agent");
    assert_eq!(peer["from"]["session"], "com.deepseek.harness:parent");
}
#[test]
fn existing_fixture_families_export_and_repeat() {
    // Pi/Grok fixtures are sanitized native captures; DeepSeek fixtures are authored.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history");
    for (provider, name, expected) in [
        (Provider::Pi, "pi-omp/v18.0.10-parent-session/native", 4),
        (Provider::GrokBuild, "grok-build/v1.0.3", 1),
        (Provider::DeepSeekHarness, "deepseek-harness/v0/raw", 2),
        (Provider::DeepSeekHarness, "deepseek-harness/v0/zstd", 2),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let o = Options::new(
            vec![Input {
                provider,
                path: root.join(name),
            }],
            dir.path().join("out"),
        );
        let first = export(&o).unwrap();
        assert_eq!(first.sessions, expected, "{name}");
        assert!(first.records > first.sessions);
        assert_eq!(export(&o).unwrap().unchanged, expected);
    }
}
#[test]
fn new_families_incremental_append_rewrite_relocation_and_tails() {
    for (provider, header, row) in [
        (
            Provider::Pi,
            pi_header(),
            pi("a", Value::Null, json!({"role":"user","content":"hello"})),
        ),
        (
            Provider::DeepSeekHarness,
            ds_header(),
            ds(
                0,
                "user/message",
                json!({"id":"m","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"hello"}]}),
            ),
        ),
        (
            Provider::GrokBuild,
            grok(
                "h",
                json!({"sessionUpdate":"available_commands_update","availableCommands":[]}),
            ),
            grok(
                "m",
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}),
            ),
        ),
    ] {
        let (dir, o) = fixture(provider, std::slice::from_ref(&header));
        export(&o).unwrap();
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&o.inputs[0].path)
            .unwrap();
        writeln!(file, "{row}").unwrap();
        export(&o).unwrap();
        let before = records(&o);
        write!(file, "{{broken").unwrap();
        export(&o).unwrap();
        assert_eq!(records(&o), before);
        writeln!(file).unwrap();
        export(&o).unwrap();
        assert_eq!(records(&o).last().unwrap()["type"], "gap");
        std::fs::write(
            &o.inputs[0].path,
            format!(
                "{header}\n{}\n",
                row.to_string().replace("hello", "changed")
            ),
        )
        .unwrap();
        assert_eq!(export(&o).unwrap().snapshots, 1);
        let changed = records(&o);
        let relocated = dir.path().join("renamed.jsonl");
        std::fs::rename(&o.inputs[0].path, &relocated).unwrap();
        let mut moved = o.clone();
        moved.inputs[0].path = relocated;
        export(&moved).unwrap();
        assert_eq!(records(&moved), changed);
    }
}

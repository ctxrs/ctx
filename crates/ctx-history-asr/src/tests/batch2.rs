//! Authored semantic cases, corroborated against each provider's writer/parser.
use super::*;
use std::io::Write;

fn gemini_header() -> Value {
    json!({"sessionId":"gemini-owner","projectHash":"project","startTime":"2026-01-01T00:00:00Z","kind":"main"})
}
fn gm(id: &str, kind: &str, text: &str) -> Value {
    json!({"id":id,"type":kind,"content":text,"timestamp":"2026-01-01T00:00:01Z"})
}
fn append(options: &Options, v: &Value) {
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&options.inputs[0].path)
            .unwrap(),
        "{v}"
    )
    .unwrap();
}

#[test]
fn gemini_reappends_revise_and_rewind_excludes_target() {
    let (_d, o) = fixture(
        Provider::Gemini,
        &[
            gemini_header(),
            gm("u", "user", "first"),
            gm("a", "gemini", "answer"),
            gm("gone", "user", "remove me"),
        ],
    );
    export(&o).unwrap();
    let before = records(&o);
    append(&o, &gm("a", "gemini", "corrected"));
    append(&o, &json!({"$rewindTo":"gone"}));
    append(&o, &gm("new", "user", "replacement"));
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(&r[..before.len()], &before);
    let revisions: Vec<_> = r.iter().filter(|v| v["messageId"] == "a").collect();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[1]["supersedes"], revisions[0]["id"]);
    assert_eq!(
        r.iter().find(|v| v["type"] == "rewind").unwrap()["to"],
        revisions[1]["id"]
    );
    assert!(r
        .iter()
        .find(|v| v["messageId"] == "new")
        .unwrap()
        .get("branchesFrom")
        .is_none());
    append(&o, &json!({"$rewindTo":"u"}));
    export(&o).unwrap();
    assert_eq!(records(&o).last().unwrap()["to"], Value::Null);
    assert_eq!(export(&o).unwrap().unchanged, 1);
}
#[test]
fn gemini_masking_keeps_result_revision_and_sibling_parts() {
    let mut a = gm("a", "gemini", "");
    a["toolCalls"] = json!([{"id":"call","name":"read_file","args":{"path":"example"},"status":"success","result":[{"functionResponse":{"id":"call","name":"read_file","response":{"output":"original"}}}]}]);
    let (_d, o) = fixture(
        Provider::Gemini,
        &[gemini_header(), gm("u", "user", "read"), a.clone()],
    );
    export(&o).unwrap();
    a["toolCalls"][0]["result"] = json!([{"functionResponse":{"id":"call","name":"read_file","response":{"output":"<tool_output_masked>preview</tool_output_masked>"}}},{"inlineData":{"mimeType":"image/png","data":"YQ=="}}]);
    append(
        &o,
        &json!({"$set":{"messages":[gm("u","user","read"),a],"lastUpdated":"2026-01-01T00:00:02Z"}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let results: Vec<_> = r.iter().filter(|v| v["type"] == "tool_result").collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[1]["supersedes"], results[0]["id"]);
    assert_eq!(results[1]["capture"]["content"]["state"], "masked");
    assert_eq!(results[1]["content"][1]["type"], "image");
    assert_eq!(results[1]["linkage"], "exact");
    assert_eq!(r.iter().filter(|v| v["type"] == "tool_call").count(), 1);
}
#[test]
fn gemini_does_not_guess_masking_from_changed_or_ordinary_text() {
    let a = json!({"id":"a","type":"gemini","content":"<tool_output_masked>quoted text</tool_output_masked>","toolCalls":[{"id":"c","name":"read_file","status":"success","result":[{"functionResponse":{"id":"c","response":{"output":"first"}}}]}]});
    let (_d, o) = fixture(Provider::Gemini, &[gemini_header(), a.clone()]);
    export(&o).unwrap();
    let mut b = a;
    b["toolCalls"][0]["result"][0]["functionResponse"]["response"]["output"] =
        json!("changed without marker");
    append(&o, &b);
    export(&o).unwrap();
    assert!(records(&o)
        .iter()
        .all(|v| v["capture"]["content"]["state"] != "masked"));
}
#[test]
fn gemini_checkpoint_messages_keep_order_for_inclusive_rewind() {
    let (_d, o) = fixture(
        Provider::Gemini,
        &[
            gemini_header(),
            json!({"$set":{"messages":[gm("u","user","first"),gm("a","gemini","second"),gm("b","user","third")]}}),
            json!({"$rewindTo":"b"}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let retained = r.iter().find(|v| v["messageId"] == "a").unwrap();
    assert_eq!(r.last().unwrap()["to"], retained["id"]);
}
#[test]
fn gemini_function_response_survives_a_call_without_nested_result() {
    let (_d, o) = fixture(
        Provider::Gemini,
        &[
            gemini_header(),
            json!({"id":"a","type":"gemini","content":[{"functionCall":{"id":"c","name":"read_file","args":{}}}]}),
            json!({"id":"b","type":"user","content":[{"functionResponse":{"id":"c","response":{"output":"kept"}}}]}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let result = r.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["content"][0]["data"]["output"], "kept");
    assert_eq!(result["linkage"], "exact");
}
#[test]
fn gemini_legacy_snapshot_migration_relocation_and_bounds() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("session-owner.json");
    let mut v = gemini_header();
    v["messages"] = json!([gm("u", "user", "hello"), gm("a", "gemini", "answer")]);
    std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let mut o = Options::new(
        vec![Input {
            provider: Provider::Gemini,
            path: path.clone(),
        }],
        d.path().join("out"),
    );
    export(&o).unwrap();
    let before = records(&o);
    assert!(before
        .iter()
        .skip(1)
        .all(|r| r["assembly"]["mode"] == "snapshot"));
    assert_eq!(
        before.iter().find(|r| r["messageId"] == "u").unwrap()["assembly"]["mode"],
        "snapshot"
    );
    let new = d.path().join("moved");
    std::fs::create_dir(&new).unwrap();
    let moved = new.join(path.file_name().unwrap());
    std::fs::rename(&path, &moved).unwrap();
    o.inputs[0].path = moved;
    export(&o).unwrap();
    assert_eq!(records(&o), before);
    v["messages"][0]["content"] = json!("x".repeat(2048));
    std::fs::write(&o.inputs[0].path, serde_json::to_vec(&v).unwrap()).unwrap();
    o.max_record_bytes = 1024;
    export(&o).unwrap();
    assert!(records(&o).iter().any(|r| r["type"] == "gap"));
    std::fs::write(
        o.inputs[0].path.with_extension("jsonl"),
        format!("{}\n{}\n", gemini_header(), gm("new", "user", "migrated")),
    )
    .unwrap();
    o.inputs[0].path = new;
    export(&o).unwrap();
    let r = records(&o);
    assert!(r.iter().any(|r| r["messageId"] == "new"));
    assert!(!r.iter().any(|r| r["messageId"] == "u"));
}
#[test]
fn gemini_kind_and_native_directory_are_required_for_spawn_lineage() {
    let d = tempfile::tempdir().unwrap();
    let parent = d.path().join("chats/parent");
    std::fs::create_dir_all(&parent).unwrap();
    let path = parent.join("child.jsonl");
    let mut h = gemini_header();
    h["sessionId"] = json!("child");
    h["kind"] = json!("subagent");
    std::fs::write(&path, format!("{h}\n")).unwrap();
    let o = Options::new(
        vec![Input {
            provider: Provider::Gemini,
            path,
        }],
        d.path().join("out"),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["lineage"]["spawnedBy"]["session"], "gemini-cli:parent");
    assert_eq!(r[0]["scope"], "subagent");
    h["kind"] = json!("main");
    std::fs::write(&o.inputs[0].path, format!("{h}\n")).unwrap();
    export(&o).unwrap();
    assert!(records(&o)[0].get("lineage").is_none());
}
fn qw(id: &str, parent: Value, kind: &str) -> Value {
    json!({"sessionId":"qwen-owner","uuid":id,"parentUuid":parent,"type":kind,"message":{"role":if kind=="assistant"{"model"}else{"user"},"parts":[{"text":id}]}})
}
#[test]
fn qwen_usage_sidecars_do_not_supply_journal_ownership() {
    let (d, mut o) = fixture(Provider::QwenCode, &[qw("u", Value::Null, "user")]);
    let source = o.inputs[0].path.parent().unwrap().to_path_buf();
    std::fs::write(
        source.join("usage_record.jsonl"),
        "{\"sessionId\":\"wrong-owner\",\"version\":\"0.26.1\",\"models\":{}}\n",
    )
    .unwrap();
    o.inputs[0].path = source;
    // Keep the destination outside the scanned source directory.
    let out = tempfile::tempdir().unwrap();
    o.out = out.path().to_path_buf();
    let summary = export(&o).unwrap();
    assert_eq!(summary.sessions, 1);
    assert_eq!(records(&o)[0]["sessionId"], "qwen-owner");
    drop(d);
}
#[test]
fn qwen_uuid_tree_rewind_and_parent_session_are_distinct() {
    let mut parent = qw("meta", json!("a"), "system");
    parent["subtype"] = json!("parent_session");
    parent["systemPayload"] = json!({"parentSessionId":"spawner"});
    let (_d, o) = fixture(
        Provider::QwenCode,
        &[
            qw("u", Value::Null, "user"),
            qw("a", json!("u"), "assistant"),
            parent,
            qw("b", json!("meta"), "user"),
            qw("alternate", json!("a"), "user"),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["lineage"]["spawnedBy"]["session"], "qwen-code:spawner");
    assert!(r[0].get("scope").is_none());
    assert_eq!(
        r.iter().find(|v| v["messageId"] == "alternate").unwrap()["branchesFrom"]["record"],
        r.iter().find(|v| v["messageId"] == "a").unwrap()["id"]
    );
    let rewind = json!({"sessionId":"qwen-owner","uuid":"rewind","parentUuid":"u","type":"system","subtype":"rewind","systemPayload":{"truncatedCount":2}});
    append(&o, &rewind);
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.last().unwrap()["to"],
        r.iter().find(|v| v["messageId"] == "u").unwrap()["id"]
    );
}
#[test]
fn qwen_sidechain_owner_and_copy_facts_do_not_leak_into_gemini() {
    let mut q = qw("copy", Value::Null, "user");
    q["agentId"] = json!("worker");
    q["isSidechain"] = json!(true);
    q["forkedFrom"] = json!({"sessionId":"ancestor","messageUuid":"copy"});
    let (_d, o) = fixture(Provider::QwenCode, &[q]);
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["sessionId"], "worker");
    assert_eq!(
        r[0]["lineage"]["spawnedBy"]["session"],
        "qwen-code:qwen-owner"
    );
    assert_eq!(
        r[0]["lineage"]["forkedFrom"]["session"],
        "qwen-code:ancestor"
    );
    assert_eq!(
        r.iter().find(|v| v["messageId"] == "copy").unwrap()["copiedFrom"]["record"],
        "messageId=copy"
    );
    let mut h = gemini_header();
    h["parentSessionId"] = json!("not-evidence");
    h["forkedFrom"] = json!({"sessionId":"not-evidence"});
    let (_d, o) = fixture(Provider::Gemini, &[h]);
    export(&o).unwrap();
    assert!(records(&o)[0].get("lineage").is_none());
}
#[test]
fn qwen_function_parts_usage_and_duplicate_call_ids() {
    let mut a = qw("a", Value::Null, "assistant");
    a["message"]["parts"] = json!([{"text":"x","thought":true,"thoughtSignature":"signature"},{"functionCall":{"id":"call","name":"read_file","args":{"path":"example"}}}]);
    a["usageMetadata"] = json!({"promptTokenCount":12,"candidatesTokenCount":3,"thoughtsTokenCount":1,"totalTokenCount":15});
    let mut b = a.clone();
    b["uuid"] = json!("b");
    b["parentUuid"] = json!("a");
    b.as_object_mut().unwrap().remove("usageMetadata");
    let mut result = qw("t", json!("b"), "tool_result");
    result["message"]["parts"] =
        json!([{"functionResponse":{"id":"call","name":"read_file","response":{"output":"done"}}}]);
    result["toolCallResult"] = json!({"callId":"call","status":"success","resultDisplay":"shown"});
    let (_d, o) = fixture(Provider::QwenCode, &[a, b, result]);
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|v| v["type"] == "tool_call").count(), 2);
    assert_eq!(
        r.iter().find(|v| v["type"] == "tool_result").unwrap()["linkage"],
        "ambiguous"
    );
    assert_eq!(r.iter().filter(|v| v["type"] == "inference").count(), 1);
}
#[test]
fn usage_does_not_assume_absent_reasoning_counts_are_zero() {
    let mut a = gm("a", "gemini", "answer");
    a["tokens"] = json!({"input":10,"output":3,"total":15});
    let (_d, o) = fixture(Provider::Gemini, &[gemini_header(), a]);
    export(&o).unwrap();
    let r = records(&o);
    let usage = &r.iter().find(|r| r["type"] == "inference").unwrap()["usage"];
    assert!(usage["output"].get("total").is_none());
    assert_eq!(usage["total"], 15);
}
#[test]
fn qwen_noop_compression_does_not_change_context() {
    let (_d, o) = fixture(
        Provider::QwenCode,
        &[
            qw("u", Value::Null, "user"),
            json!({"uuid":"noop","parentUuid":"u","sessionId":"qwen-owner","type":"system","subtype":"chat_compression","systemPayload":{"info":{"compressionStatus":5}}}),
            qw("a", json!("noop"), "assistant"),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert!(r.iter().all(|r| r["type"] != "compaction"));
    assert!(r
        .iter()
        .find(|r| r["messageId"] == "a")
        .unwrap()
        .get("branchesFrom")
        .is_none());
}
#[test]
fn antigravity_directory_owns_identity_and_full_transcript_wins() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("brain/conversation/.system_generated/logs");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("transcript.jsonl"),
        "{\"step_index\":0,\"type\":\"USER_INPUT\",\"content\":\"short\"}\n",
    )
    .unwrap();
    std::fs::write(path.join("transcript_full.jsonl"),format!("{}\n",json!({"step_index":0,"type":"PLANNER_RESPONSE","source":"planner","content":"full","sessionId":"gemini-lookalike","parentSessionId":"not-lineage","tool_calls":[{"name":"read_file","args":{}}]}))).unwrap();
    let o = Options::new(
        vec![Input {
            provider: Provider::Antigravity,
            path,
        }],
        d.path().join("out"),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["sessionId"], "conversation");
    assert!(r
        .iter()
        .skip(1)
        .all(|r| r["assembly"]["mode"] == "snapshot"));
    assert!(r[0].get("lineage").is_none());
    assert_eq!(r.iter().filter(|v| v["type"] == "message").count(), 1);
    assert_eq!(
        r.iter().find(|v| v["type"] == "message").unwrap()["displayContent"][0]["text"],
        "full"
    );
    assert_eq!(
        r.iter().find(|v| v["type"] == "tool_call").unwrap()["callIdSource"],
        "derived"
    );
    assert_eq!(r.iter().filter(|v| v["type"] == "tool_result").count(), 0);
}

#[test]
fn gemini_resume_segment_checkpoint_cannot_prune_another_recording() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("chats");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("session-first.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            gemini_header(),
            gm("u", "user", "first"),
            gm("a", "gemini", "continued")
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("session-second.jsonl"),
        format!(
            "{}\n{}\n",
            gemini_header(),
            json!({"$set":{"messages":[gm("u","user","first")]}})
        ),
    )
    .unwrap();
    let o = Options::new(
        vec![Input {
            provider: Provider::Gemini,
            path: root,
        }],
        d.path().join("out"),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "message").count(), 2);
    assert!(!r
        .iter()
        .any(|r| r["type"] == "compaction" || r["type"] == "rewind"));
}
#[test]
fn qwen_unknown_first_parent_and_injected_offpath_are_branches() {
    let mut injected = qw("notice", json!("old"), "user");
    injected["provenance"] = json!("system");
    let (_d, o) = fixture(
        Provider::QwenCode,
        &[
            qw("root", json!("missing"), "user"),
            qw("old", json!("root"), "assistant"),
            qw("other", Value::Null, "user"),
            injected,
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "root").unwrap()["branchesFrom"]["state"],
        "unknown"
    );
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "notice").unwrap()["branchesFrom"]["record"],
        r.iter().find(|r| r["messageId"] == "old").unwrap()["id"]
    );
}
#[test]
fn gemini_lookalike_copy_fields_are_only_metadata() {
    let mut m = gm("u", "user", "same text");
    m["forkedFrom"] = json!({"sessionId":"unrelated","messageUuid":"x"});
    let (_d, o) = fixture(Provider::Gemini, &[gemini_header(), m]);
    export(&o).unwrap();
    assert!(records(&o).iter().all(|r| r.get("copiedFrom").is_none()));
}

#[test]
fn gemini_metadata_patches_retain_prior_title_and_project() {
    let (_d, o) = fixture(
        Provider::Gemini,
        &[
            gemini_header(),
            json!({"$set":{"summary":"title"}}),
            json!({"$set":{"lastUpdated":"2026-01-01T00:00:02Z"}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let info = r.iter().rfind(|r| r["type"] == "session_info").unwrap();
    assert_eq!(info["title"], "title");
    assert_eq!(info["project"]["id"], "project");
}
#[test]
fn qwen_secret_answers_are_removed_before_unknown_vendor_projection() {
    let mut call = qw("a", Value::Null, "assistant");
    call["message"]["parts"] = json!([{"functionCall":{"id":"question","name":"ask_user_question","args":{"questions":[{"id":"credential","question":"Secret?","secret":true}]}}}]);
    let mut result = qw("t", json!("a"), "tool_result");
    result["message"]["parts"] = json!([{"functionResponse":{"id":"question","response":{"answers":{"credential":"authored-private-value"}}}}]);
    let unknown = json!({"sessionId":"qwen-owner","uuid":"audit","type":"system","subtype":"future","copy":"authored-private-value"});
    let (_d, o) = fixture(Provider::QwenCode, &[call, result, unknown]);
    export(&o).unwrap();
    assert!(!serde_json::to_string(&records(&o))
        .unwrap()
        .contains("authored-private-value"));
}
#[test]
fn gemini_torn_tail_then_completed_update_keeps_prior_ids() {
    let (_d, o) = fixture(
        Provider::Gemini,
        &[gemini_header(), gm("u", "user", "first")],
    );
    export(&o).unwrap();
    let before = records(&o);
    let update = gm("a", "gemini", "tail").to_string();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap();
    write!(f, "{update}").unwrap();
    drop(f);
    export(&o).unwrap();
    assert_eq!(records(&o), before);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    export(&o).unwrap();
    assert_eq!(&records(&o)[..before.len()], &before);
}

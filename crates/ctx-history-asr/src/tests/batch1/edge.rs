use super::*;

#[test]
fn pi_mutable_title_is_a_tail_snapshot_and_append_creates_a_new_capture() {
    let (_dir, o) = fixture(
        Provider::Pi,
        &[
            json!({"type":"title","v":1,"title":"current title"}),
            pi_header(),
            pi("u", Value::Null, json!({"role":"user","content":"first"})),
        ],
    );
    export(&o).unwrap();
    let before = records(&o);
    assert_eq!(before.last().unwrap()["type"], "session_info");
    assert_eq!(before.last().unwrap()["assembly"]["mode"], "snapshot");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap();
    writeln!(
        file,
        "{}",
        pi("next", json!("u"), json!({"role":"user","content":"next"}))
    )
    .unwrap();
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    assert_eq!(after[1]["id"], before[1]["id"]);
    assert_eq!(after[2]["messageId"], "next");
    assert_eq!(after.last().unwrap()["title"], "current title");
    assert_eq!(export(&o).unwrap().unchanged, 1);
}

#[test]
fn grok_optional_response_boundaries_have_disjoint_usage_and_signature() {
    let (_dir, o) = fixture(
        Provider::GrokBuild,
        &[
            grok(
                "s",
                json!({"sessionUpdate":"response_started","message_id":"response","model":"served","input_tokens":8,"cache_read_input_tokens":2,"cache_creation_input_tokens":1}),
            ),
            grok(
                "t",
                json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thought"}}),
            ),
            grok(
                "sig",
                json!({"sessionUpdate":"reasoning_completed","signature":"opaque-signature"}),
            ),
            grok(
                "f",
                json!({"sessionUpdate":"response_completed","message_id":"response","usage":{"input_tokens":8,"output_tokens":4,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"reasoning_tokens":3},"stop_reason":"end_turn","stop_sequence":"done"}),
            ),
            grok(
                "end",
                json!({"sessionUpdate":"turn_completed","prompt_id":"turn","stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":4,"total_tokens":15,"cached_read_tokens":2,"cache_creation_tokens":1,"cost_usd_ticks":2000000000,"model_calls":1}}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let inf: Vec<_> = r.iter().filter(|r| r["type"] == "inference").collect();
    assert_eq!(inf.len(), 2);
    assert_eq!(inf[1]["supersedes"], inf[0]["id"]);
    assert_eq!(inf[1]["usage"]["input"]["uncached"], 8);
    assert_eq!(inf[1]["model"]["id"], "served");
    let m = r.iter().find(|r| r["type"] == "message").unwrap();
    assert_eq!(m["inferenceId"], "response");
    assert_eq!(m["content"][0]["signature"], "opaque-signature");
    let total = r.iter().find(|r| r["type"] == "usage_summary").unwrap();
    assert_eq!(total["usage"]["input"]["total"], 11);
    assert_eq!(total["usage"]["input"]["includesCache"], true);
    assert_eq!(total["cost"]["amount"], 0.2);
}

#[test]
fn pi_shell_context_exclusion_and_parent_copy_evidence() {
    let (dir, o) = fixture(Provider::Pi, &[pi_header()]);
    let parent = dir.path().join("parent.jsonl");
    let copied = pi("u", Value::Null, json!({"role":"user","content":"copied"}));
    std::fs::write(
        &parent,
        format!(
            "{}\n{copied}\n",
            json!({"type":"session","version":3,"id":"parent-id"})
        ),
    )
    .unwrap();
    std::fs::write(&o.inputs[0].path,format!("{}\n{copied}\n{}\n",json!({"type":"session","version":3,"id":"child","parentSession":parent}),pi("bash",json!("u"),json!({"role":"bashExecution","command":"pwd","output":"/workspace","exitCode":0,"excludeFromContext":true})))).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "u").unwrap()["copiedFrom"]["proof"],
        "native_id"
    );
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "bash").unwrap()["audience"],
        "user"
    );
    assert_eq!(r.iter().filter(|r| r["type"] == "tool_call").count(), 0);
    assert_eq!(
        r.iter().find(|r| r["type"] == "terminal").unwrap()["exitCode"],
        0
    );
}

#[test]
fn deepseek_unstarted_tool_requests_and_execution_do_not_duplicate() {
    let (_dir, o) = fixture(
        Provider::DeepSeekHarness,
        &[
            ds_header(),
            ds(
                0,
                "assistant/message",
                json!({"turn":1,"step":1,"message":{"id":"m","role":"assistant","source":{"kind":"model","provider":"test","model":"test"},"content":[{"type":"tool-call","id":"started","name":"read","arguments":"{\"path\":\"a\"}"},{"type":"tool-call","id":"unstarted","name":"read","arguments":"{}"}]}}),
            ),
            ds(
                1,
                "tool/call",
                json!({"turn":1,"step":1,"callId":"started","name":"read","arguments":"{\"path\":\"a\"}"}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let calls: Vec<_> = r.iter().filter(|r| r["type"] == "tool_call").collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["input"]["path"], "a");
    assert_eq!(calls[1]["callId"], "unstarted");
    assert_eq!(calls[0]["inferenceId"], calls[1]["inferenceId"]);
}

#[test]
fn deepseek_failed_requests_and_committed_response_meter_once() {
    let chunk = |seq, c| ds(seq, "assistant/chunk", json!({"turn":1,"step":1,"chunk":c}));
    let (_dir, o) = fixture(
        Provider::DeepSeekHarness,
        &[
            ds_header(),
            ds(0, "step/start", json!({"turn":1,"step":1})),
            chunk(
                1,
                json!({"type":"usage","usage":{"inputTokens":9,"outputTokens":1}}),
            ),
            chunk(
                2,
                json!({"type":"finish","reason":{"kind":"error","error":{"message":"failed"}}}),
            ),
            chunk(
                3,
                json!({"type":"usage","usage":{"inputTokens":11,"outputTokens":2}}),
            ),
            chunk(4, json!({"type":"finish","reason":{"kind":"stop"}})),
            ds(
                5,
                "assistant/message",
                json!({"turn":1,"step":1,"message":{"id":"a","role":"assistant","source":{"kind":"model","provider":"test","model":"served"},"content":[{"type":"text","text":"done"}]}}),
            ),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let inf: Vec<_> = r.iter().filter(|r| r["type"] == "inference").collect();
    assert_eq!(inf.len(), 3);
    assert_eq!(inf[0]["usage"]["input"]["uncached"], 9);
    assert_eq!(inf[2]["usage"]["input"]["uncached"], 11);
    assert_eq!(inf[2]["inferenceId"], inf[1]["inferenceId"]);
    assert_eq!(inf[2]["supersedes"], inf[1]["id"]);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "a").unwrap()["inferenceId"],
        inf[2]["inferenceId"]
    );
}

#[test]
fn grok_foreign_session_is_not_folded_into_this_conversation() {
    let mut foreign = grok(
        "2",
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"foreign"}}),
    );
    foreign["params"]["sessionId"] = json!("other");
    let (_dir, o) = fixture(
        Provider::GrokBuild,
        &[
            grok(
                "1",
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"own"}}),
            ),
            foreign,
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "message").count(), 1);
    assert_eq!(r[1]["content"][0]["text"], "own");
    assert_eq!(r.iter().filter(|r| r["type"] == "gap").count(), 1);
}

#[test]
fn grok_foreign_response_cannot_supply_identity_or_reasoning_signature() {
    let mut foreign = grok(
        "foreign",
        json!({"sessionUpdate":"response_completed","message_id":"foreign-response","signature":"foreign-signature"}),
    );
    foreign["params"]["sessionId"] = json!("other");
    let (_dir, o) = fixture(
        Provider::GrokBuild,
        &[
            grok("start", json!({"sessionUpdate":"response_started"})),
            grok(
                "thought",
                json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"own"}}),
            ),
            foreign,
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let inf = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(inf["idSource"], "derived");
    assert_ne!(inf["inferenceId"], "foreign-response");
    let m = r.iter().find(|r| r["type"] == "message").unwrap();
    assert_eq!(m["inferenceId"], inf["inferenceId"]);
    assert!(m["content"][0].get("signature").is_none());
    assert_eq!(r.iter().filter(|r| r["type"] == "gap").count(), 1);
}

#[test]
fn all_new_families_remove_secret_answers_from_vendor_metadata_and_blobs() {
    for (provider, header) in [
        (Provider::Pi, pi_header()),
        (
            Provider::GrokBuild,
            grok("h", json!({"sessionUpdate":"available_commands_update"})),
        ),
        (Provider::DeepSeekHarness, ds_header()),
    ] {
        let secret = "authored-secret-marker";
        let row = json!({"type":"unknown","questions":[{"id":"key","secret":true}],"answers":{"key":secret},"echo":secret});
        let (_dir, mut o) = fixture(provider, &[header, row]);
        o.max_record_bytes = 1024;
        export(&o).unwrap();
        assert!(!serde_json::to_string(&records(&o))
            .unwrap()
            .contains(secret));
    }
}

#[test]
fn grok_secret_answers_are_removed_before_large_chunk_blobs() {
    let secret = "authored-secret-value";
    let mut rows = vec![grok(
        "q",
        json!({"sessionUpdate":"extension","questions":[{"id":"q","secret":true}],"answers":{"q":secret}}),
    )];
    for n in 0..20 {
        rows.push(grok(&format!("chunk-{n}"),json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("{secret}{}","x".repeat(200))}})));
    }
    let (_dir, mut o) = fixture(Provider::GrokBuild, &rows);
    o.max_record_bytes = 1024;
    export(&o).unwrap();
    let r = records(&o);
    assert!(!serde_json::to_string(&r).unwrap().contains(secret));
    let m = r.iter().find(|r| r["type"] == "message").unwrap();
    assert_eq!(m["capture"]["content"]["state"], "external");
    for blob in std::fs::read_dir(o.out.join("blobs/sha256")).unwrap() {
        assert!(!std::fs::read_to_string(blob.unwrap().path())
            .unwrap()
            .contains(secret));
    }
}

#[test]
fn pi_compaction_keeps_the_first_native_unit_and_unknown_branch_extent() {
    let (_dir, o) = fixture(
        Provider::Pi,
        &[
            pi_header(),
            pi("u", Value::Null, json!({"role":"user","content":"old"})),
            pi(
                "a",
                json!("u"),
                json!({"role":"assistant","content":[{"type":"text","text":"kept"}]}),
            ),
            json!({"type":"compaction","id":"c","parentId":"a","firstKeptEntryId":"a","summary":"earlier","tokensBefore":20}),
            json!({"type":"branch_summary","id":"b","parentId":"c","fromId":"not-retained","summary":"branch"}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let m = r.iter().find(|r| r["messageId"] == "a").unwrap();
    let c = r.iter().find(|r| r["compactionId"] == "c").unwrap();
    assert_eq!(c["keptFromId"], m["id"]);
    assert_eq!(
        r.iter().find(|r| r["compactionId"] == "b").unwrap()["replaces"]["state"],
        "unknown"
    );
}

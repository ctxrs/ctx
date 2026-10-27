//! Authored edge cases, separate from the private authentic-capture qualification.
use super::*;
use std::fs;

#[test]
fn codex_async_question_message_does_not_claim_a_tool_call() {
    let (_d, o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"display-question","delivery":"async","questions":[{"id":"choice","question":"Choose a color","options":[{"label":"blue"}]}]}}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"actual-call","name":"request_user_input","arguments":"{\"questions\":[{\"id\":\"tool-choice\",\"question\":\"Choose a shape\"}]}"}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let question = r
        .iter()
        .find(|r| r["requestId"] == "display-question")
        .unwrap();
    assert_eq!(question["type"], "input_request");
    assert_eq!(question["blocking"], false);
    assert!(question.get("callId").is_none());
    assert_eq!(r.iter().filter(|r| r["type"] == "tool_call").count(), 1);
    let tool_question = r.iter().find(|r| r["requestId"] == "actual-call").unwrap();
    assert_eq!(tool_question["callId"], "actual-call");
    assert_eq!(export(&o).unwrap().unchanged, 1);
}

#[test]
fn codex_process_provenance_and_native_collaboration_invocations() {
    let (_d, o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"exec","command":"true","status":"completed","process_id":"0007"}}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CollabAgentToolCall","id":"nested","tool":"spawn_agent","status":"completed","receiver_thread_ids":["child"]}}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"model","name":"spawn_agent","arguments":"{}"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CollabAgentToolCall","id":"model","tool":"spawn_agent","receiver_thread_ids":["other"]}}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"SubAgentActivity","id":"activity","kind":"interacted","agent_thread_id":"child","agent_path":"/child"}}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"activity","output":""}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let exec = r
        .iter()
        .find(|r| r["type"] == "tool_result" && r["callId"] == "exec")
        .unwrap();
    assert_eq!(exec["exec"]["processId"], "0007");
    assert!(exec["exec"].get("terminalId").is_none());
    assert_eq!(r.iter().filter(|r| r["type"] == "terminal").count(), 0);
    let c = r
        .iter()
        .find(|r| r["type"] == "tool_call" && r["callId"] == "nested")
        .unwrap();
    assert_eq!(c["name"], "spawn_agent");
    assert_eq!(c["capture"]["input"]["state"], "absent");
    let activity = r
        .iter()
        .find(|r| r["type"] == "tool_call" && r["callId"] == "activity")
        .unwrap();
    assert_eq!(activity["capture"]["name"]["state"], "absent");
    assert!(activity.get("requestedBy").is_none());
    assert_eq!(
        r.iter()
            .filter(|r| r["type"] == "tool_call" && r["callId"] == "model")
            .count(),
        1
    );
}

#[test]
fn claude_prefix_boundary_uses_actual_parent_projection_independent_of_sort_order() {
    let (d, mut o) = fixture(
        Provider::Claude,
        &[
            json!({"type":"fork-context-ref","sessionId":"a-child","parentSessionId":"z-parent","parentLastUuid":"result"}),
            json!({"type":"user","sessionId":"a-child","uuid":"child","message":{"role":"user","content":"continue"}}),
        ],
    );
    let parent = d.path().join("parent.jsonl");
    fs::write(&parent,[
        json!({"type":"assistant","sessionId":"z-parent","uuid":"request","message":{"id":"response","role":"assistant","content":[{"type":"tool_use","id":"call","name":"read","input":{}}]}}),
        json!({"type":"user","sessionId":"z-parent","uuid":"result","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"result"}]}}),
    ].iter().map(|v|format!("{v}\n")).collect::<String>()).unwrap();
    o.inputs.push(Input {
        provider: Provider::Claude,
        path: parent,
    });
    export(&o).unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    let read = |id: &str| -> Vec<Value> {
        let e = manifest["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["sessionId"] == id)
            .unwrap();
        fs::read_to_string(o.out.join(e["path"].as_str().unwrap()))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let parent = read("z-parent");
    let child = read("a-child");
    let target = parent.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(child[0]["lineage"]["inherits"]["throughId"], target["id"]);
    assert_eq!(export(&o).unwrap().unchanged, 2);
    records(&o);
}

#[test]
fn cline_sdk_document_identity_usage_and_relocation() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("sdk-session");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("sdk-session.json"),json!({"version":1,"session_id":"sdk-session","started_at":"2026-01-01T00:00:00Z","cwd":"/synthetic"}).to_string()).unwrap();
    fs::write(dir.join("sdk-session.messages.json"),json!({"version":1,"sessionId":"sdk-session","messages":[
        {"id":"a","role":"assistant","content":[{"type":"tool_use","id":"c","name":"read_files","input":{"files":[]}}],"modelInfo":{"id":"example","provider":"example"},"metrics":{"inputTokens":5,"outputTokens":2}},
        {"id":"b","role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"result"}]}
    ]}).to_string()).unwrap();
    let mut o = Options::new(
        vec![Input {
            provider: Provider::Cline,
            path: dir.clone(),
        }],
        d.path().join("out"),
    );
    o.namespace = Some("synthetic-installation".into());
    export(&o).unwrap();
    let before = records(&o);
    assert_eq!(before[0]["sessionId"], "sdk-session");
    assert!(before
        .iter()
        .any(|v| v["type"] == "inference" && v["usage"]["input"]["total"] == 5));
    assert!(before
        .iter()
        .any(|v| v["type"] == "tool_result" && v["callId"] == "c"));
    assert_eq!(export(&o).unwrap().unchanged, 1);
    let moved = d.path().join("moved");
    fs::create_dir(&moved).unwrap();
    let moved = moved.join("sdk-session");
    fs::rename(dir, &moved).unwrap();
    o.inputs[0].path = moved;
    export(&o).unwrap();
    assert_eq!(records(&o), before);
}

#[test]
fn pi_v4_lanes_and_usage_ledger_do_not_double_count_response_usage() {
    let (_d, o) = fixture(
        Provider::Pi,
        &[
            json!({"kind":"header","version":4,"id":"test","createdAt":1767225600000u64,"cwd":"/synthetic"}),
            json!({"kind":"entry","type":"message","seq":1,"id":"u","parentId":null,"lane":"main","timestamp":1767225600001u64,"message":{"role":"user","content":"hello"}}),
            json!({"kind":"lane","seq":2,"lane":"review","leafId":"u"}),
            json!({"kind":"entry","type":"message","seq":3,"id":"review","parentId":"u","lane":"review","timestamp":1767225600002u64,"message":{"role":"user","content":"review"}}),
            json!({"kind":"entry","type":"message","seq":4,"id":"a","parentId":"u","lane":"main","timestamp":1767225600003u64,"message":{"role":"assistant","model":"test-model","provider":"example","content":[{"type":"text","text":"done"}],"usage":{"input":1,"output":2,"totalTokens":3}}}),
            json!({"kind":"record","type":"usage","seq":5,"id":"usage","lane":"main","timestamp":1767225600004u64,"cause":"assistant","runId":"run","entryId":"a","attempt":1,"stopReason":"stop","usage":{"input":1,"output":2,"totalTokens":3}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["startedAt"], "2026-01-01T00:00:00.000Z");
    assert_eq!(r.iter().filter(|r| r["type"] == "inference").count(), 1);
    assert!(r
        .iter()
        .any(|r| r["messageId"] == "a" && r["inferenceId"] == "usage"));
    assert_eq!(r.last().unwrap()["heads"].as_array().unwrap().len(), 2);
    assert!(r.iter().find(|r| r["messageId"] == "u").unwrap()["ts"].is_string());
}

#[test]
fn deepseek_v4_result_shape_and_latest_generation() {
    let (d, mut o) = fixture(Provider::DeepSeekHarness, &[]);
    let rows = [
        json!({"type":"session","version":4,"id":"test","createdAt":1767225600000u64,"isSeeded":false,"delegationDepth":0}),
        json!({"type":"assistant/message","seq":0,"time":1767225600001u64,"data":{"turn":1,"step":1,"message":{"role":"assistant","id":"a","content":[{"type":"tool-call","id":"c","name":"read","arguments":"{}"}]}}}),
        json!({"type":"tool/result","seq":1,"time":1767225600002u64,"data":{"turn":1,"step":1,"message":{"role":"tool","id":"result","toolCallId":"c","source":{"kind":"tool","callId":"c"},"isError":false,"content":[{"type":"text","text":"result"}]}},"surfaceOp":"append"}),
    ];
    fs::write(
        d.path().join("session.v4.jsonl"),
        rows.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    fs::write(
        d.path().join("session.jsonl"),
        format!(
            "{}\n",
            json!({"type":"session","version":0,"id":"test","createdAt":1767225600000u64})
        ),
    )
    .unwrap();
    o.inputs[0].path = d.path().into();
    export(&o).unwrap();
    let r = records(&o);
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["callId"], "c");
    assert!(serde_json::to_string(&result["raw"])
        .unwrap()
        .contains("result"));
}

#[test]
fn vibe_unified_uses_only_current_generation_and_keeps_snapshot_scope() {
    use sha2::{Digest, Sha256};
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("native");
    let gen = root.join("generations/0000000000000001");
    fs::create_dir_all(&gen).unwrap();
    fs::create_dir_all(root.join("journal")).unwrap();
    let write = |name: &str, value: Value| {
        let bytes = value.to_string();
        fs::write(gen.join(name), &bytes).unwrap();
        json!({"path":name,"sha256":format!("{:x}",Sha256::digest(bytes.as_bytes()))})
    };
    let checkpoint = write(
        "checkpoint.json",
        json!({"checkpoint_version":1,"context":{"messages":[
            {"source":"history","turn_index":0,"message":{"role":"assistant","content":[{"type":"tool_call","id":"call","name":"read","arguments":{"type":"json","raw":"{}","value":{}}}]}},
            {"source":"history","turn_index":0,"message":{"role":"tool","tool_call_id":"call","outcome":"success","content":[{"type":"text","text":"result"}]}}
        ]}}),
    );
    let runtime = write(
        "runtime-state.json",
        json!({"session_id":"test","snapshot_sequence":1,"session_metadata":{"cwd":"/synthetic"}}),
    );
    let projection = write(
        "projection-state.json",
        json!({"session_id":"test","snapshot_sequence":1,"snapshot":{"history":{"entries":[]}}}),
    );
    let manifest = write(
        "manifest.json",
        json!({"session_id":"test","generation":"0000000000000001","snapshot_sequence":1,"checkpoint":checkpoint,"runtime_state":runtime,"projection_state":projection,"execution_state":"quiescent","recovery_journal_segment":{"path":"journal/0000000000000002.jsonl","first_sequence":2}}),
    );
    fs::write(root.join("CURRENT"),json!({"store_format":"mistral.vibe.unified-session-store/v1","store_format_minor":10,"session_id":"test","generation":"0000000000000001","snapshot_sequence":1,"manifest_sha256":manifest["sha256"]}).to_string()).unwrap();
    fs::write(root.join("journal/0000000000000002.jsonl"), "").unwrap();
    // An abandoned future generation must never enter the published capture.
    fs::create_dir(root.join("generations/0000000000000002")).unwrap();
    fs::write(
        root.join("generations/0000000000000002/messages.jsonl"),
        "invalid",
    )
    .unwrap();
    let o = Options::new(
        vec![Input {
            provider: Provider::MistralVibe,
            path: root,
        }],
        d.path().join("out"),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "tool_call").count(), 1);
    assert!(r.iter().any(|r| r["type"] == "tool_result"
        && r["callId"] == "call"
        && r["status"] == "completed"));
    assert!(r
        .iter()
        .filter(|r| r["type"] == "message")
        .all(|r| r["idScope"] == "capture" && r["assembly"]["mode"] == "snapshot"));
    assert_eq!(export(&o).unwrap().unchanged, 1);
}

#[test]
#[ignore = "requires disposable authentic captures; never reads shared provider state"]
fn qualify_round4_native_captures() {
    let root =
        PathBuf::from(std::env::var_os("CTX_ASR_ROUND4_CAPTURE_ROOT").expect("capture root"));
    for (provider, suffix) in [
        (Provider::Cline, "cline/.cline/data/sessions"),
        (Provider::MistralVibe, "vibe/.vibe/logs/session/unified"),
        (Provider::DeepSeekHarness, "deepseek/.dsh/sessions"),
        (Provider::Pi, "pi/.pi/agent/sessions"),
    ] {
        let d = tempfile::tempdir().unwrap();
        let mut o = Options::new(
            vec![Input {
                provider,
                path: root.join(suffix),
            }],
            d.path().join("out"),
        );
        o.namespace = Some("round4-scratch".into());
        let summary = export(&o).unwrap();
        assert!(summary.records > 0, "{provider:?}");
        if let Some(validator) = std::env::var_os("ASR_VALIDATOR") {
            let result = std::process::Command::new("node")
                .arg(validator)
                .arg(&o.out)
                .args(["--streaming", "--format", "json"])
                .output()
                .unwrap();
            let validation: Value = serde_json::from_slice(&result.stdout).unwrap();
            eprintln!(
                "oracle {provider:?}: {} {}",
                validation["validator"], validation["summary"]
            );
            assert!(
                result.status.success(),
                "native {provider:?} failed validation; inspect aggregate counts"
            );
        }
        let again = export(&o).unwrap();
        assert_eq!(again.records, summary.records);
        assert_eq!(again.unchanged, summary.sessions);
        eprintln!("authentic {provider:?}: {summary:?}");
    }
}

#[test]
fn codex_self_rooted_child_turn_is_not_assigned_to_spawn_tree_root() {
    let (_d, o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","session_id":"tree-root"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"child-turn","root_turn_id":"child-turn"}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let state = r.iter().find(|r| r["type"] == "state").unwrap();
    assert_eq!(state["rootTurn"]["session"], "codex:test");
    assert_eq!(state["rootTurn"]["turnId"], "child-turn");
}

#[test]
fn codex_causal_root_owner_is_independent_of_source_and_session_order() {
    let (d, mut o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"a-child","session_id":"unrelated-tree"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"child-turn","root_turn_id":"causal-turn"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"other-turn","root_turn_id":"not-captured"}}),
        ],
    );
    let parent = d.path().join("parent.jsonl");
    fs::write(
        &parent,
        [
            json!({"type":"session_meta","payload":{"id":"z-cause"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"causal-turn"}}),
        ]
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>(),
    )
    .unwrap();
    o.inputs.push(Input {
        provider: Provider::Codex,
        path: parent.clone(),
    });
    export(&o).unwrap();
    let r = records(&o);
    let root = r
        .iter()
        .find(|r| r["type"] == "state" && r["turnId"] == "child-turn")
        .unwrap();
    assert_eq!(
        root["rootTurn"],
        json!({"session":"codex:z-cause","turnId":"causal-turn"})
    );
    let unknown = r
        .iter()
        .find(|r| r["type"] == "state" && r["turnId"] == "other-turn")
        .unwrap();
    assert!(unknown.get("rootTurn").is_none());
    assert_eq!(
        unknown["_meta"]["com.openai.codex/remainingFields"]["unboundRootTurn"]["turnId"],
        "not-captured"
    );
    assert_eq!(export(&o).unwrap().unchanged, 2);
    // Removing ownership evidence invalidates the dependent unchanged source.
    fs::remove_file(parent).unwrap();
    o.inputs.pop();
    export(&o).unwrap();
    let r = records(&o);
    assert!(r
        .iter()
        .all(|r| r["type"] != "state" || r.get("rootTurn").is_none()));
}

#[test]
fn deepseek_v4_inherited_marker_bounds_copies_and_preserves_local_suffix() {
    let (_d, o) = fixture(
        Provider::DeepSeekHarness,
        &[
            json!({"type":"session","version":4,"id":"test","createdAt":1767225600000u64,"isSeeded":true,"parentSession":"parent","delegationDepth":1}),
            json!({"type":"user/message","seq":0,"data":{"id":"copied","role":"user","content":"inherited"}}),
            json!({"type":"session/end-seed","seq":1,"data":{"inherited":true}}),
            json!({"type":"user/message","seq":2,"data":{"id":"local","role":"user","content":"new"}}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    let copied = r.iter().find(|r| r["messageId"] == "copied").unwrap();
    let local = r.iter().find(|r| r["messageId"] == "local").unwrap();
    assert!(copied["copiedFrom"].is_object());
    assert!(local.get("copiedFrom").is_none());
    assert_eq!(r[0]["lineage"]["inherits"]["localThroughId"], copied["id"]);
    assert!(!serde_json::to_string(&r[0])
        .unwrap()
        .contains("_seedLength"));
}

#[test]
fn pi_v4_compaction_usage_is_counted_once_and_configuration_leaf_resolves() {
    let (_d, o) = fixture(
        Provider::Pi,
        &[
            json!({"kind":"header","version":4,"id":"test","createdAt":1767225600000u64}),
            json!({"kind":"entry","type":"message","seq":1,"lane":"main","id":"u","parentId":null,"message":{"role":"user","content":"hello"}}),
            json!({"kind":"entry","type":"compaction","seq":2,"lane":"main","id":"compact","parentId":"u","summary":"summary","retainedTail":[],"usage":{"input":2,"output":1}}),
            json!({"kind":"record","type":"usage","seq":3,"lane":"main","id":"meter","entryId":"compact","cause":"compaction","usage":{"input":2,"output":1}}),
            json!({"kind":"entry","type":"model_change","seq":4,"lane":"main","id":"config","parentId":"compact","modelId":"example"}),
        ],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r.get("usage").is_some()).count(), 1);
    let c = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert_eq!(r.last().unwrap()["heads"], json!([c["id"]]));
}

#[test]
fn codex_copied_self_root_retains_its_observed_original_owner() {
    let (d, mut o) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","ordinal":0,"payload":{"id":"a-copy","forked_from_id":"z-origin","forked_from_ordinal_exclusive":2}}),
            json!({"type":"event_msg","ordinal":1,"payload":{"type":"task_started","turn_id":"root-turn","root_turn_id":"root-turn"}}),
            json!({"type":"event_msg","ordinal":2,"payload":{"type":"task_started","turn_id":"new-turn","root_turn_id":"new-turn"}}),
        ],
    );
    let parent = d.path().join("parent.jsonl");
    fs::write(&parent,[json!({"type":"session_meta","payload":{"id":"z-origin"}}),json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"root-turn","root_turn_id":"root-turn"}})].iter().map(|r|format!("{r}\n")).collect::<String>()).unwrap();
    o.inputs.push(Input {
        provider: Provider::Codex,
        path: parent,
    });
    export(&o).unwrap();
    let r = records(&o);
    let copied = r
        .iter()
        .find(|r| r["type"] == "state" && r["turnId"] == "root-turn")
        .unwrap();
    assert_eq!(copied["rootTurn"]["session"], "codex:z-origin");
    let own = r
        .iter()
        .find(|r| r["type"] == "state" && r["turnId"] == "new-turn")
        .unwrap();
    assert_eq!(own["rootTurn"]["session"], "codex:a-copy");
}

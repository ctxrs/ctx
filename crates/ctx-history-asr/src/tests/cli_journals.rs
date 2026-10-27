//! Authored semantic cases, not captures of a provider release.
use super::*;
use std::io::Write;

fn rows(p: Provider) -> Vec<Value> {
    match p {
        Provider::Copilot => vec![
            json!({"type":"session.start","id":"start","data":{"sessionId":"copilot-test","copilotVersion":"1.0.104","selectedModel":"selected","context":{"cwd":"/synthetic"}}}),
            json!({"type":"session.schedule_created","id":"schedule","data":{"id":7,"intervalMs":10000,"prompt":"check","recurring":true,"origin":"user"}}),
            json!({"type":"user.message","id":"scheduled","data":{"turnId":"t","source":"schedule-7","messageId":"prompt","content":"check","transformedContent":"wrapped check","delivery":"idle"}}),
            json!({"type":"assistant.turn_start","id":"turn","data":{"turnId":"t"}}),
            json!({"type":"assistant.message","id":"answer","data":{"turnId":"t","messageId":"m","apiCallId":"api","content":"looking","toolRequests":[{"toolCallId":"call","name":"mcp__ambiguous__name","arguments":{"q":"one"}}]}}),
            json!({"type":"tool.execution_start","id":"call-event","data":{"toolCallId":"call","toolName":"mcp__ambiguous__name","arguments":"{\"q\":\"one\"}","mcpServerName":"ambiguous__server","mcpToolName":"name"}}),
            json!({"type":"tool.execution_complete","id":"result","data":{"toolCallId":"call","success":false,"shellExecution":{"exitCode":0},"result":{"content":"model output","detailedContent":"display output","structuredContent":{"x":1}}}}),
            json!({"type":"user.message","id":"steer","data":{"turnId":"t","source":"schedule-7","content":"steer check","delivery":"steering"}}),
            json!({"type":"assistant.usage","id":"usage","data":{"apiCallId":"api","model":"served","inputTokens":12,"cacheReadTokens":4,"outputTokens":0,"cost":2,"duration":10}}),
            json!({"type":"assistant.turn_end","id":"end","data":{"turnId":"t"}}),
            json!({"type":"hook.start","id":"hook-start","data":{"hookInvocationId":"hook","hookType":"preToolUse"}}),
            json!({"type":"hook.end","id":"hook-end","data":{"hookInvocationId":"hook","hookType":"preToolUse","success":true,"output":{"checked":true}}}),
            json!({"type":"permission.requested","id":"permission-start","data":{"requestId":"permission","permissionRequest":{"toolCallId":"call","kind":"shell"}}}),
            json!({"type":"permission.completed","id":"permission-end","data":{"requestId":"permission","toolCallId":"call","decisionSource":"human_response","result":{"kind":"approved-for-session"}}}),
            json!({"type":"user_input.requested","id":"input-start","data":{"requestId":"input","question":"Continue?","choices":["Yes","No"],"allowFreeform":true}}),
            json!({"type":"user_input.completed","id":"input-end","data":{"requestId":"input","answer":"Yes","wasFreeform":false}}),
            json!({"type":"session.compaction_complete","id":"compaction-failed","data":{"compactionAttemptId":"compact","success":false,"preCompactionTokens":100}}),
        ],
        Provider::Droid => vec![
            json!({"type":"session_start","sessionId":"droid-test","cwd":"/synthetic","callingSessionId":"parent","decompSessionType":"worker"}),
            json!({"type":"message","id":"u","role":"user","content":"hello"}),
            json!({"type":"message","id":"a","message":{"role":"assistant","id":"not-a-call-id","content":[{"type":"tool_use","id":"call","name":"Execute","input":{"command":"true"}}]}}),
            json!({"type":"message","id":"r","role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"first","is_error":true}]}),
            json!({"type":"message","id":"r","parentId":"r","role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"retry","is_error":false}]}),
            json!({"type":"compaction_state","id":"compact","summary":"earlier work"}),
        ],
        Provider::Qoder => vec![
            json!({"type":"session_meta","sessionId":"qoder-test","uuid":"start","cwd":"/synthetic"}),
            json!({"type":"assistant","sessionId":"qoder-test","uuid":"a","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"mcp__server__tool","input":{"q":"x"}}]}}),
            json!({"type":"user","sessionId":"qoder-test","uuid":"r","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"model output","is_error":false}]},"toolUseResult":{"content":"UI output","exitCode":2}}),
            json!({"type":"tool_result","sessionId":"qoder-test","uuid":"orphan","toolCallId":"missing","output":"display only"}),
            json!({"type":"future","sessionId":"qoder-test","uuid":"future","details":{"unknown":true}}),
        ],
        Provider::Kimi => vec![
            json!({"type":"metadata","protocol_version":"1.5","created_at":1700000000000_i64}),
            json!({"type":"turn.prompt","input":[{"type":"text","text":"hello"}],"origin":{"kind":"user"}}),
            json!({"type":"context.append_message","message":{"role":"user","content":[{"type":"text","text":"hello"}],"toolCalls":[],"origin":{"kind":"user"}}}),
            json!({"type":"context.append_loop_event","event":{"type":"step.begin","uuid":"step","turnId":"1","step":1}}),
            json!({"type":"llm.request","kind":"loop","model":"requested","provider":"kimi","turnStep":"1.1","attempt":"1","toolsHash":"tools","messageCount":1}),
            json!({"type":"context.append_loop_event","event":{"type":"content.part","uuid":"part","turnId":"1","step":1,"stepUuid":"step","part":{"type":"think","think":"reason","encrypted":"signature"}}}),
            json!({"type":"context.append_loop_event","event":{"type":"tool.call","uuid":"call-event","turnId":"1","step":1,"stepUuid":"step","toolCallId":"call","name":"ReadFile","args":{"path":"file"}}}),
            json!({"type":"context.append_loop_event","event":{"type":"step.end","uuid":"step","turnId":"1","step":1,"usage":{"inputOther":10,"inputCacheRead":2,"inputCacheCreation":3,"output":4},"rawFinishReason":"tool_calls"}}),
            json!({"type":"usage.record","model":"requested","usage":{"inputOther":10,"inputCacheRead":2,"inputCacheCreation":3,"output":4},"usageScope":"turn"}),
            json!({"type":"context.append_loop_event","event":{"type":"tool.result","parentUuid":"call-event","toolCallId":"call","result":{"output":"file content","note":"model note","message":"UI note"}}}),
            json!({"type":"permission.record_approval_result","turnId":1,"toolCallId":"call","toolName":"ReadFile","action":"read file","result":{"decision":"approved","scope":"session"}}),
        ],
        _ => unreachable!(),
    }
}
pub(super) fn journal_fixture(p: Provider) -> (tempfile::TempDir, Options) {
    let (dir, mut options) = fixture(p, &rows(p));
    if p == Provider::Kimi {
        let path = dir
            .path()
            .join("sessions/work/kimi-test/agents/main/wire.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::rename(&options.inputs[0].path, &path).unwrap();
        options.inputs[0].path = path;
        std::fs::write(
            dir.path().join("sessions/work/kimi-test/state.json"),
            json!({"workDir":"/synthetic"}).to_string(),
        )
        .unwrap();
    }
    (dir, options)
}
#[test]
fn copilot_schedule_identity_and_tool_views() {
    let (_dir, o) = journal_fixture(Provider::Copilot);
    export(&o).unwrap();
    let r = records(&o);
    let m = r.iter().find(|r| r["messageId"] == "prompt").unwrap();
    assert_eq!(
        m["trigger"],
        json!({"kind":"schedule","native":"schedule-7","id":"7","schedule":{"intervalMs":10000,"recurring":true}})
    );
    assert_eq!(m["author"], "harness");
    assert_eq!(m["content"][0]["text"], "wrapped check");
    assert_eq!(r.iter().filter(|r| r.get("trigger").is_some()).count(), 1);
    let calls: Vec<_> = r.iter().filter(|r| r["type"] == "tool_call").collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["name"], "mcp__ambiguous__name");
    assert_eq!(calls[0]["provider"]["server"], "ambiguous__server");
    assert_eq!(calls[0]["provider"]["identity"], "exact");
    let out = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(out["status"], "failed");
    assert_eq!(out["content"][0]["text"], "model output");
    assert_eq!(out["displayContent"][0]["text"], "display output");
    let usage = r.iter().rev().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(usage["model"]["id"], "served");
    assert!(usage.get("cost").is_none());
    assert_eq!(usage["usage"]["input"]["includesCache"], Value::Null);
}
#[test]
fn droid_preserves_retries_and_does_not_invent_model_calls() {
    let (_dir, o) = journal_fixture(Provider::Droid);
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "tool_result").count(), 2);
    assert!(!r.iter().any(|r| r["type"] == "inference"));
    assert_eq!(r[0]["lineage"]["spawnedBy"]["session"], "droid:parent");
    assert!(r
        .iter()
        .find(|r| r["messageId"] == "u")
        .unwrap()
        .get("author")
        .is_none());
}
#[test]
fn qoder_uses_model_block_once_and_keeps_ui_sidecar() {
    let (_dir, o) = journal_fixture(Provider::Qoder);
    export(&o).unwrap();
    let r = records(&o);
    let results: Vec<_> = r.iter().filter(|r| r["type"] == "tool_result").collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["content"][0]["text"], "model output");
    assert_eq!(results[0]["raw"]["content"], "UI output");
    assert_eq!(results[1]["callId"], "missing");
    assert_eq!(results[1]["linkage"], "exact");
    assert_eq!(results[1]["status"], "unknown");
    assert!(results[1].get("content").is_none());
}
#[test]
fn kimi_identity_usage_and_reasoning() {
    let (_dir, o) = journal_fixture(Provider::Kimi);
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["sessionId"], "main");
    assert_eq!(r[0]["namespace"], "kimi-test");
    assert_eq!(
        r.iter()
            .filter(|r| r["type"] == "message" && r["role"] == "user")
            .count(),
        1
    );
    let inf: Vec<_> = r.iter().filter(|r| r["type"] == "inference").collect();
    assert_eq!(inf.len(), 2);
    assert_eq!(inf[1]["supersedes"], inf[0]["id"]);
    assert_eq!(inf[1]["usage"]["input"]["uncached"], 10);
    assert!(r
        .iter()
        .any(|r| r["type"] == "message" && r["content"][0]["type"] == "reasoning"));
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    assert_eq!(manifest["sessions"][0]["namespace"], "kimi-test");
}
#[test]
fn journals_repeat_append_rewrite_relocate_and_torn_tail() {
    for p in [
        Provider::Copilot,
        Provider::Droid,
        Provider::Kimi,
        Provider::Qoder,
    ] {
        let (dir, mut o) = journal_fixture(p);
        export(&o).unwrap();
        let before = records(&o);
        assert_eq!(export(&o).unwrap().unchanged, 1);
        let native = std::fs::read(&o.inputs[0].path).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&o.inputs[0].path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"future","id":"appended","uuid":"appended","sessionId":"qoder-test"})
        )
        .unwrap();
        export(&o).unwrap();
        let after = records(&o);
        assert_eq!(&after[..before.len()], &before);
        write!(file, "{{\"type\":").unwrap();
        file.flush().unwrap();
        export(&o).unwrap();
        assert_eq!(records(&o), after);
        std::fs::write(&o.inputs[0].path, &native).unwrap();
        assert_eq!(export(&o).unwrap().snapshots, 1);
        assert_eq!(records(&o), before);
        if p != Provider::Kimi {
            let moved = dir.path().join("moved.jsonl");
            std::fs::rename(&o.inputs[0].path, &moved).unwrap();
            o.inputs[0].path = moved;
        } else {
            let moved = dir.path().join("relocated");
            std::fs::rename(dir.path().join("sessions"), &moved).unwrap();
            o.inputs[0].path = moved.join("work/kimi-test/agents/main/wire.jsonl");
        }
        export(&o).unwrap();
        assert_eq!(records(&o), before);
    }
}
#[test]
fn journal_duplicate_call_ids_stay_ambiguous() {
    for p in [
        Provider::Copilot,
        Provider::Droid,
        Provider::Kimi,
        Provider::Qoder,
    ] {
        let (_dir, o) = journal_fixture(p);
        let mut data = rows(p);
        let call = match p {
            Provider::Copilot => {
                json!({"type":"tool.execution_start","id":"other-call","data":{"toolCallId":"call","toolName":"view","arguments":{}}})
            }
            Provider::Kimi => {
                json!({"type":"context.append_loop_event","event":{"type":"tool.call","uuid":"other-call","toolCallId":"call","name":"ReadFile","args":{}}})
            }
            _ => {
                let mut call = data
                    .iter()
                    .find(|v| v.pointer("/message/content/0/type") == Some(&json!("tool_use")))
                    .unwrap()
                    .clone();
                call["uuid"] = json!("other-call");
                call["id"] = json!("other-call");
                call
            }
        };
        data.push(call);
        std::fs::write(
            &o.inputs[0].path,
            data.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
        export(&o).unwrap();
        let records = records(&o);
        let key = format!("{}/nativeCallId", p.domain());
        let results: Vec<_> = records
            .iter()
            .filter(|r| r["type"] == "tool_result" && r["_meta"][&key] == "call")
            .collect();
        assert!(!results.is_empty(), "{p:?}");
        assert!(results
            .iter()
            .all(|r| r["linkage"] == "ambiguous" && r.get("callId").is_none()));
    }
}
#[test]
fn copilot_embedded_agents_have_separate_streams() {
    let (_dir, o) = journal_fixture(Provider::Copilot);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap();
    writeln!(f,"{}",json!({"type":"assistant.message","id":"child-event","agentId":"child","data":{"messageId":"child-message","content":"child answer"}})).unwrap();
    assert_eq!(export(&o).unwrap().sessions, 2);
    let m: Value =
        serde_json::from_slice(&std::fs::read(o.out.join("bundle.json")).unwrap()).unwrap();
    for entry in m["sessions"].as_array().unwrap() {
        let r: Vec<Value> = std::fs::read_to_string(o.out.join(entry["path"].as_str().unwrap()))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        if entry["sessionId"] == "child" {
            assert_eq!(entry["namespace"], "copilot-test");
            assert!(r.iter().any(|r| r["messageId"] == "child-message"));
            assert!(!r.iter().any(|r| r["messageId"] == "prompt"));
        } else {
            assert!(!r.iter().any(|r| r["messageId"] == "child-message"));
        }
    }
}
#[test]
#[ignore = "writes authored conformance bundles to CTX_ASR_FIXTURE_OUT"]
fn export_journal_qualification_bundles() {
    let dest = std::path::PathBuf::from(
        std::env::var_os("CTX_ASR_FIXTURE_OUT").expect("explicit output path"),
    );
    for p in [
        Provider::Copilot,
        Provider::Droid,
        Provider::Kimi,
        Provider::Qoder,
    ] {
        let (_dir, mut o) = journal_fixture(p);
        o.out = dest.join(p.agent());
        export(&o).unwrap();
    }
}

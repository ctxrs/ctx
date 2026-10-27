//! Authored semantic cases. These are not captures of native harness output.
use super::*;
use std::fs;
fn journal(provider: Provider, rows: &[Value], metadata: Value) -> (tempfile::TempDir, Options) {
    let dir = tempfile::tempdir().unwrap();
    let relative = match provider {
        Provider::Junie => "sessions/session-test/events.jsonl",
        Provider::MistralVibe => "session/messages.jsonl",
        Provider::Mux => "sessions/test/chat.jsonl",
        Provider::Tabnine => "chats/test.jsonl",
        Provider::Cursor => "projects/work/agent-transcripts/test/test.jsonl",
        _ => unreachable!(),
    };
    let path = dir.path().join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_rows(&path, rows);
    match provider {
        Provider::Junie => write_rows(&dir.path().join("sessions/index.jsonl"), &[metadata]),
        Provider::MistralVibe => {
            fs::write(path.with_file_name("meta.json"), metadata.to_string()).unwrap()
        }
        Provider::Mux => {
            fs::write(path.with_file_name("metadata.json"), metadata.to_string()).unwrap()
        }
        _ => {}
    }
    let options = Options::new(vec![Input { provider, path }], dir.path().join("out"));
    (dir, options)
}
fn write_rows(path: &std::path::Path, rows: &[Value]) {
    fs::write(
        path,
        rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
}
#[test]
fn vibe_preserves_roles_calls_large_integers_and_metadata_tail() {
    let (_d, o) = journal(
        Provider::MistralVibe,
        &[
            json!({"role":"user","message_id":"u","content":"question","injected":true}),
            json!({"role":"assistant","message_id":"a","reasoning_content":"consider","tool_calls":[{"id":"c","function":{"name":"mcp_unseparable_name","arguments":"{\"n\":9007199254740993}"}}]}),
            json!({"role":"tool","tool_call_id":"c","content":"answer","tool_result":{"duration":0.25,"output":{"n":9007199254740993u64}}}),
        ],
        json!({"session_id":"test","title":"current","stats":{"session_prompt_tokens":10,"session_completion_tokens":3},"config":{"active_model":"alias","api_key":"AUTHORED_SECRET","providers":[{"extra_headers":{"X-API-Key":"AUTHORED_HEADER_SECRET"}}]}}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "u").unwrap()["author"],
        "harness"
    );
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["name"], "mcp_unseparable_name");
    assert!(call.get("input").is_none());
    assert_eq!(call["inputText"], "{\"n\":9007199254740993}");
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["durationMs"], 250.0);
    assert_eq!(result["status"], "unknown");
    assert!(result.get("raw").is_none());
    let first_snapshot = r
        .iter()
        .position(|r| r["assembly"]["mode"] == "snapshot")
        .unwrap();
    assert!(r[first_snapshot..]
        .iter()
        .all(|r| r["assembly"]["mode"] == "snapshot"));
    assert!(!serde_json::to_string(&r)
        .unwrap()
        .contains("AUTHORED_SECRET"));
    assert!(!serde_json::to_string(&r)
        .unwrap()
        .contains("AUTHORED_HEADER_SECRET"));
    assert_eq!(export(&o).unwrap().unchanged, 1);
}
#[test]
fn tabnine_folds_whole_messages_but_does_not_pair_reused_call_ids() {
    let (_d, o) = journal(
        Provider::Tabnine,
        &[
            json!({"sessionId":"test","kind":"main"}),
            json!({"id":"a","type":"tabnine","content":"partial","toolCalls":[{"id":"same","name":"run","args":{}}]}),
            json!({"id":"a","type":"tabnine","content":"final","toolCalls":[{"id":"same","name":"run","args":{},"result":"one"}],"tokens":{"input":3,"output":2,"total":5}}),
            json!({"id":"b","type":"tabnine","toolCalls":[{"id":"same","name":"run","args":{},"result":"two"}]}),
        ],
        json!({}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["messageId"] == "a").count(), 1);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "a").unwrap()["content"][0]["text"],
        "final"
    );
    for result in r.iter().filter(|r| r["type"] == "tool_result") {
        assert_eq!(result["linkage"], "ambiguous");
        assert!(result.get("callId").is_none());
    }
}
#[test]
fn mutable_tail_moves_but_native_message_identity_survives_new_capture() {
    let (_d, o) = journal(
        Provider::MistralVibe,
        &[json!({"message_id":"u","role":"user","content":"first"})],
        json!({"session_id":"test","title":"one"}),
    );
    export(&o).unwrap();
    let before = records(&o);
    let old = fs::read(o.out.join("bundle.json")).unwrap();
    write_rows(
        &o.inputs[0].path,
        &[
            json!({"message_id":"x","role":"user","content":"inserted"}),
            json!({"message_id":"u","role":"user","content":"first"}),
        ],
    );
    assert_eq!(export(&o).unwrap().snapshots, 1);
    let after = records(&o);
    let a = before.iter().find(|r| r["messageId"] == "u").unwrap();
    let b = after.iter().find(|r| r["messageId"] == "u").unwrap();
    assert_eq!(a["id"], b["id"]);
    assert_ne!(a["seq"], b["seq"]);
    assert!(after[0]["observedAt"].is_string());
    assert_ne!(old, fs::read(o.out.join("bundle.json")).unwrap());
    assert_eq!(export(&o).unwrap().unchanged, 1);
    assert_eq!(records(&o), after);
}
#[test]
fn cursor_uses_strict_parser_and_does_not_inherit_embedded_timestamps() {
    let (_d, o) = journal(
        Provider::Cursor,
        &[
            json!({"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC-7)</timestamp>hello"}]}}),
            json!({"role":"assistant","message":{"content":[{"type":"text","text":"answer"},{"type":"tool_use","name":"run","input":{}}]}}),
            json!({"role":"system","message":{"content":[{"type":"tool_use","id":"false-call","name":"run","input":{}}]}}),
        ],
        json!({}),
    );
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["type"] == "tool_call").count(), 1);
    assert_eq!(r.iter().filter(|r| r.get("ts").is_some()).count(), 1);
    assert_eq!(
        r.iter().find(|r| r["role"] == "user").unwrap()["tsPrecision"],
        "min"
    );
    assert!(r.iter().skip(1).all(|r| r["idScope"] == "capture"));
    assert!(r.iter().all(|r| r["type"] != "inference"));
}
#[test]
fn junie_ui_text_is_display_only_and_file_changes_preserve_native_contents() {
    let (_d, o) = journal(
        Provider::Junie,
        &[
            json!({"kind":"UserPromptEvent","prompt":"edit"}),
            json!({"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"FileChangesBlockUpdatedEvent","stepId":"edit","changes":[{"beforeRelativePath":"a.rs","afterRelativePath":"a.rs","beforeContent":{"kind":"text","text":"old"},"afterContent":{"kind":"text","text":"new"}}]}}}),
            json!({"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"final","result":"done"}}}),
        ],
        json!({"sessionId":"session-test","taskName":"test"}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let change = r.iter().find(|r| r["type"] == "file_change").unwrap();
    assert_eq!(change["changes"][0]["after"]["text"], "new");
    let m = r
        .iter()
        .find(|r| r.get("displayContent").is_some())
        .unwrap();
    assert_eq!(m["capture"]["content"]["state"], "absent");
    assert!(m.get("role").is_none());
}
#[test]
fn mux_partial_replaces_only_with_recorded_sequence_and_more_parts() {
    let (_d, o) = journal(
        Provider::Mux,
        &[
            json!({"id":"a","role":"assistant","parts":[{"type":"text","text":"one"}],"metadata":{"historySequence":1}}),
        ],
        json!({"name":"test"}),
    );
    fs::write(o.inputs[0].path.with_file_name("partial.json"),json!({"id":"a","role":"assistant","parts":[{"type":"text","text":"one"},{"type":"text","text":"two"}],"metadata":{"historySequence":1,"partial":true}}).to_string()).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r.iter().filter(|r| r["messageId"] == "a").count(), 1);
    let m = r.iter().find(|r| r["messageId"] == "a").unwrap();
    assert_eq!(m["assembly"]["mode"], "snapshot");
    assert_eq!(m["content"].as_array().unwrap().len(), 2);
    assert_eq!(m["status"], "partial");
    // A distinct source position that reuses the ID is ambiguous, not a replacement.
    fs::write(o.inputs[0].path.with_file_name("partial.json"),json!({"id":"a","role":"assistant","parts":[{"type":"text","text":"other"}],"metadata":{"historySequence":2}}).to_string()).unwrap();
    export(&o).unwrap();
    let r = records(&o);
    let messages: Vec<_> = r.iter().filter(|r| r["type"] == "message").collect();
    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0]["id"], messages[1]["id"]);
    assert!(messages.iter().all(|r| r["idScope"] == "capture"));
}

#[test]
fn junie_folds_growing_ui_steps_and_keeps_turn_local_step_identity() {
    let (_d, o) = journal(
        Provider::Junie,
        &[
            json!({"kind":"UserPromptEvent","prompt":"first"}),
            json!({"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"step","command":"echo first","details":"a"}}}),
            json!({"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"step","details":"ab","status":"COMPLETED"}}}),
            json!({"kind":"UserPromptEvent","prompt":"second"}),
            json!({"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"step","command":"echo second","details":"second"}}}),
        ],
        json!({"sessionId":"session-test"}),
    );
    export(&o).unwrap();
    let r = records(&o);
    let calls: Vec<_> = r.iter().filter(|v| v["type"] == "tool_call").collect();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0]["callId"], calls[1]["callId"]);
    assert_eq!(calls[0]["input"]["command"], "echo first");
    let result = r.iter().find(|v| v["type"] == "tool_result").unwrap();
    assert_eq!(result["displayContent"][0]["text"], "ab");
    assert_eq!(result["assembly"]["mode"], "folded");
    assert!(r.iter().all(|v| v.get("supersedes").is_none()));
}
#[test]
fn mux_sealed_archive_precedes_active_history_and_nested_calls_keep_parent() {
    let (_d, o) = journal(
        Provider::Mux,
        &[
            json!({"id":"new","role":"assistant","parts":[{"type":"dynamic-tool","toolCallId":"outer","toolName":"code_execution","input":{},"state":"output-available","output":{},"nestedCalls":[{"toolCallId":"inner","toolName":"read","input":{},"state":"output-available","output":"ok"}]}]}),
        ],
        json!({}),
    );
    write_rows(
        &o.inputs[0].path.with_file_name("chat-archive.jsonl"),
        &[json!({"id":"old","role":"user","parts":[{"type":"text","text":"archived"}]})],
    );
    export(&o).unwrap();
    let r = records(&o);
    assert!(
        r.iter().position(|v| v["messageId"] == "old").unwrap()
            < r.iter().position(|v| v["messageId"] == "new").unwrap()
    );
    let inner = r
        .iter()
        .find(|v| v["type"] == "tool_call" && v["callId"] == "inner")
        .unwrap();
    assert_eq!(inner["parentCallId"], "outer");
    assert_eq!(inner["requestedBy"], "code");
}
#[test]
fn large_content_is_external_and_numbers_beyond_u64_are_never_rounded() {
    let (_d, o) = journal(
        Provider::MistralVibe,
        &[json!({"role":"user","message_id":"large","content":"x".repeat(70_000)})],
        json!({"session_id":"test"}),
    );
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&o.inputs[0].path)
        .unwrap()
        .write_all(b"{\"role\":\"user\",\"content\":123456789012345678901234567890}\n{\"role\":\"user\",\"content\":-9223372036854775809}\n")
        .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    let m = r.iter().find(|v| v["messageId"] == "large").unwrap();
    assert_eq!(m["capture"]["content"]["state"], "external");
    assert!(serde_json::to_string(&r)
        .unwrap()
        .contains("123456789012345678901234567890"));
    assert!(r.iter().any(|r| r["type"] == "ai.mistral.vibe/record"));
    assert!(serde_json::to_string(&r)
        .unwrap()
        .contains("-9223372036854775809"));
}

#[test]
fn mux_stale_partial_does_not_make_effective_call_ambiguous() {
    let call = json!({"type":"dynamic-tool","toolCallId":"c","toolName":"read","input":{},"state":"output-available","output":"ok"});
    let row = json!({"id":"m","role":"assistant","parts":[call],"metadata":{"historySequence":2}});
    let (_d, o) = journal(Provider::Mux, std::slice::from_ref(&row), json!({}));
    fs::write(
        o.inputs[0].path.with_file_name("partial.json"),
        row.to_string(),
    )
    .unwrap();
    export(&o).unwrap();
    let r = records(&o);
    let result = r.iter().find(|r| r["type"] == "tool_result").unwrap();
    assert_eq!(result["linkage"], "exact");
    let ids: std::collections::HashSet<_> = r.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), r.len());
}
#[test]
fn secret_answers_in_folded_junie_events_and_mux_partial_do_not_escape() {
    for provider in [Provider::Junie, Provider::Mux] {
        let (_d, o) = journal(
            provider,
            &[
                json!({"kind":"unknown","questions":[{"id":"q","secret":true,"question":"key"}],"answers":{"q":"AUTHORED_SECRET"}}),
            ],
            if provider == Provider::Junie {
                json!({"sessionId":"session-test"})
            } else {
                json!({})
            },
        );
        if provider == Provider::Mux {
            fs::write(o.inputs[0].path.with_file_name("partial.json"),json!({"id":"p","role":"assistant","parts":[{"type":"text","text":"AUTHORED_SECRET"}],"metadata":{"historySequence":1}}).to_string()).unwrap();
        } else {
            use std::io::Write;
            writeln!(fs::OpenOptions::new().append(true).open(&o.inputs[0].path).unwrap(),"{}",json!({"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"s","result":"AUTHORED_SECRET"}}})).unwrap();
        }
        export(&o).unwrap();
        assert!(!serde_json::to_string(&records(&o))
            .unwrap()
            .contains("AUTHORED_SECRET"));
    }
}
#[test]
fn malformed_and_incomplete_journal_lines_do_not_invent_messages() {
    let (_d, o) = journal(Provider::MistralVibe, &[], json!({"session_id":"test"}));
    fs::write(
        &o.inputs[0].path,
        b"not-json\n{\"role\":\"user\",\"role\":\"assistant\"}\n{\"role\":",
    )
    .unwrap();
    let summary = export(&o).unwrap();
    assert_eq!(summary.gaps, 2);
    assert!(records(&o).iter().all(|r| r["type"] != "message"));
}

#[test]
fn mux_discovers_partial_and_archive_without_active_history() {
    for name in ["partial.json", "chat-archive.jsonl"] {
        let (_d, mut o) = journal(Provider::Mux, &[], json!({}));
        let row = json!({"id":"m","role":"assistant","parts":[{"type":"text","text":"saved"}],"metadata":{"historySequence":0}});
        fs::remove_file(&o.inputs[0].path).unwrap();
        o.inputs[0].path = o.inputs[0].path.with_file_name(name);
        write_rows(&o.inputs[0].path, &[row]);
        assert_eq!(export(&o).unwrap().sessions, 1);
        let r = records(&o);
        let m = r.iter().find(|v| v["messageId"] == "m").unwrap();
        assert_eq!(m["content"][0]["text"], "saved");
        if name == "partial.json" {
            assert_eq!(m["assembly"]["mode"], "snapshot");
        }
    }
}
#[test]
fn vibe_reused_manual_operation_ids_are_ambiguous() {
    let (_d, o) = journal(
        Provider::MistralVibe,
        &[
            json!({"role":"user","message_id":"u1","content":"one","manual_shell":{"operation_id":"op","command":"pwd"}}),
            json!({"role":"user","message_id":"u2","content":"two","manual_shell":{"operation_id":"op","command":"ls"}}),
        ],
        json!({"session_id":"test"}),
    );
    export(&o).unwrap();
    for result in records(&o).iter().filter(|r| r["type"] == "tool_result") {
        assert!(result.get("callId").is_none());
        assert_eq!(result["linkage"], "ambiguous");
    }
}

#[test]
fn mux_archive_holder_does_not_fabricate_immediate_spawner() {
    let (d, mut o) = journal(Provider::Mux, &[], json!({}));
    let child = d
        .path()
        .join("sessions/test/subagent-transcripts/child/chat.jsonl");
    fs::create_dir_all(child.parent().unwrap()).unwrap();
    write_rows(&child, &[json!({"id":"m","role":"user","parts":[]})]);
    o.inputs[0].path = child;
    export(&o).unwrap();
    let r = records(&o);
    assert_eq!(r[0]["scope"], "subagent");
    assert!(r[0]["lineage"].get("spawnedBy").is_none());
    assert!(r[0]["lineage"]["relatedUnknown"]["session"]
        .as_str()
        .unwrap()
        .ends_with(":test"));
}

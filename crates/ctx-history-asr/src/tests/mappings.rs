use super::*;
#[test]
fn registered_metadata_uses_its_semantic_record() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","message":{"content":"hi"}}),
            json!({"type":"agent-name","sessionId":"test","agentName":"example"}),
            json!({"type":"agent-name","sessionId":"test","agentName":"example"}),
            json!({"type":"agent-color","sessionId":"test","agentColor":"blue"}),
            json!({"type":"assistant","uuid":"a","sessionId":"test","wireToolInputs":{"call":{"command":"true"}},"wireIngestContext":{"call":{"cwd":"/synthetic"}},"apiErrorParams":{"rate_limit_info":{"status":"allowed"}},"message":{"id":"response","content":[{"type":"tool_use","id":"call","name":"Bash","input":{"command":"true"}}]}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let info: Vec<_> = r.iter().filter(|r| r["type"] == "session_info").collect();
    assert_eq!(info.len(), 2);
    assert_eq!(
        info[0]["_meta"]["com.anthropic.claude-code/agentName"],
        "example"
    );
    assert_eq!(
        info[1]["_meta"]["com.anthropic.claude-code/agentColor"],
        "blue"
    );
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(
        call["_meta"]["com.anthropic.claude-code/wireToolInputs"],
        json!({"wireToolInputs":{"command":"true"},"wireIngestContext":{"cwd":"/synthetic"}})
    );
    let inference = r.iter().find(|r| r["type"] == "inference").unwrap();
    assert_eq!(
        inference["_meta"]["com.anthropic.claude-code/quotaLimits"],
        json!({"status":"allowed"})
    );
}

#[test]
fn codex_registry_metadata_preserves_citations_and_guardian_fields() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
            json!({"type":"response_item","payload":{"type":"agent_message","id":"message","content":[],"memory_citation":{"entries":[],"rollout_ids":[]}}}),
            json!({"type":"compacted","payload":{"window_id":"window","message":"summary","guardian_review":{"status":"complete"}}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let message = r.iter().find(|r| r["messageId"] == "message").unwrap();
    assert_eq!(
        message["_meta"]["com.openai.codex/memoryCitation"],
        json!({"entries":[],"rollout_ids":[]})
    );
    let compact = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert_eq!(
        compact["_meta"]["com.openai.codex/compaction"],
        json!({"guardian_review":{"status":"complete"}})
    );
}

#[test]
fn secret_answers_never_survive_vendor_metadata_or_request_blobs() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"question","name":"request_user_input","arguments":"{\"questions\":[{\"id\":\"key\",\"question\":\"Secret?\",\"isSecret\":true}]}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"question","output":"{\"answers\":{\"key\":{\"answers\":[\"synthetic-secret-value\"]}}}"}}),
            json!({"type":"unknown","payload":{"nested":{"retained_context":"copy: synthetic-secret-value"}}}),
        ],
    );
    export(&options).unwrap();
    let result = records(&options);
    assert!(!serde_json::to_string(&result)
        .unwrap()
        .contains("synthetic-secret-value"));
    let request = result
        .iter()
        .find(|r| r["type"] == "input_request")
        .unwrap();
    assert_eq!(request["questions"][0]["secret"], true);
}
#[test]
fn ui_execution_revision_keeps_model_output() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli","cwd":"/synthetic"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"call","name":"exec_command","arguments":"{}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call","output":"model saw this"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call","command":"true","cwd":"/synthetic//src/.","status":"completed","exit_code":0}}}),
        ],
    );
    export(&options).unwrap();
    let result = records(&options);
    let revisions: Vec<_> = result
        .iter()
        .filter(|r| r["type"] == "tool_result")
        .collect();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[1]["supersedes"], revisions[0]["id"]);
    assert_eq!(revisions[1]["content"][0]["text"], "model saw this");
    assert_eq!(revisions[1]["exec"]["exitCode"], 0);
    assert_eq!(revisions[1]["exec"]["cwd"]["path"], "src");
}
#[test]
fn model_output_replaces_absent_capture_from_earlier_ui_result() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"call","name":"exec_command","arguments":"{}"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call","command":"true","status":"completed","exit_code":0}}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call","output":"model saw this"}}),
        ],
    );
    export(&options).unwrap();
    let result = records(&options);
    let revisions: Vec<_> = result
        .iter()
        .filter(|r| r["type"] == "tool_result")
        .collect();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0]["capture"]["content"]["state"], "absent");
    assert_eq!(revisions[1]["content"][0]["text"], "model saw this");
    assert!(revisions[1].pointer("/capture/content").is_none());
    assert_eq!(revisions[1]["exec"]["exitCode"], 0);
    assert_eq!(revisions[1]["supersedes"], revisions[0]["id"]);
}
#[test]
fn codex_ui_images_use_asr_image_or_resource_link() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"user","content":[{"type":"image","image_url":"data:image/png;base64,AA=="},{"type":"image","image_url":"https://example.invalid/image.png"}]}}}),
        ],
    );
    export(&options).unwrap();
    let result = records(&options);
    let message = result.iter().find(|r| r["messageId"] == "user").unwrap();
    assert_eq!(
        message["displayContent"][0],
        json!({"type":"image","mimeType":"image/png","data":"AA=="})
    );
    assert_eq!(message["displayContent"][1]["type"], "resource_link");
    assert_eq!(message["capture"]["content"]["state"], "absent");
}
#[test]
fn claude_permission_hook_schedule_and_question() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","promptId":"t","turnOrigin":"scheduled","scheduledTaskId":"schedule","scheduledFireId":"fire","message":{"content":"run"}}),
            json!({"type":"assistant","uuid":"a","parentUuid":"u","sessionId":"test","message":{"id":"response","content":[{"type":"tool_use","id":"question","name":"AskUserQuestion","input":{"questions":[{"question":"Choose","options":[{"label":"Yes"}]}]}}]}}),
            json!({"type":"user","uuid":"b","parentUuid":"a","sessionId":"test","message":{"content":[{"type":"tool_result","tool_use_id":"question","content":"Yes"}]},"toolUseResult":{"answers":{"Choose":"Yes"}},"permissionDecision":{"decision":"accept","source":"user_permanent"}}),
            json!({"type":"attachment","uuid":"h","sessionId":"test","attachment":{"type":"hook_blocking_error","hookEvent":"Stop","stdout":"","stderr":"stop","exitCode":1}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    assert_eq!(
        r.iter().find(|r| r["messageId"] == "u").unwrap()["trigger"]["runId"],
        "fire"
    );
    assert_eq!(
        r.iter().find(|r| r["type"] == "tool_result").unwrap()["permission"]["decision"],
        "allow_always"
    );
    assert_eq!(
        r.iter().rfind(|r| r["type"] == "input_request").unwrap()["answers"][0]["selected"][0],
        "Yes"
    );
    assert_eq!(
        r.iter().find(|r| r["type"] == "hook").unwrap()["status"],
        "blocked"
    );
}

#[test]
fn codex_nested_mcp_and_compaction_retains() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli","cwd":"/synthetic"}}),
            json!({"type":"response_item","payload":{"type":"message","id":"original","role":"user","content":[{"type":"input_text","text":"keep"}]}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"McpToolCall","id":"nested","server":"server","tool":"lookup","connectorId":null,"arguments":{},"status":"completed","result":{"content":[]},"cell_id":"cell"}}}),
            json!({"type":"compacted","payload":{"window_id":"window","message":"summary","replacement_history":[{"type":"message","retained_source":{"id":{"message_id":"original"},"complete":false}}]}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let call = r.iter().find(|r| r["type"] == "tool_call").unwrap();
    assert_eq!(call["requestedBy"], "code");
    assert_eq!(call["parentCallId"], "cell");
    assert_eq!(call["provider"]["kind"], "mcp");
    assert_eq!(call["provider"]["identity"], "exact");
    let original = r.iter().find(|r| r["messageId"] == "original").unwrap();
    let compact = r.iter().find(|r| r["type"] == "compaction").unwrap();
    assert_eq!(compact["retains"][0]["ref"], original["id"]);
    assert_eq!(compact["retains"][0]["capture"], "truncated");
}
#[test]
fn claude_request_prefix_uses_blobs_and_previous_inference() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","message":{"content":"hi"}}),
            json!({"type":"api-request-blob","hash":"blob","message":{"role":"user","content":"hi"}}),
            json!({"type":"api-request","id":"q1","tail":["blob"],"params":{"model":"requested"}}),
            json!({"type":"assistant","uuid":"a","sessionId":"test","requestRef":"q1","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}),
            json!({"type":"api-request","id":"q2","base":"q1","keep":1,"tail":[]}),
            json!({"type":"assistant","uuid":"b","sessionId":"test","requestRef":"q2","message":{"id":"m2","content":[{"type":"text","text":"next"}]}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let first = r
        .iter()
        .find(|r| r["type"] == "inference" && r["inferenceId"] == "m1")
        .unwrap();
    assert!(first["context"]["entries"][0]["blob"]["blob"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let next = r
        .iter()
        .find(|r| r["type"] == "inference" && r["inferenceId"] == "m2")
        .unwrap();
    assert_eq!(next["context"]["prefixFrom"]["inferenceId"], "m1");
    assert_eq!(next["context"]["prefixFrom"]["count"], 1);
}

#[test]
fn repeated_claude_uuid_keeps_block_order_and_latest_usage() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","message":{"content":"hi"}}),
            json!({"type":"assistant","uuid":"a","sessionId":"test","message":{"id":"response","content":[{"type":"text","text":"one"}],"usage":{"output_tokens":1}}}),
            json!({"type":"assistant","uuid":"b","sessionId":"test","message":{"id":"response","content":[{"type":"text","text":"two"}],"usage":{"output_tokens":2}}}),
            json!({"type":"assistant","uuid":"a","sessionId":"test","message":{"id":"response","content":[{"type":"text","text":"one"}],"usage":{"output_tokens":9},"stop_reason":"end_turn"}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let m = r.iter().find(|r| r["messageId"] == "response").unwrap();
    assert_eq!(
        m["content"],
        json!([{"type":"text","text":"one"},{"type":"text","text":"two"}])
    );
    assert_eq!(
        r.iter().find(|r| r["type"] == "inference").unwrap()["usage"]["output"]["total"],
        9
    );
}
#[test]
fn bash_omitted_file_count_becomes_boolean_with_count_preserved() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","cwd":"/synthetic","message":{"content":"hi"}}),
            json!({"type":"assistant","uuid":"a","sessionId":"test","message":{"id":"response","content":[{"type":"tool_use","id":"call","name":"Bash","input":{"command":"touch a"}}]}}),
            json!({"type":"user","uuid":"b","sessionId":"test","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"","is_error":false}]},"toolUseResult":{"bashEditDiff":{"files":[{"filePath":"/synthetic/a","created":true}],"moreFiles":2}}}),
        ],
    );
    export(&options).unwrap();
    let r = records(&options);
    let change = r.iter().find(|r| r["type"] == "file_change").unwrap();
    assert_eq!(change["moreFiles"], true);
    assert_eq!(
        change["_meta"]["com.anthropic.claude-code/moreFilesCount"],
        2
    );
}

#[test]
fn reused_native_call_ids_are_not_paired_by_order() {
    for provider in [Provider::Codex, Provider::Claude] {
        let rows = if provider == Provider::Codex {
            vec![
                json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
                json!({"type":"response_item","payload":{"type":"function_call","call_id":"reused","name":"exec_command","arguments":"{}"}}),
                json!({"type":"response_item","payload":{"type":"function_call","call_id":"reused","name":"exec_command","arguments":"{}"}}),
                json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"reused","output":"result"}}),
            ]
        } else {
            vec![
                json!({"type":"user","uuid":"u","sessionId":"test","message":{"content":"hi"}}),
                json!({"type":"assistant","uuid":"a","sessionId":"test","message":{"id":"r1","content":[{"type":"tool_use","id":"reused","name":"Bash","input":{}}]}}),
                json!({"type":"assistant","uuid":"b","sessionId":"test","message":{"id":"r2","content":[{"type":"tool_use","id":"reused","name":"Bash","input":{}}]}}),
                json!({"type":"user","uuid":"c","sessionId":"test","message":{"content":[{"type":"tool_result","tool_use_id":"reused","content":"result"}]}}),
            ]
        };
        let (_dir, options) = fixture(provider, &rows);
        export(&options).unwrap();
        let records = records(&options);
        assert_eq!(
            records
                .iter()
                .filter(|r| r["type"] == "tool_call" && r["callId"] == "reused")
                .count(),
            2
        );
        let result = records.iter().find(|r| r["type"] == "tool_result").unwrap();
        assert_eq!(result["callId"], "reused");
        assert_eq!(result["linkage"], "ambiguous");
        assert!(result.get("exec").is_none());
    }
}

#[test]
fn escaped_secret_answers_are_removed_from_json_string_copies() {
    let secret = "synthetic\"answer\nline";
    let question =
        json!({"questions":[{"id":"q","question":"Secret?","isSecret":true}]}).to_string();
    let answer = format!("\n {}\n", json!({"answers":{"q":{"answers":[secret]}}}));
    let (_dir, options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","payload":{"id":"test","source":"cli"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"question","name":"request_user_input","arguments":question}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"question","output":answer}}),
            json!({"type":"unknown","payload":{"copy":answer}}),
        ],
    );
    export(&options).unwrap();
    let result = records(&options);
    let vendor = result
        .iter()
        .find(|r| r["type"] == "com.openai.codex/unknown")
        .unwrap();
    let copy: Value = serde_json::from_str(
        vendor["native"]["record"]["payload"]["copy"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_ne!(copy["answers"]["q"]["answers"][0], secret);
    assert_eq!(
        copy["answers"]["q"]["answers"][0],
        "[secret answer redacted]"
    );
}

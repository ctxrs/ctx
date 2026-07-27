use super::*;

#[test]
fn codex_custom_exec_projects_command_run_root_and_typed_result() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("custom-exec.jsonl");
    let workdir = "/workspace/ctx-real";
    let transcript = [
        jsonl_line(json!({
            "timestamp": "2026-07-18T18:24:08Z",
            "type": "session_meta",
            "payload": {
                "id": "custom-exec-session",
                "timestamp": "2026-07-18T18:24:08Z",
                "cwd": "/workspace/launch",
                "originator": "codex-cli"
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-18T18:24:09.265Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "call_id": "call-custom-exec",
                "name": "exec",
                "input": format!(
                    "const r = await tools.exec_command({{cmd:\"git cherry-pick b29a185e\",workdir:\"{workdir}\",yield_time_ms:30000}}); text(r.output);"
                )
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-18T18:24:09.485Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-custom-exec",
                "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.2 seconds\nOutput:\n"},
                    {"type": "input_text", "text": "[ctx/main fe9a28dd] picked\n 1 file changed\n"}
                ]
            }
        })),
    ]
    .concat();
    fs::write(&path, transcript).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store.activate_projection_journal(&"a".repeat(64)).unwrap();

    let imported =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(imported.failed, 0, "{:?}", imported.failures);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "custom-exec-session")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let output = events
        .iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .unwrap();
    assert_eq!(output.payload["body"]["tool"], "exec");
    assert!(output.payload["body"].get("command").is_none());
    assert!(output.payload["body"].get("result_content_ref").is_some());
    assert!(!output.payload.to_string().contains("picked"));
    let runs = store.runs_for_session(session.id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].cwd.as_deref(), Some(workdir));
    assert_eq!(
        runs[0].command_preview.as_deref(),
        Some("git cherry-pick b29a185e")
    );

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert!(snapshot
        .authorized_repository_roots
        .iter()
        .any(|root| root == workdir));
    let canonical_output = snapshot
        .records
        .iter()
        .filter_map(|record| record.canonical_payload.as_ref())
        .find(|payload| payload["event_type"] == "command_output")
        .unwrap();
    assert_eq!(canonical_output["result"]["outcome"], "success");
    assert_eq!(canonical_output["payload"]["tool"], "exec");
    assert!(canonical_output["result"]["content_ref"].is_object());
    assert!(canonical_output["result"]["identifiers"]
        .as_array()
        .is_some_and(|identifiers| identifiers.iter().any(|identifier| {
            identifier["kind"] == "git_commit_summary_id" && identifier["value"] == "fe9a28dd"
        })));
}

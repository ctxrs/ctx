use super::*;
use ctx_history_core::ActivityJsonCapture;

#[test]
fn code_mode_text_survives_publication_replay_and_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("rollout.jsonl");
    let root = temp.path().join("index");
    let header = serde_json::json!({"timestamp":"2026-08-01T00:00:00Z", "type":"session_meta","payload":{
        "id":"019fb000-0000-7000-8000-0000000000b1","cwd":"/fixture", "originator":"codex_exec"
    }});
    // Deliberately ambiguous JS still has complete, searchable output.
    let call = serde_json::json!({"type":"response_item", "payload":{
        "type":"custom_tool_call", "name":"exec", "call_id":"code-mode",
        "input":"if (condition) { text(await tools.exec_command({cmd: 'git status'})); }"
    }});
    let native_output = serde_json::json!([
        {"type":"input_text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
        {"type":"input_text", "text":"complete synthetic code-mode result\nlast line\n"}
    ]);
    let output = serde_json::json!({"type":"response_item", "payload":{
        "type":"custom_tool_call_output", "call_id":"code-mode", "output":native_output
    }});
    let bytes = format!("{header}\n{call}\n{output}\n");
    fs::write(&source_path, &bytes).unwrap();
    let registry = registry(&source_path);
    let first =
        refresh_source_backed_generation(&root, &registry, WriterOptions::default()).unwrap();
    assert!(first.failed_routes.is_empty());
    let source = first.sources[0].observation().source();
    let initial = records(&root, source);
    assert_eq!(initial.len(), 2);
    let expected = "Script completed\nWall time 0.1 seconds\nOutput:\n\ncomplete synthetic code-mode result\nlast line\n";
    assert_eq!(
        initial[1].content.normalized_body.as_deref(),
        Some(expected)
    );
    let result = initial[1]
        .content
        .activity
        .as_ref()
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    assert_eq!(
        result.text,
        ActivityTextCapture::Present {
            value: expected.to_owned()
        }
    );
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: native_output
        }
    );
    let repeat =
        refresh_source_backed_generation(&root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(first.commit.generation_id, repeat.commit.generation_id);
    assert_eq!(records(&root, source), initial);
    assert_eq!(fs::read_to_string(&source_path).unwrap(), bytes);
    let appended = serde_json::json!({"type":"response_item", "payload":{
        "type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"appended response"}]
    }});
    fs::write(&source_path, format!("{bytes}{appended}\n")).unwrap();
    let next =
        refresh_source_backed_generation(&root, &registry, WriterOptions::default()).unwrap();
    assert!(next.failed_routes.is_empty());
    assert_eq!(&records(&root, source)[..2], &initial);
}

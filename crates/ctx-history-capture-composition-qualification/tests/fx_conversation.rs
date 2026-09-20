use super::*;
use ctx_history_core::ActivityTextCapture;

fn conversation_sessions(root: &Path, large: bool) -> PathBuf {
    let sessions = root.join("sessions");
    let case = if large {
        "synthetic-large-results-migration"
    } else {
        "existing-capture-migration"
    };
    copy_tree(
        &fixture_path(&format!("v0.0.10/{case}/sessions")),
        &sessions,
    );
    sessions
}

fn result_artifact(session: &Path) -> PathBuf {
    let rows = fs::read_to_string(session.join("events.jsonl")).unwrap();
    for row in rows.lines() {
        let value: Value = serde_json::from_str(row).unwrap();
        if let Some(name) = value["event"]["tool_result"]["artifact_ref"].as_str() {
            return session.join("tool-results").join(name);
        }
    }
    panic!("native fixture has no tool result")
}

fn result_record(index: &Path) -> CoreRecord {
    records(index)
        .into_iter()
        .find(|record| record.event_type == "tool_output")
        .unwrap()
}

#[test]
fn migrated_native_conversation_keeps_full_artifacts_beyond_control_sidecar_limits() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = conversation_sessions(temp.path(), true);
    let index = temp.path().join("index");
    fs::write(sessions.join(".resume-catalog"), "native root cache").unwrap();
    let registry = registry(&sessions);
    let receipt = refresh(&index, &registry);
    assert_clean_receipt(&receipt, 1);
    let records = records(&index);
    let results: Vec<_> = records
        .iter()
        .filter(|record| record.event_type == "tool_output")
        .collect();
    assert_eq!(results.len(), 12);
    assert!(
        results
            .iter()
            .map(|record| record.content.normalized_body.as_ref().unwrap().len())
            .sum::<usize>()
            > 1024 * 1024
    );
    for (i, record) in results.iter().enumerate() {
        assert!(record
            .content
            .normalized_body
            .as_ref()
            .unwrap()
            .contains(&format!("ARTIFACT_TAIL_{i}")));
        assert_eq!(
            record
                .content
                .activity
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .text,
            ActivityTextCapture::NormalizedBody
        );
    }
    let no_op = refresh(&index, &registry);
    assert_eq!(receipt.commit.generation_id, no_op.commit.generation_id);
}

#[test]
fn artifact_only_changes_and_late_repair_invalidate_watch_catalog_and_replace_content() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = conversation_sessions(temp.path(), false);
    let session = sessions.join(READ_FILE_ID);
    let artifact = result_artifact(&session);
    let index = temp.path().join("index");
    let registry = registry(&sessions);
    let catalog = registry.watch_catalog();
    let route = catalog.route_ids().next().unwrap();
    assert!(catalog.target_paths().any(|path| path == sessions));
    assert!(catalog.routes_overlapping_path(&artifact).contains(route));
    assert!(
        catalog.certify_route_observation(route).is_none(),
        "a directory cannot certify unchanged descendant content"
    );
    let initial = refresh(&index, &registry);
    assert_clean_receipt(&initial, 1);
    let original = result_record(&index);
    let log_before = fs::read(session.join("events.jsonl")).unwrap();
    fs::write(&artifact, "artifactonlyreplacementneedle").unwrap();
    let changed = refresh(&index, &registry);
    assert_clean_receipt(&changed, 1);
    assert_ne!(initial.commit.generation_id, changed.commit.generation_id);
    assert_eq!(result_record(&index).event_id, original.event_id);
    assert_search_hit(&index, "artifactonlyreplacementneedle", READ_FILE_ID);
    fs::remove_file(&artifact).unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    assert_eq!(
        result_record(&index)
            .content
            .activity
            .unwrap()
            .result
            .unwrap()
            .text,
        ActivityTextCapture::Unavailable
    );
    fs::write(&artifact, "laterepairneedle").unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let repaired = result_record(&index);
    assert_eq!(repaired.event_id, original.event_id);
    assert_search_hit(&index, "laterepairneedle", READ_FILE_ID);
    assert_eq!(fs::read(session.join("events.jsonl")).unwrap(), log_before);
}

#[test]
fn conversation_append_and_truncate_reuse_native_sequence_without_stale_content() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = conversation_sessions(temp.path(), false);
    let log = sessions.join(READ_FILE_ID).join("events.jsonl");
    let index = temp.path().join("index");
    let registry = registry(&sessions);
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let initial = records(&index);
    let original = fs::read(&log).unwrap();
    let row = |text| {
        serde_json::to_vec(&json!({"schema_version":2,"seq":6,"timestamp_ms":1700000104000_i64,"event":{"assistant":{"text":text}}})).unwrap()
    };
    let mut next = original.clone();
    next.extend(row("obsoleteappendneedle"));
    next.push(b'\n');
    next.extend(serde_json::to_vec(&json!({"schema_version":2,"seq":7,"timestamp_ms":1700000105000_i64,"event":{"assistant":{"text":"discardedtailneedle"}}})).unwrap());
    next.push(b'\n');
    fs::write(&log, &next).unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let appended = records(&index);
    assert_eq!(&appended[..initial.len()], initial);
    let old = appended[initial.len()].event_id;
    let mut replacement = original;
    replacement.extend(row("replacementsequence"));
    replacement.push(b'\n');
    fs::write(&log, &replacement).unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let current = records(&index);
    assert_eq!(current.len(), initial.len() + 1);
    assert!(current.iter().all(|record| !record
        .content
        .normalized_body
        .as_deref()
        .unwrap_or_default()
        .contains("discardedtailneedle")));
    assert_eq!(current.last().unwrap().event_id, old);
    assert!(current.iter().all(|record| !record
        .content
        .normalized_body
        .as_deref()
        .unwrap_or_default()
        .contains("obsoleteappendneedle")));
    assert_search_hit(&index, "replacementsequence", READ_FILE_ID);
}

#[test]
fn child_owner_uses_parent_native_session_and_malformed_rows_do_not_hide_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = conversation_sessions(temp.path(), false);
    let child = sessions.join("child-native-id");
    fs::create_dir_all(child.join("subagent")).unwrap();
    fs::write(child.join("session.json"), serde_json::to_vec(&json!({"schema_version":4,"id":"child-native-id","workspace_root":"/fixture/child","subagent_child":true})).unwrap()).unwrap();
    fs::write(
        child.join("subagent/owner.json"),
        serde_json::to_vec(&json!({"schema_version":1,"parent_id":READ_FILE_ID})).unwrap(),
    )
    .unwrap();
    let mut bytes = b"{malformed}\n".to_vec();
    bytes.extend(serde_json::to_vec(&json!({"schema_version":1,"seq":1,"timestamp_ms":1,"event":{"assistant":{"text":"childretainedneedle"}}})).unwrap());
    bytes.push(b'\n');
    fs::write(child.join("events.jsonl"), bytes).unwrap();
    let index = temp.path().join("index");
    let receipt = refresh(&index, &registry(&sessions));
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.record_rejections.total(), 1);
    let parent = records_for(&index, READ_FILE_ID);
    let children = records_for(&index, "child-native-id");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].parent_session_id, Some(parent[0].session_id));
    assert_eq!(
        children[0].agent_scope,
        Some(ctx_history_core::AgentScope::Subagent)
    );
    assert_search_hit(&index, "childretainedneedle", "child-native-id");
}

fn command_replay(stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
    let mut bytes = b"FXRPLY01".to_vec();
    for (stream, text) in [(0, stdout), (1, stderr)] {
        if !text.is_empty() {
            bytes.push(stream);
            bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
            bytes.extend_from_slice(text);
        }
    }
    bytes
}

#[test]
fn native_command_directory_modification_and_repair_preserve_exact_tool_result() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = conversation_sessions(temp.path(), false);
    let session = sessions.join(READ_FILE_ID);
    let log = session.join("events.jsonl");
    let stored = fs::read_to_string(result_artifact(&session)).unwrap();
    let mut rows: Vec<Value> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows[2]["event"]["tool_result"]["command_replay_ref"] = json!("command.bin");
    rows.push(json!({"schema_version":2,"seq":6,"timestamp_ms":1700000105000_i64,"event":{"interrupted":{"reason":"canceled","partial_text":"partial","command_artifact_ref":"command.txt"}}}));
    let mut content = Vec::new();
    for row in rows {
        content.extend(serde_json::to_vec(&row).unwrap());
        content.push(b'\n');
    }
    fs::write(&log, &content).unwrap();
    let commands = session.join("logs/commands");
    fs::create_dir_all(&commands).unwrap();
    fs::write(commands.join("command.txt"), "interruptedcommandneedle").unwrap();
    let artifact = commands.join("command.bin");
    fs::write(
        &artifact,
        command_replay(b"originalstdout", b"originalstderr"),
    )
    .unwrap();
    let registry = registry(&sessions);
    let index = temp.path().join("index");
    let catalog = registry.watch_catalog();
    let route = catalog.route_ids().next().unwrap();
    assert!(catalog.routes_overlapping_path(&artifact).contains(route));
    assert!(catalog.certify_route_observation(route).is_none());
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let original = result_record(&index);
    assert_eq!(
        original
            .content
            .activity
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .text,
        ActivityTextCapture::Present {
            value: stored.clone()
        }
    );
    let capture = &original.content.structured_content.as_ref().unwrap()["event"]["tool_result"]
        ["command_replay_capture"];
    let body = original.content.normalized_body.as_ref().unwrap();
    for (stream, expected) in [("stdout", "originalstdout"), ("stderr", "originalstderr")] {
        let start = capture["streams"][stream]["byte_start"].as_u64().unwrap() as usize;
        let len = capture["streams"][stream]["byte_length"].as_u64().unwrap() as usize;
        assert_eq!(&body.as_bytes()[start..start + len], expected.as_bytes());
    }
    assert_search_hit(&index, "interruptedcommandneedle", READ_FILE_ID);
    fs::write(
        &artifact,
        command_replay(b"commandonlychangeneedle", b"stderr"),
    )
    .unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    assert_search_hit(&index, "commandonlychangeneedle", READ_FILE_ID);
    fs::remove_file(&artifact).unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let missing = result_record(&index);
    assert_eq!(
        missing.content.activity.unwrap().result.unwrap().text,
        ActivityTextCapture::NormalizedBody
    );
    assert_eq!(
        missing.content.structured_content.unwrap()["event"]["tool_result"]
            ["command_replay_capture"]["capture_status"],
        "unavailable"
    );
    fs::write(&artifact, command_replay(b"commandrepairneedle", b"stderr")).unwrap();
    assert_clean_receipt(&refresh(&index, &registry), 1);
    let repaired = result_record(&index);
    assert_eq!(repaired.event_id, original.event_id);
    assert_eq!(
        repaired.content.activity.unwrap().result.unwrap().text,
        ActivityTextCapture::Present { value: stored }
    );
    assert_search_hit(&index, "commandrepairneedle", READ_FILE_ID);
    assert_eq!(fs::read(&log).unwrap(), content);
}

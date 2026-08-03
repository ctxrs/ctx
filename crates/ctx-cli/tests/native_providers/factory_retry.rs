use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
};

use super::*;

fn factory_header(session_id: &str) -> Value {
    json!({
        "type": "session_start",
        "id": session_id,
        "timestamp": "2026-07-14T09:30:00Z",
        "cwd": "/workspace",
        "model": "factory/droid"
    })
}

fn factory_result(message_id: &str, parent_id: Option<&str>, results: &[(&str, &str)]) -> Value {
    let content = results
        .iter()
        .map(|(tool_use_id, content)| {
            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "is_error": content.contains("cancelled"),
                "content": content
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "type": "message",
        "id": message_id,
        "timestamp": "2026-07-14T09:30:13Z",
        "message": {"role": "user", "content": content}
    });
    if let Some(parent_id) = parent_id {
        value["parentId"] = json!(parent_id);
        value["message"]["visibility"] = json!("user_only");
    }
    value
}

fn write_jsonl(path: &Path, records: &[Value]) {
    let encoded = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    fs::write(path, encoded).unwrap();
}

fn import_factory_result(temp: &TempDir, path: &str) -> Value {
    let imported = json_output(ctx(temp).args([
        "import",
        "--provider",
        "factory-ai-droid",
        "--path",
        path,
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    wait_for_imported_core(temp, &imported);
    imported
}

fn import_factory(temp: &TempDir, path: &str) -> Value {
    let imported = import_factory_result(temp, path);
    assert_explicit_source_publication(
        &imported,
        "factory_ai_droid",
        "factory_ai_droid_sessions_jsonl",
    );
    imported
}

fn factory_event_ids(temp: &TempDir) -> BTreeMap<String, String> {
    provider_core_records(&data_root(temp), "factory_ai_droid")
        .into_iter()
        .map(|record| {
            (
                record.content.normalized_body.unwrap(),
                record.event_id.to_string(),
            )
        })
        .collect()
}

fn factory_record_ids(temp: &TempDir) -> BTreeSet<(String, String)> {
    provider_core_records(&data_root(temp), "factory_ai_droid")
        .into_iter()
        .map(|record| (record.session_id.to_string(), record.event_id.to_string()))
        .collect()
}

#[test]
fn factory_droid_default_source_imports_searches_and_reimports_without_identity_drift() {
    let temp = tempdir();
    let query = "factory-default-discovery-oracle";
    let generated = PathBuf::from(write_native_factory_droid_fixture(&temp, query));
    let default_root = temp.path().join(".factory/sessions");
    copy_dir_all(&generated, &default_root);
    let _daemon = start_isolated_provider_daemon(&temp);

    let sources = json_output(ctx(&temp).args([
        "sources",
        "--provider",
        "factory-ai-droid",
        "--format=json",
    ]));
    let source = source_by_path(&sources, "factory_ai_droid", &default_root);
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "factory_ai_droid_sessions_jsonl");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "factory-ai-droid",
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&first);
    assert_eq!(first["totals"]["current_rejected_records"], 0);
    wait_for_imported_core(&temp, &first);
    assert_eq!(
        provider_core_counts(&data_root(&temp), "factory_ai_droid"),
        (1, 2)
    );
    let first_ids = factory_record_ids(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "factory-ai-droid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "factory_ai_droid", query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "factory-ai-droid",
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&second);
    assert_noop_publication(&second);
    assert_eq!(
        provider_core_counts(&data_root(&temp), "factory_ai_droid"),
        (1, 2)
    );
    assert_eq!(factory_record_ids(&temp), first_ids);
}

#[test]
fn factory_droid_retry_lifecycle_keeps_native_ids_stable() {
    let temp = tempdir();
    let root = temp.path().join("native-droid-retry/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let session_file = root.join("droid-retry.jsonl");
    let path = root.parent().unwrap().display().to_string();
    let header = factory_header("demo");
    let anchor = factory_result("dup", None, &[("Execute_1", "first")]);
    let retry = factory_result(
        "dup",
        Some("dup"),
        &[("Execute_2", "Error: Tool execution cancelled by user")],
    );
    write_jsonl(
        &session_file,
        &[header.clone(), anchor.clone(), retry.clone()],
    );
    let _daemon = start_isolated_provider_daemon(&temp);

    let cold = import_factory(&temp, &path);
    assert_eq!(
        provider_core_records(&data_root(&temp), "factory_ai_droid").len(),
        3,
        "{cold:#}"
    );
    let cold_ids = factory_event_ids(&temp);
    let anchor_id = cold_ids["first"].clone();
    let retry_id = cold_ids["Error: Tool execution cancelled by user"].clone();
    assert_ne!(anchor_id, retry_id);

    let no_op = import_factory(&temp, &path);
    assert_noop_publication(&no_op);
    assert_eq!(factory_event_ids(&temp), cold_ids);

    let appended = factory_result("dup", Some("dup"), &[("Execute_3", "recovered")]);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&session_file)
        .unwrap();
    writeln!(file, "{appended}").unwrap();
    drop(file);
    import_factory(&temp, &path);
    let after_append = factory_event_ids(&temp);
    assert_eq!(after_append["first"], anchor_id);
    assert_eq!(
        after_append["Error: Tool execution cancelled by user"],
        retry_id
    );
    let appended_id = after_append["recovered"].clone();

    let multi = factory_result(
        "dup",
        Some("dup"),
        &[("Execute_A", "multi a"), ("Execute_B", "multi b")],
    );
    write_jsonl(
        &session_file,
        &[
            header.clone(),
            multi,
            appended.clone(),
            anchor.clone(),
            retry.clone(),
        ],
    );
    import_factory(&temp, &path);
    let after_insert = factory_event_ids(&temp);
    assert_eq!(after_insert["first"], anchor_id);
    assert_eq!(
        after_insert["Error: Tool execution cancelled by user"],
        retry_id
    );
    assert_eq!(after_insert["recovered"], appended_id);
    let multi_a_id = after_insert["multi a"].clone();
    let multi_b_id = after_insert["multi b"].clone();

    let rewritten_retry = factory_result(
        "dup",
        Some("dup"),
        &[("Execute_2", "cancelled retry rewritten")],
    );
    let reordered_multi = factory_result(
        "dup",
        Some("dup"),
        &[("Execute_B", "multi b"), ("Execute_A", "multi a")],
    );
    write_jsonl(
        &session_file,
        &[
            header.clone(),
            appended.clone(),
            rewritten_retry.clone(),
            reordered_multi.clone(),
            anchor.clone(),
        ],
    );
    import_factory(&temp, &path);
    let after_rewrite_and_reorder = factory_event_ids(&temp);
    assert_eq!(after_rewrite_and_reorder["first"], anchor_id);
    assert_eq!(
        after_rewrite_and_reorder["cancelled retry rewritten"],
        retry_id
    );
    assert_eq!(after_rewrite_and_reorder["recovered"], appended_id);
    assert_eq!(after_rewrite_and_reorder["multi a"], multi_a_id);
    assert_eq!(after_rewrite_and_reorder["multi b"], multi_b_id);

    write_jsonl(
        &session_file,
        &[header, appended, rewritten_retry, reordered_multi],
    );
    import_factory(&temp, &path);
    let after_anchor_delete = factory_event_ids(&temp);
    assert_eq!(after_anchor_delete["cancelled retry rewritten"], retry_id);
    assert_eq!(after_anchor_delete["recovered"], appended_id);
    assert_eq!(after_anchor_delete["multi a"], multi_a_id);
    assert_eq!(after_anchor_delete["multi b"], multi_b_id);
    assert!(!after_anchor_delete.values().any(|id| id == &anchor_id));
    let stderr = failure_stderr(ctx(&temp).args(["show", "event", &anchor_id, "--format=json"]));
    assert!(stderr.contains("not found"), "{stderr}");
}

#[test]
fn factory_droid_retry_ambiguity_and_invalid_evidence_fail_closed() {
    for (name, records) in [
        (
            "exact duplicate",
            vec![
                factory_header("duplicate"),
                factory_result("dup", None, &[("Execute_1", "anchor")]),
                factory_result("dup", Some("dup"), &[("Execute_2", "retry")]),
                factory_result("dup", Some("dup"), &[("Execute_2", "duplicate retry")]),
            ],
        ),
        (
            "missing parent evidence",
            vec![
                factory_header("missing-parent"),
                factory_result("dup", None, &[("Execute_1", "anchor")]),
                factory_result("dup", None, &[("Execute_2", "ambiguous retry")]),
            ],
        ),
        (
            "contradictory parent evidence",
            vec![
                factory_header("wrong-parent"),
                factory_result("dup", None, &[("Execute_1", "anchor")]),
                factory_result(
                    "dup",
                    Some("different-message"),
                    &[("Execute_2", "ambiguous retry")],
                ),
            ],
        ),
    ] {
        let temp = tempdir();
        let root = temp.path().join("sessions/project");
        fs::create_dir_all(&root).unwrap();
        write_jsonl(&root.join("failure.jsonl"), &records);
        let path = root.parent().unwrap().display().to_string();
        let _daemon = start_isolated_provider_daemon(&temp);
        let stderr = failure_stderr(ctx(&temp).args([
            "import",
            "--provider",
            "factory-ai-droid",
            "--path",
            &path,
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert!(
            stderr.contains("duplicate event identity"),
            "{name}: {stderr}"
        );
    }

    let temp = tempdir();
    let root = temp.path().join("sessions/project");
    fs::create_dir_all(&root).unwrap();
    let missing_linkage = json!({
        "type": "message",
        "id": "dup",
        "parentId": "dup",
        "timestamp": "2026-07-14T09:30:23Z",
        "message": {
            "role": "user",
            "content": [{"type": "tool_result", "content": "missing tool_use_id"}]
        }
    });
    write_jsonl(
        &root.join("missing-linkage.jsonl"),
        &[
            factory_header("missing-linkage"),
            factory_result("dup", None, &[("Execute_1", "anchor")]),
            missing_linkage,
        ],
    );
    let path = root.parent().unwrap().display().to_string();
    let _daemon = start_isolated_provider_daemon(&temp);
    let imported = import_factory_result(&temp, &path);
    assert_eq!(
        imported["outcome"], "completed_with_rejections",
        "{imported:#}"
    );
    assert_eq!(imported["sources"][0]["status"], "partial", "{imported:#}");
    assert_eq!(imported["totals"]["current_rejected_records"], 1);
    assert_eq!(factory_event_ids(&temp).len(), 2);
}

#[test]
fn factory_retry_identity_is_source_scoped() {
    let temp = tempdir();
    let root = temp.path().join("sessions/project");
    fs::create_dir_all(&root).unwrap();
    for session_id in ["one", "two"] {
        write_jsonl(
            &root.join(format!("{session_id}.jsonl")),
            &[
                factory_header(session_id),
                factory_result("dup", None, &[("Execute_1", "cross-source anchor")]),
                factory_result("dup", Some("dup"), &[("Execute_2", "cross-source retry")]),
            ],
        );
    }
    let path = root.parent().unwrap().display().to_string();
    let _daemon = start_isolated_provider_daemon(&temp);
    import_factory(&temp, &path);
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    assert_eq!(records.len(), 6);
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_id.to_string())
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.source.clone())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
}

#[test]
fn non_factory_native_jsonl_duplicates_still_fail() {
    let temp = tempdir();
    let root = temp.path().join("projects/workspace/transcript");
    fs::create_dir_all(&root).unwrap();
    let qoder = |body: &str| {
        json!({
            "type": "user",
            "sessionId": "qoder-duplicate-session",
            "uuid": "qoder-duplicate-event",
            "timestamp": "2026-07-14T09:30:13Z",
            "message": {"role": "user", "content": body}
        })
    };
    write_jsonl(
        &root.join("duplicate.jsonl"),
        &[
            json!({
                "type": "session_meta",
                "sessionId": "qoder-duplicate-session",
                "uuid": "qoder-meta",
                "timestamp": "2026-07-14T09:30:00Z",
                "data": {"meta_type": "session_info"}
            }),
            qoder("first"),
            qoder("second"),
        ],
    );
    let path = temp.path().join("projects").display().to_string();
    let _daemon = start_isolated_provider_daemon(&temp);
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "qoder",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(stderr.contains("duplicate event identity"), "{stderr}");
}

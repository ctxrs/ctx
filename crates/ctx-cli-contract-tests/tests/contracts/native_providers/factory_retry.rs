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
    import_factory_with_rejections(temp, path, 0)
}

fn import_factory_with_rejections(temp: &TempDir, path: &str, rejected_records: u64) -> Value {
    let imported = import_factory_result(temp, path);
    assert_explicit_source_publication_with_rejections(
        &imported,
        "factory_ai_droid",
        "factory_ai_droid_sessions_jsonl",
        rejected_records,
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
fn factory_droid_import_preserves_result_order_and_record_local_rejections() {
    let temp = tempdir();
    let root = temp.path().join("droid-order/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let file = root.join("ordered.jsonl");
    let path = root.parent().unwrap().to_str().unwrap();
    let message = |id: &str, body: &str| {
        json!({
            "type": "message", "id": id, "role": "assistant", "content": body
        })
    };
    let mut result = factory_result(
        "result",
        None,
        &[("call-one", "result one"), ("call-two", "result two")],
    );
    // A non-result block leaves the first retained result at native index one.
    result["message"]["content"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"type": "text", "text": "context"}));
    let initial = vec![
        factory_header("ordered"),
        message("before", "before"),
        result,
        message("after", "after"),
    ];
    write_jsonl(&file, &initial);
    let _daemon = start_isolated_provider_daemon(&temp);
    let cold = import_factory(&temp, path);
    assert_eq!(cold["totals"]["current_rejected_records"], 0);
    let ids = factory_event_ids(&temp);
    let session = provider_core_records(&data_root(&temp), "factory_ai_droid")[0]
        .session_id
        .as_uuid()
        .to_string();
    let shown = json_output(ctx(&temp).args([
        "show",
        "session",
        &session,
        "--mode",
        "log",
        "--format=json",
    ]));
    let bodies = shown["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["text"].as_str())
        .filter(|text| *text != "notice")
        .collect::<Vec<_>>();
    assert_eq!(bodies, ["before", "result one", "result two", "after"]);
    let window = json_output(ctx(&temp).args([
        "show",
        "event",
        &ids["result one"],
        "--window",
        "1",
        "--format=json",
    ]));
    assert_eq!(
        window["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["before", "result one", "result two"]
    );

    let noop = import_factory(&temp, path);
    assert_noop_publication(&noop);
    assert_eq!(factory_event_ids(&temp), ids);
    let mut appended = fs::OpenOptions::new().append(true).open(&file).unwrap();
    writeln!(
        appended,
        "{}",
        message(&"x".repeat(65_537), "invalid identity")
    )
    .unwrap();
    writeln!(appended, "{}", message("tail", "valid tail")).unwrap();
    drop(appended);
    let append = import_factory_with_rejections(&temp, path, 1);
    assert_eq!(
        append["totals"]["current_rejected_records"], 1,
        "{append:#}"
    );
    let appended_ids = factory_event_ids(&temp);
    assert!(appended_ids.contains_key("valid tail"));
    assert!(!appended_ids.contains_key("invalid identity"));
    for (body, id) in &ids {
        assert_eq!(appended_ids[body], *id);
    }
    let repeat = import_factory_with_rejections(&temp, path, 1);
    assert_eq!(
        repeat["totals"]["current_rejected_records"], 1,
        "{repeat:#}"
    );
    assert_noop_publication(&repeat);
    assert_eq!(factory_event_ids(&temp), appended_ids);

    // Replace the source with the same valid records around a malformed key.
    let mut rewritten = initial;
    rewritten.insert(2, message(&"y".repeat(65_536), "composite key overflow"));
    rewritten.push(message("tail", "valid tail"));
    write_jsonl(&file, &rewritten);
    let replacement = import_factory_with_rejections(&temp, path, 1);
    assert_eq!(
        replacement["totals"]["current_rejected_records"], 1,
        "{replacement:#}"
    );
    assert_eq!(factory_event_ids(&temp), appended_ids);
    let search = json_output(ctx(&temp).args([
        "search",
        "valid tail",
        "--provider",
        "factory-ai-droid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(!search["results"].as_array().unwrap().is_empty());
}

#[test]
fn factory_droid_empty_completion_is_stored_and_showable() {
    let temp = tempdir();
    let root = temp.path().join("droid-empty/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let file = root.join("empty.jsonl");
    let path = root.parent().unwrap().to_str().unwrap();
    let result = factory_result("empty-result", None, &[("call-empty", "")]);
    write_jsonl(&file, &[factory_header("empty"), result.clone()]);
    let _daemon = start_isolated_provider_daemon(&temp);
    let cold = import_factory(&temp, path);
    assert_eq!(cold["totals"]["current_rejected_records"], 0);
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    let stored = records
        .iter()
        .find(|record| record.event_type == "tool_output")
        .unwrap();
    assert_eq!(stored.content.normalized_body, None);
    assert_eq!(stored.content.structured_content.as_ref(), Some(&result));
    let id = stored.event_id.as_uuid().to_string();
    let shown = json_output(ctx(&temp).args(["show", "event", &id, "--format=json"]));
    assert!(shown["event"].get("text").is_none());
    assert_eq!(shown["event"]["structured_content"], result);
    assert_eq!(shown["event"]["content"]["complete"], true);
    assert_eq!(
        shown["event"]["activity"]["provider_call_id"],
        json!({"Utf8": "call-empty"})
    );
    let noop = import_factory(&temp, path);
    assert_noop_publication(&noop);
    assert_eq!(
        factory_record_ids(&temp),
        records
            .iter()
            .map(|record| (record.session_id.to_string(), record.event_id.to_string()))
            .collect()
    );
}

#[test]
fn factory_droid_invalid_optional_parent_preserves_child_history() {
    let temp = tempdir();
    let root = temp.path().join("droid-parent/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let cases = [
        ("self-child", "parent", "self-child".to_owned()),
        ("large-child", "parent", "x".repeat(65_537)),
        (
            "encoded-bound-child",
            "callingSessionId",
            "x".repeat(65_536),
        ),
        ("unresolved-child", "parent", "unresolved-parent".to_owned()),
    ];
    for (child, field, parent) in &cases {
        let mut header = factory_header(child);
        header[*field] = json!(parent);
        write_jsonl(
            &root.join(format!("{child}.jsonl")),
            &[
                header,
                json!({"type": "message", "id": format!("{child}-message"), "role": "assistant", "content": format!("retained {child}")}),
            ],
        );
    }
    let _daemon = start_isolated_provider_daemon(&temp);
    let path = root.parent().unwrap().to_str().unwrap();
    let cold = import_factory(&temp, path);
    assert_eq!(cold["totals"]["current_rejected_records"], 0, "{cold:#}");
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    assert_eq!(records.len(), 8);
    for (child, _, _) in &cases {
        let record = records
            .iter()
            .find(|record| {
                record.content.normalized_body.as_deref() == Some(&format!("retained {child}"))
            })
            .unwrap();
        assert_eq!(
            record.parent_session_id.is_some(),
            *child == "unresolved-child"
        );
        assert_eq!(
            record.session_relationship.is_some(),
            *child == "unresolved-child"
        );
        let shown = json_output(ctx(&temp).args([
            "show",
            "event",
            &record.event_id.as_uuid().to_string(),
            "--format=json",
        ]));
        assert_eq!(shown["event"]["text"], format!("retained {child}"));
    }
    let ids = factory_record_ids(&temp);
    let noop = import_factory(&temp, path);
    assert_noop_publication(&noop);
    assert_eq!(factory_record_ids(&temp), ids);
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
fn factory_droid_duplicate_retry_record_is_rejected_without_aborting_import() {
    let temp = tempdir();
    let root = temp.path().join("sessions/project");
    fs::create_dir_all(&root).unwrap();
    write_jsonl(
        &root.join("duplicate.jsonl"),
        &[
            factory_header("duplicate"),
            factory_result("dup", None, &[("Execute_1", "anchor")]),
            factory_result("dup", Some("dup"), &[("Execute_2", "retry")]),
            factory_result("dup", Some("dup"), &[("Execute_2", "duplicate retry")]),
        ],
    );
    let path = root.parent().unwrap().display().to_string();
    let _daemon = start_isolated_provider_daemon(&temp);
    let imported = import_factory_result(&temp, &path);
    let source = assert_explicit_source_publication_with_rejections(
        &imported,
        "factory_ai_droid",
        "factory_ai_droid_sessions_jsonl",
        1,
    );
    let diagnostics = source["rejection_diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{imported:#}");
    assert_eq!(diagnostics[0]["line"], 4, "{imported:#}");
    assert_eq!(diagnostics[0]["class"], "malformed_record", "{imported:#}");
    assert!(
        diagnostics[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("reused an event identity")),
        "{imported:#}"
    );
    let bodies = provider_core_records(&data_root(&temp), "factory_ai_droid")
        .into_iter()
        .filter_map(|record| record.content.normalized_body)
        .collect::<BTreeSet<_>>();
    assert!(bodies.contains("retry"), "{imported:#}");
    assert!(!bodies.contains("duplicate retry"), "{imported:#}");
}

#[test]
fn factory_droid_missing_retry_linkage_is_rejected() {
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
fn non_factory_native_jsonl_duplicate_is_rejected_without_aborting_import() {
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
    let imported = json_output(ctx(&temp).args([
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
    assert_explicit_source_publication_with_rejections(
        &imported,
        "qoder",
        "qoder_transcript_jsonl_tree",
        1,
    );
    let records = provider_core_records(&data_root(&temp), "qoder");
    assert_eq!(records.len(), 2, "{imported:#}");
    assert!(records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("first") }));
    assert!(!records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("second") }));
}

/// Minimized, anonymized Factory Droid output from ctxrs/ctx#648. The provider
/// wrote one message id twice with the same tool-result linkage. The first
/// physical record wins and the later record is rejected atomically.
#[test]
fn factory_droid_repeated_record_ids_from_authentic_fixture() {
    let temp = tempdir();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures/provider/factory_ai_droid_repeated_record_evidence.jsonl");
    let root = temp
        .path()
        .join("native-droid-repeated-evidence/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::copy(&fixture, root.join("droid-repeated-evidence.jsonl")).unwrap();
    let path = root.parent().unwrap().display().to_string();
    let _daemon = start_isolated_provider_daemon(&temp);

    let imported = import_factory_result(&temp, &path);
    assert_explicit_source_publication_with_rejections(
        &imported,
        "factory_ai_droid",
        "factory_ai_droid_sessions_jsonl",
        1,
    );
    assert_eq!(
        provider_core_records(&data_root(&temp), "factory_ai_droid").len(),
        5,
        "{imported:#}"
    );

    let no_op = import_factory_result(&temp, &path);
    assert_explicit_source_publication_with_rejections(
        &no_op,
        "factory_ai_droid",
        "factory_ai_droid_sessions_jsonl",
        1,
    );
    assert_noop_publication(&no_op);
}

#[test]
fn factory_droid_appended_duplicate_is_rejected_against_the_published_base() {
    let temp = tempdir();
    let root = temp
        .path()
        .join("native-droid-duplicate-append/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let session_file = root.join("droid-duplicate-append.jsonl");
    let path = root.parent().unwrap().display().to_string();
    let first = factory_result("dup", None, &[("Execute_1", "first accepted value")]);
    write_jsonl(&session_file, &[factory_header("duplicate-append"), first]);
    let _daemon = start_isolated_provider_daemon(&temp);

    let cold = import_factory(&temp, &path);
    assert_eq!(cold["totals"]["current_rejected_records"], 0, "{cold:#}");

    let duplicate = factory_result("dup", None, &[("Execute_1", "later duplicate value")]);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&session_file)
        .unwrap();
    writeln!(file, "{duplicate}").unwrap();
    drop(file);

    let appended = import_factory_result(&temp, &path);
    assert_explicit_source_publication_with_rejections(
        &appended,
        "factory_ai_droid",
        "factory_ai_droid_sessions_jsonl",
        1,
    );
    let bodies = provider_core_records(&data_root(&temp), "factory_ai_droid")
        .into_iter()
        .filter_map(|record| record.content.normalized_body)
        .collect::<BTreeSet<_>>();
    assert!(bodies.contains("first accepted value"), "{appended:#}");
    assert!(!bodies.contains("later duplicate value"), "{appended:#}");
}

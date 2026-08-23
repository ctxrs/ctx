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

/// Minimal Factory Droid `message` row with synthetic ids. Used to construct
/// in-memory test sessions; for an authentic captured double-write shape see
/// `factory_droid_repeated_record_ids_from_authentic_fixture`.
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
fn factory_droid_retry_lifecycle_keeps_retry_ids_stable() {
    // Factory self-parented retry identities are keyed by tool_use_id, not by
    // scan order or content. They must therefore stay stable across no-op
    // re-imports, certified-suffix appends, content rewrites, multi-subrecord
    // reordering, insertion of other retries, and deletion of the original
    // no-parent anchor.
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
    let retry_id = cold_ids["Error: Tool execution cancelled by user"].clone();

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
        &[header.clone(), multi, appended.clone(), retry.clone()],
    );
    import_factory(&temp, &path);
    let after_insert = factory_event_ids(&temp);
    assert_eq!(
        after_insert["Error: Tool execution cancelled by user"],
        retry_id
    );
    assert_eq!(after_insert["recovered"], appended_id);
    let multi_a_id = after_insert["multi a"].clone();
    let multi_b_id = after_insert["multi b"].clone();
    assert_ne!(multi_a_id, multi_b_id);

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
        ],
    );
    import_factory(&temp, &path);
    let after_rewrite_and_reorder = factory_event_ids(&temp);
    assert_eq!(
        after_rewrite_and_reorder["cancelled retry rewritten"],
        retry_id
    );
    assert_eq!(after_rewrite_and_reorder["recovered"], appended_id);
    assert_eq!(after_rewrite_and_reorder["multi a"], multi_a_id);
    assert_eq!(after_rewrite_and_reorder["multi b"], multi_b_id);

    write_jsonl(&session_file, &[header, appended, rewritten_retry]);
    import_factory(&temp, &path);
    let after_multi_delete = factory_event_ids(&temp);
    assert_eq!(after_multi_delete["recovered"], appended_id);
    assert_eq!(after_multi_delete["cancelled retry rewritten"], retry_id);
    assert!(!after_multi_delete.contains_key("multi a"));
    assert!(!after_multi_delete.contains_key("multi b"));
}

#[test]
fn factory_droid_retry_ambiguity_and_invalid_evidence_fail_closed() {
    // Two self-parented Factory retries with the same tool_use_id must keep a
    // single identity, because the explicit retry discriminator is evidence
    // they are the same event. Collapsing them is the correct fail-closed
    // behavior; it is distinct from repeated records that lack retry evidence.
    let records = vec![
        factory_header("duplicate"),
        factory_result("dup", None, &[("Execute_1", "anchor")]),
        factory_result("dup", Some("dup"), &[("Execute_2", "retry")]),
        factory_result("dup", Some("dup"), &[("Execute_2", "duplicate retry")]),
    ];
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
    assert!(stderr.contains("duplicate event identity"), "{stderr}");

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

#[test]
fn factory_droid_repeated_record_ids_are_retained() {
    let temp = tempdir();
    let root = temp.path().join("native-droid-repeated/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let session_file = root.join("droid-repeated.jsonl");
    let path = root.parent().unwrap().display().to_string();
    let header = factory_header("demo");
    // One message id emitted twice under two different parents with identical
    // tool_result linkage: a Factory double-write, not a self-parented retry.
    let first = factory_result(
        "dup",
        Some("parent-a"),
        &[("call_A", "shared output a"), ("call_B", "shared output b")],
    );
    let second = factory_result(
        "dup",
        Some("parent-b"),
        &[("call_A", "shared output a"), ("call_B", "shared output b")],
    );
    write_jsonl(
        &session_file,
        &[header.clone(), first.clone(), second.clone()],
    );
    let _daemon = start_isolated_provider_daemon(&temp);

    let cold = import_factory(&temp, &path);
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    assert_eq!(records.len(), 5, "{cold:#}");
    let cold_ids: BTreeSet<String> = records
        .iter()
        .map(|record| record.event_id.to_string())
        .collect();
    assert_eq!(cold_ids.len(), 5, "{cold:#}");

    let no_op = import_factory(&temp, &path);
    assert_noop_publication(&no_op);
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_id.to_string())
            .collect::<BTreeSet<_>>(),
        cold_ids
    );

    // A third emission of the same id arriving in a later certified-suffix
    // append must join the imported prefix instead of colliding with it.
    let third = factory_result(
        "dup",
        Some("parent-c"),
        &[("call_A", "shared output a"), ("call_B", "shared output b")],
    );
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&session_file)
        .unwrap();
    writeln!(file, "{third}").unwrap();
    drop(file);
    import_factory(&temp, &path);
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    assert_eq!(records.len(), 7);
    let after_append: BTreeSet<String> = records
        .iter()
        .map(|record| record.event_id.to_string())
        .collect();
    assert_eq!(after_append.len(), 7);
    assert!(cold_ids.iter().all(|id| after_append.contains(id)));

    // Replacement scans must derive the same identities from parent linkage,
    // independent of file order or rewritten content.
    let rewritten_first = factory_result(
        "dup",
        Some("parent-a"),
        &[
            ("call_A", "rewritten shared output a"),
            ("call_B", "shared output b"),
        ],
    );
    write_jsonl(
        &session_file,
        &[
            header.clone(),
            third.clone(),
            second.clone(),
            rewritten_first.clone(),
        ],
    );
    import_factory(&temp, &path);
    let after_reorder = factory_record_ids(&temp)
        .into_iter()
        .map(|(_, event_id)| event_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(after_reorder, after_append);

    // Deleting one copy removes only that copy; surviving citations retain
    // their event identities.
    write_jsonl(&session_file, &[header, third, rewritten_first.clone()]);
    import_factory(&temp, &path);
    let after_delete = factory_record_ids(&temp)
        .into_iter()
        .map(|(_, event_id)| event_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(after_delete.len(), 5);
    assert!(after_delete.is_subset(&after_append));

    // A later copy under an already-used parent has no stable occurrence
    // discriminator. Reject it and retain the last good generation.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&session_file)
        .unwrap();
    writeln!(file, "{rewritten_first}").unwrap();
    drop(file);
    let ambiguous = json_output(ctx(&temp).args([
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
    assert_eq!(ambiguous["outcome"], "completed_with_source_failures");
    assert_eq!(ambiguous["totals"]["failed_sources"], 1);
    assert_eq!(ambiguous["sources"][0]["status"], "failure");
    assert_eq!(ambiguous["sources"][0]["carried_forward"], true);
    assert_eq!(
        factory_record_ids(&temp)
            .into_iter()
            .map(|(_, event_id)| event_id)
            .collect::<BTreeSet<_>>(),
        after_delete
    );
}

#[test]
fn factory_droid_repeated_record_append_preserves_existing_base_identity() {
    let temp = tempdir();
    let root = temp
        .path()
        .join("native-droid-repeated-append/sessions/project");
    fs::create_dir_all(&root).unwrap();
    let session_file = root.join("droid-repeated-append.jsonl");
    let path = root.parent().unwrap().display().to_string();
    let header = factory_header("demo-append");
    let first = factory_result(
        "dup",
        Some("parent-a"),
        &[("call_A", "shared append output")],
    );
    let second = factory_result(
        "dup",
        Some("parent-b"),
        &[("call_A", "shared append output")],
    );
    // This singleton is the last good generation an older release can have
    // before Factory appends the first duplicate. The provider keeps the prior
    // identity revision so this checkpoint remains a certified append prefix
    // after upgrade.
    write_jsonl(&session_file, &[header.clone(), first.clone()]);
    let _daemon = start_isolated_provider_daemon(&temp);

    import_factory(&temp, &path);
    let base_ids = factory_record_ids(&temp);
    assert_eq!(base_ids.len(), 2);

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&session_file)
        .unwrap();
    writeln!(file, "{second}").unwrap();
    drop(file);
    import_factory(&temp, &path);
    let after_append = factory_record_ids(&temp);
    assert_eq!(after_append.len(), 3);
    assert!(base_ids.is_subset(&after_append));

    write_jsonl(&session_file, &[header, second, first]);
    import_factory(&temp, &path);
    assert_eq!(factory_record_ids(&temp), after_append);
}

#[test]
fn factory_droid_repeated_record_mixed_parent_shapes_fail_closed() {
    let with_parent = factory_result(
        "dup",
        Some("parent-a"),
        &[("call_A", "shared output a"), ("call_B", "shared output b")],
    );
    let no_parent = factory_result(
        "dup",
        None,
        &[("call_A", "shared output a"), ("call_B", "shared output b")],
    );

    let cases = [
        (
            "parent-first",
            "demo-mixed-parent-first",
            vec![with_parent.clone(), no_parent.clone()],
        ),
        (
            "no-parent-first",
            "demo-mixed-no-parent-first",
            vec![no_parent.clone(), with_parent.clone()],
        ),
    ];

    for (label, session_id, messages) in cases {
        let temp = tempdir();
        let root = temp.path().join(format!(
            "native-droid-repeated-mixed-{label}/sessions/project"
        ));
        fs::create_dir_all(&root).unwrap();
        let session_file = root.join("droid-repeated-mixed.jsonl");
        let mut records = vec![factory_header(session_id)];
        records.extend(messages);
        write_jsonl(&session_file, &records);
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
            stderr.contains("every copy must have non-empty parentId evidence"),
            "{label}: {stderr}"
        );
    }
}

/// Authentic provider-evidence test for the Factory Droid double-write shape
/// fixed in ctxrs/ctx#648 / #649.
///
/// The pinned fixture at `tests/fixtures/provider/factory_ai_droid_repeated_record_evidence.jsonl`
/// is a minimized, anonymized export from real Factory Droid session output.
/// It contains one `message` record id emitted twice under two different
/// `parentId` values with identical `tool_use_id` linkage. Prior to #649 the
/// native JSONL provider failed closed on this file; after #649 both copies are
/// retained as distinct events via the `factory-ai-droid.repeated-record`
/// subrecord selector derived from the parent id.
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

    let imported = import_factory(&temp, &path);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    let records = provider_core_records(&data_root(&temp), "factory_ai_droid");
    assert_eq!(
        records.len(),
        7,
        "expected 7 imported core records (session_start + 4 messages, with duplicated message collapsed to one parent-linked subrecord): {imported:#}"
    );

    let no_op = import_factory(&temp, &path);
    assert_noop_publication(&no_op);
}

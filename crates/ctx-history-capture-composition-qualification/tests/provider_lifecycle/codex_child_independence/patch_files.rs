use super::*;

fn patch_call(call: &str, path: &str, quoted: bool) -> serde_json::Value {
    let patch = format!("*** Begin Patch\n*** Delete File: {path}\n*** End Patch");
    let input = if quoted {
        format!(
            "await tools.apply_patch({});",
            serde_json::to_string(&patch).unwrap()
        )
    } else {
        patch
    };
    serde_json::json!({
        "type":"response_item", "timestamp":"2026-08-09T12:00:03Z",
        "payload":{"type":"custom_tool_call", "name":if quoted { "exec" } else { "apply_patch" },
            "call_id":call, "input":input}
    })
}

fn files(records: &[CoreRecord]) -> Vec<String> {
    records
        .iter()
        .flat_map(|record| {
            record
                .content
                .activity
                .iter()
                .flat_map(|activity| activity.facts.iter())
        })
        .filter(|fact| fact.kind == ctx_history_core::LiteralFactKind::File)
        .map(|fact| fact.value.clone())
        .collect()
}

#[test]
fn patch_file_references_survive_noop_append_and_source_rewrite() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir(&sessions).unwrap();
    let session = "019fb000-0000-7000-8000-000000000097";
    let path = session_path(&sessions, session);
    write_session(
        &sessions,
        session,
        ProviderNativeSessionRelationship::Root,
        None,
        [patch_call("first", "old.rs", false)],
    );
    let registry = register_tree(&[&sessions]);
    let first = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(first.failed_routes.is_empty() && first.logical_source_failures.is_empty());
    let original = records_for(&VerifiedIndex::open_pinned(&index_root).unwrap(), session);
    assert_eq!(files(&original), ["old.rs"]);
    let bytes = fs::read(&path).unwrap();
    let noop = incremental_refresh(&index_root, &registry, &first).0;
    assert_eq!(noop.commit.generation_id, first.commit.generation_id);
    assert_eq!(fs::read(&path).unwrap(), bytes);

    append_event(&path, patch_call("second", "nested.rs", true));
    let appended = incremental_refresh(&index_root, &registry, &noop).0;
    assert!(appended.failed_routes.is_empty() && appended.logical_source_failures.is_empty());
    let records = records_for(&VerifiedIndex::open_pinned(&index_root).unwrap(), session);
    assert_eq!(files(&records), ["old.rs", "nested.rs"]);
    assert_eq!(&records[..original.len()], &original);

    write_session(
        &sessions,
        session,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            patch_call("first", "new.rs", false),
            patch_call("second", "nested.rs", true),
        ],
    );
    let rewritten = incremental_refresh(&index_root, &registry, &appended).0;
    assert!(rewritten.failed_routes.is_empty() && rewritten.logical_source_failures.is_empty());
    let records = records_for(&VerifiedIndex::open_pinned(&index_root).unwrap(), session);
    assert_eq!(files(&records), ["new.rs", "nested.rs"]);
    let cold_root = temp.path().join("cold");
    refresh_source_backed_generation(&cold_root, &registry, writer_options()).unwrap();
    assert_eq!(
        records_for(&VerifiedIndex::open_pinned(&cold_root).unwrap(), session),
        records
    );
    let noop = incremental_refresh(&index_root, &registry, &rewritten).0;
    assert_eq!(noop.commit.generation_id, rewritten.commit.generation_id);
}

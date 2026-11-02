use super::*;

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::PrimeAgent,
            path: root.to_path_buf(),
            exists: true,
            source_format: "prime_agent_session_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn header(id: &str) -> Value {
    json!({"type":"session", "version":3, "id":id, "timestamp":"2026-10-01T12:00:00Z",
        "cwd":"/workspace/example", "rlmDepth":0})
}

fn message(id: &str, text: &str) -> Value {
    json!({"type":"message", "id":id, "timestamp":"2026-10-01T12:00:01Z",
        "message":{"role":"user", "content":text}})
}

#[test]
fn prime_agent_cold_append_rewrite_and_torn_tail_keep_stable_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let path = root.join("session.jsonl");
    write_openclaw_lifecycle_transcript(
        &path,
        &[header("session"), message("first", "initial oracle")],
    );
    fs::write(
        root.join("semantic-edges.jsonl"),
        "{\"type\":\"session_registered\"}\n",
    )
    .unwrap();
    fs::write(root.join("session.window-cache.json"), "private cache").unwrap();
    let index = temp.path().join("index");
    let registry = registry(&root);
    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let initial = all_indexed_records(&index);
    assert_eq!(initial.len(), 2);
    let unchanged = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);

    let next = message("second", "append oracle").to_string();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(next.as_bytes()).unwrap();
    let torn = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(torn.failed_routes.is_empty());
    assert_eq!(all_indexed_records(&index).len(), 2);
    file.write_all(b"\n{bad-json}\n").unwrap();
    append_openclaw_lifecycle_transcript(&path, &message("third", "valid neighbor oracle"));
    let append = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(append.failed_routes.is_empty());
    let appended = all_indexed_records(&index);
    assert_eq!(appended.len(), 4);
    assert!(initial
        .iter()
        .all(|old| appended.iter().any(|new| new.event_id == old.event_id)));
    write_openclaw_lifecycle_transcript(
        &path,
        &[header("session"), message("first", "rewritten oracle")],
    );
    let replacement =
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(replacement.failed_routes.is_empty());
    let replaced = all_indexed_records(&index);
    assert_eq!(replaced.len(), 2);
    assert_eq!(replaced[1].event_id, initial[1].event_id);
    assert_eq!(
        replaced[1].content.normalized_body.as_deref(),
        Some("rewritten oracle")
    );
}

#[test]
fn prime_agent_bad_sibling_and_deleted_parent_do_not_hide_child_history() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("agent");
    let parent = root.join("sessions/parent.jsonl");
    write_openclaw_lifecycle_transcript(
        &parent,
        &[header("parent"), message("p", "parent oracle")],
    );
    let mut child_header = header("child");
    child_header["parentSession"] = json!(parent.to_string_lossy());
    child_header["rlmDepth"] = json!(1);
    write_openclaw_lifecycle_transcript(
        &root.join("session-artifacts/parent/sub-test/child.jsonl"),
        &[child_header, message("c", "child oracle")],
    );
    let index = temp.path().join("index");
    let registry = registry(&root);
    let first = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(first.failed_routes.is_empty());
    let records = all_indexed_records(&index);
    let parent_record = &published_session(&records, "parent")[0];
    let child_record = &published_session(&records, "child")[0];
    assert_eq!(
        child_record.parent_session_id,
        Some(parent_record.session_id)
    );
    assert_eq!(
        child_record.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    let child_id = child_record.session_id;
    fs::remove_file(parent).unwrap();
    fs::write(
        root.join("bad.jsonl"),
        "{\"type\":\"session\",\"version\":99}\n",
    )
    .unwrap();
    let refreshed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let records = all_indexed_records(&index);
    assert_eq!(published_session(&records, "child").len(), 2);
    assert_eq!(published_session(&records, "child")[0].session_id, child_id);
    assert!(!refreshed.failed_routes.is_empty() || !refreshed.logical_source_failures.is_empty());
}

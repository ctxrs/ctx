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
fn prime_agent_automatic_import_keeps_cli_artifacts_and_t3_together() {
    use ctx_history_source_discovery::{
        discover_provider_sources_for_provider_with_context, DiscoveryContext, DiscoveryPlatform,
        DiscoveryPlatformDirs,
    };
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let cases = [
        ("cli", ".prime/agent/sessions/cli.jsonl"),
        (
            "child",
            ".prime/agent/session-artifacts/cli/sub-one/child.jsonl",
        ),
        ("t3", ".t3/userdata/prime-agent/prime/thread/t3.jsonl"),
    ];
    for (id, path) in cases {
        write_openclaw_lifecycle_transcript(&home.join(path), &[header(id), message("event", id)]);
    }
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report = discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        &context,
        CaptureProvider::PrimeAgent,
    );
    assert_eq!(report.sources.len(), 3);
    let mut registry = SourceBackedProviderRegistry::new();
    for source in report.sources {
        register_landed_source_backed_route(
            &mut registry,
            source,
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
    }
    let index = temp.path().join("index");
    let first = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(first.failed_routes.is_empty());
    assert_eq!(all_indexed_records(&index).len(), 6);
    for (id, _) in cases {
        assert_eq!(published_session(&all_indexed_records(&index), id).len(), 2);
    }
    let again = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(again.commit.generation_id, first.commit.generation_id);
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
    fs::write(root.join("rlm-ledger.jsonl"), "not a transcript\n").unwrap();
    write_openclaw_lifecycle_transcript(
        &root.join("unrelated/project/nested.jsonl"),
        &[header("unrelated"), message("ignored", "unrelated oracle")],
    );
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
    let root = temp.path().join(".t3/userdata/prime-agent");
    let parent = root.join("prime/thread/parent.jsonl");
    write_openclaw_lifecycle_transcript(
        &parent,
        &[header("parent"), message("p", "parent oracle")],
    );
    let mut child_header = header("child");
    child_header["parentSession"] = json!(parent.to_string_lossy());
    child_header["rlmDepth"] = json!(1);
    write_openclaw_lifecycle_transcript(
        &root.join("prime/session-artifacts/parent/sub-test/child.jsonl"),
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
        root.join("prime/thread/bad.jsonl"),
        "{\"type\":\"session\",\"version\":99}\n",
    )
    .unwrap();
    let refreshed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let records = all_indexed_records(&index);
    assert_eq!(published_session(&records, "child").len(), 2);
    assert_eq!(published_session(&records, "child")[0].session_id, child_id);
    assert!(!refreshed.failed_routes.is_empty() || !refreshed.logical_source_failures.is_empty());
}

#[cfg(unix)]
#[test]
fn prime_agent_unreadable_sibling_does_not_hide_readable_history() {
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_openclaw_lifecycle_transcript(
        &root.join("good.jsonl"),
        &[header("good"), message("first", "readable oracle")],
    );
    let denied = root.join("denied.jsonl");
    write_openclaw_lifecycle_transcript(&denied, &[header("denied")]);
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&denied).is_ok() {
        // A privileged test runner cannot exercise permission denial.
        return;
    }
    let index = temp.path().join("index");
    let outcome =
        refresh_source_backed_generation(&index, &registry(&root), writer_options()).unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        published_session(&all_indexed_records(&index), "good").len(),
        2
    );
    assert!(!outcome.logical_source_failures.is_empty());
}

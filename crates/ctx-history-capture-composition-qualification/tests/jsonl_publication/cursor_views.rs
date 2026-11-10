use super::*;

fn cursor_copy_path(root: &Path, project: &str) -> std::path::PathBuf {
    root.join(project)
        .join("agent-transcripts/fork-session/fork-session.jsonl")
}

fn cursor_histories(index: &Path) -> Vec<Vec<CoreRecord>> {
    let mut histories = std::collections::BTreeMap::new();
    for record in indexed_records(index, CaptureProvider::Cursor, "fork-session") {
        histories
            .entry(record.session_id.digest())
            .or_insert_with(Vec::new)
            .push(record);
    }
    histories.into_values().collect()
}

fn assert_cursor_refresh(index: &Path, registry: &SourceBackedProviderRegistry) {
    let receipt = refresh_source_backed_generation(index, registry, writer_options()).unwrap();
    assert!(
        receipt.failed_routes.is_empty(),
        "{:?}",
        receipt.failed_routes
    );
    assert!(
        receipt.logical_source_failures.is_empty(),
        "{:?}",
        receipt.logical_source_failures
    );
}

#[test]
fn cursor_forks_preserve_order_unique_content_and_tool_events_as_separate_histories() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    let first = cursor_message("user", "2026-08-16T00:00:00Z", "first marker");
    let second = cursor_message("assistant", "2026-08-16T00:00:01Z", "second marker");
    let tool = json!({
        "timestamp": "2026-08-16T00:00:02Z",
        "role": "assistant",
        "message": {"content": [{
            "type": "tool_use", "id": "fork-tool", "name": "read_file",
            "input": {"path": "example.txt"}
        }]}
    });
    write_transcript(
        &cursor_copy_path(&root, "a"),
        &[first.clone(), second.clone()],
    );
    write_transcript(&cursor_copy_path(&root, "b"), &[second, first.clone()]);
    write_transcript(
        &cursor_copy_path(&root, "c"),
        &[
            first,
            cursor_message("assistant", "2026-08-16T00:00:03Z", "unique marker"),
            tool,
        ],
    );
    let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
    assert_cursor_refresh(&index, &registry);
    let histories = cursor_histories(&index);
    assert_eq!(histories.len(), 3);
    assert_eq!(histories.iter().map(Vec::len).sum::<usize>(), 7);
    assert!(histories.iter().any(|history| {
        history[0].content.normalized_body.as_deref() == Some("first marker")
            && history[1].content.normalized_body.as_deref() == Some("second marker")
    }));
    assert!(histories.iter().any(|history| {
        history[0].content.normalized_body.as_deref() == Some("second marker")
            && history[1].content.normalized_body.as_deref() == Some("first marker")
    }));
    assert!(histories.iter().any(|history| {
        history
            .iter()
            .any(|record| record.event_type == "tool_call")
            && history
                .iter()
                .any(|record| record.content.normalized_body.as_deref() == Some("unique marker"))
    }));
    assert_cursor_refresh(&index, &registry);
    assert_eq!(cursor_histories(&index), histories);
}

#[test]
fn cursor_warm_fork_keeps_canonical_citations_across_growth_convergence_and_removal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    let canonical = cursor_copy_path(&root, "a");
    let alternative = cursor_copy_path(&root, "b");
    write_transcript(
        &canonical,
        &[cursor_message(
            "user",
            "2026-08-16T00:00:00Z",
            "canonical marker",
        )],
    );
    let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
    assert_cursor_refresh(&index, &registry);
    let initial = cursor_histories(&index).remove(0);
    write_transcript(
        &alternative,
        &[cursor_message(
            "user",
            "2026-08-16T00:00:00Z",
            "alternative marker",
        )],
    );
    assert_cursor_refresh(&index, &registry);
    let forked = cursor_histories(&index);
    assert_eq!(forked.len(), 2);
    assert!(forked.contains(&initial));
    let alternative_before = forked
        .iter()
        .find(|history| **history != initial)
        .unwrap()
        .clone();
    for ordinal in 1..4 {
        append_transcript(
            &alternative,
            &cursor_message(
                "assistant",
                "2026-08-16T00:00:01Z",
                &format!("alternative append {ordinal}"),
            ),
        );
    }
    assert_cursor_refresh(&index, &registry);
    let grown = cursor_histories(&index);
    assert!(grown.contains(&initial));
    let alternative_after = grown
        .iter()
        .find(|history| history[0].session_id == alternative_before[0].session_id)
        .unwrap();
    assert_eq!(alternative_after.len(), 4);
    assert_eq!(
        alternative_after[0].event_id,
        alternative_before[0].event_id
    );
    assert_eq!(alternative_after[0].source, alternative_before[0].source);
    let alternative_bytes = fs::read(&alternative).unwrap();
    write_transcript(
        &alternative,
        &[cursor_message(
            "user",
            "2026-08-16T00:00:00Z",
            "canonical marker",
        )],
    );
    assert_cursor_refresh(&index, &registry);
    let converged = cursor_histories(&index);
    assert_eq!(converged.len(), 2);
    assert!(converged.contains(&initial));
    let corrected = converged
        .iter()
        .find(|history| history[0].session_id == alternative_before[0].session_id)
        .unwrap();
    assert_literal_bodies(corrected, &["canonical marker"]);
    fs::remove_file(&alternative).unwrap();
    assert_cursor_refresh(&index, &registry);
    assert_eq!(cursor_histories(&index), converged);
    fs::write(&alternative, alternative_bytes).unwrap();
    assert_cursor_refresh(&index, &registry);
    assert_eq!(cursor_histories(&index), grown);
    fs::remove_file(&canonical).unwrap();
    assert_cursor_refresh(&index, &registry);
    assert_eq!(cursor_histories(&index), grown);
    // The surviving alternative must not inherit the missing canonical ID,
    // even after its bytes become equivalent to the old canonical history.
    write_transcript(
        &alternative,
        &[cursor_message(
            "user",
            "2026-08-16T00:00:00Z",
            "canonical marker",
        )],
    );
    assert_cursor_refresh(&index, &registry);
    let remaining = cursor_histories(&index);
    assert_eq!(remaining, converged);
    let alternative = remaining
        .iter()
        .find(|history| **history != initial)
        .unwrap();
    assert_eq!(alternative[0].session_id, alternative_before[0].session_id);
    assert_ne!(alternative[0].event_id, initial[0].event_id);
}

#[test]
fn cursor_unforked_equal_and_prefix_copies_still_dedupe() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    let first = cursor_message("user", "2026-08-16T00:00:00Z", "ordinary first");
    write_transcript(&cursor_copy_path(&root, "a"), std::slice::from_ref(&first));
    write_transcript(&cursor_copy_path(&root, "b"), &[first]);
    let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
    assert_cursor_refresh(&index, &registry);
    let initial = cursor_histories(&index).remove(0);
    append_transcript(
        &cursor_copy_path(&root, "b"),
        &cursor_message("assistant", "2026-08-16T00:00:01Z", "ordinary extension"),
    );
    assert_cursor_refresh(&index, &registry);
    let histories = cursor_histories(&index);
    assert_eq!(histories.len(), 1);
    assert_literal_bodies(&histories[0], &["ordinary first", "ordinary extension"]);
    assert_eq!(histories[0][0].event_id, initial[0].event_id);
    // Removing the selected extension still reuses the ordinary canonical ID
    // for its equivalent-prefix alias, as before fork preservation.
    fs::remove_file(cursor_copy_path(&root, "b")).unwrap();
    assert_cursor_refresh(&index, &registry);
    assert_eq!(cursor_histories(&index), vec![initial]);
}

fn named_cursor_registry(root: &Path) -> SourceBackedProviderRegistry {
    let home = root.parent().unwrap();
    let context = DiscoveryContext::new(
        home,
        home,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![ctx_history_capture_model::ProviderRootDefinition {
        id: "portable-cursor".to_owned(),
        provider: CaptureProvider::Cursor,
        path: home.to_path_buf(),
        group: None,
        kind: None,
    }]);
    let source = ProviderSource {
        provider: CaptureProvider::Cursor,
        path: root.to_path_buf(),
        exists: true,
        source_format: CURSOR_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance:
            ctx_history_capture_model::ProviderSourceRouteProvenance::ConfiguredRoot {
                root_id: "portable-cursor".to_owned(),
                root_path: root.parent().unwrap().to_path_buf(),
                route_role: ctx_history_capture_model::ProviderRouteRole::from_static(
                    "cursor-projects",
                ),
                automatic_route_role: None,
            },
    };
    let build = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &crate::test_provider_probes(),
        &context,
        &home.join("data"),
        DiscoveryReport {
            sources: vec![source],
            issues: Vec::new(),
        },
        &std::collections::BTreeMap::new(),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    assert_eq!(build.registry.routes().len(), 1);
    build.registry
}

#[test]
fn cursor_named_root_relocation_keeps_canonical_and_alternative_ids() {
    for cold_fork in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("before/projects");
        let moved = temp.path().join("after/projects");
        let index = temp.path().join("index");
        write_transcript(
            &cursor_copy_path(&root, "a"),
            &[cursor_message(
                "user",
                "2026-08-16T00:00:00Z",
                "canonical portable",
            )],
        );
        if !cold_fork {
            assert_cursor_refresh(&index, &named_cursor_registry(&root));
        }
        write_transcript(
            &cursor_copy_path(&root, "b"),
            &[cursor_message(
                "user",
                "2026-08-16T00:00:00Z",
                "alternative portable",
            )],
        );
        assert_cursor_refresh(&index, &named_cursor_registry(&root));
        let before = cursor_histories(&index);
        assert_eq!(before.len(), 2);
        fs::rename(root.parent().unwrap(), moved.parent().unwrap()).unwrap();
        let registry = named_cursor_registry(&moved);
        assert_cursor_refresh(&index, &registry);
        assert_eq!(cursor_histories(&index), before);
        append_transcript(
            &cursor_copy_path(&moved, "b"),
            &cursor_message("assistant", "2026-08-16T00:00:01Z", "portable append"),
        );
        assert_cursor_refresh(&index, &registry);
        let after = cursor_histories(&index);
        assert_eq!(after.len(), 2);
        assert_eq!(after.iter().map(Vec::len).sum::<usize>(), 3);
        for history in before {
            assert!(after.iter().any(|current| current.starts_with(&history)));
        }
    }
}

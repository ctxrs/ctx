use super::*;

#[test]
fn codex_cold_appends_during_bounded_capture_catch_up_once() {
    const INVENTORY_APPEND_MARKER: &str = "coldinventoryappendtoken631a";
    const OBSERVATION_APPEND_MARKER: &str = "coldobservationappendtoken631b";
    const SEMANTIC_APPEND_MARKER: &str = "coldsemanticappendtoken631c";
    const TERMINAL_APPEND_MARKER: &str = "coldterminalappendtoken631d";
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-continuous-append");
    let index_root = temp.path().join("index-continuous-append");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000063";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("coldprefixuniquetoken631")],
    );
    let registry = register_tree(&[&sessions]);

    // Product contract: docs/provider-import-policy.md#active-writer-lifecycle-contract.
    // Keep this as one end-to-end Codex route test: lower-level frozen-prefix
    // tests cannot prove that provider inventory and shared preflight compose.
    let append_path = path.clone();
    crate::provider::codex::nativepath::source_backed::install_after_codex_metadata_inventory_hook(
        move || {
            append_event(&append_path, message(INVENTORY_APPEND_MARKER));
        },
    );
    let append_path = path.clone();
    crate::provider::source_backed::family::jsonl::set_after_jsonl_append_observation_route_binding_hook(
        path.clone(),
        move || append_event(&append_path, message(OBSERVATION_APPEND_MARKER)),
    );
    let append_path = path.clone();
    crate::provider::source_backed::family::jsonl::set_after_jsonl_semantic_preflight_hook(
        path.clone(),
        move || append_event(&append_path, message(SEMANTIC_APPEND_MARKER)),
    );
    let append_path = path.clone();
    set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
        append_event(&append_path, message(TERMINAL_APPEND_MARKER));
    });

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(
        cold.failed_routes.is_empty(),
        "unexpected route failures: {:?}",
        cold.failed_routes
    );
    assert!(cold.logical_source_failures.is_empty());

    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert!(source_records_contain(
        &initial,
        native_session_id,
        "coldprefixuniquetoken631"
    ));
    for marker in [
        INVENTORY_APPEND_MARKER,
        OBSERVATION_APPEND_MARKER,
        SEMANTIC_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
    ] {
        assert!(
            !source_records_contain(&initial, native_session_id, marker),
            "cold publication included deferred suffix {marker}"
        );
    }
    drop(initial);

    let caught_up =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(caught_up.failed_routes.is_empty());
    assert!(caught_up.logical_source_failures.is_empty());
    let current = VerifiedIndex::open(&index_root).unwrap();
    let caught_up_generation = current.generation_id().to_owned();
    assert_eq!(records_for(&current, native_session_id).len(), 5);
    for marker in [
        INVENTORY_APPEND_MARKER,
        OBSERVATION_APPEND_MARKER,
        SEMANTIC_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
    ] {
        assert!(source_records_contain(&current, native_session_id, marker));
        assert_eq!(
            source_snapshot(&current, native_session_id, marker)
                .search_event_ids
                .len(),
            1,
            "catch-up did not index suffix {marker} exactly once"
        );
    }
    drop(current);

    let no_op = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(no_op.failed_routes.is_empty());
    assert!(no_op.logical_source_failures.is_empty());
    let terminal = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(terminal.generation_id(), caught_up_generation);
    assert_eq!(records_for(&terminal, native_session_id).len(), 5);
}

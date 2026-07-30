#[test]
fn owned_source_backed_constructors_have_no_preview_body_or_store_fallback() {
    let sources = [
        (
            "mistral_vibe",
            include_str!("../../mistral_vibe/native_path/source_backed.rs"),
        ),
        (
            "nanoclaw",
            include_str!("../../nanoclaw/native_path/source_backed.rs"),
        ),
        ("native_jsonl", include_str!("source_backed/lifecycle.rs")),
        (
            "openclaw",
            include_str!("../../openclaw/native_path/source_backed.rs"),
        ),
        (
            "openhands",
            include_str!("../../openhands/nativepath/source_backed.rs"),
        ),
        ("pi", include_str!("../../pi/nativepath/source_backed.rs")),
        (
            "shelley",
            include_str!("../../shelley/native_path/source_backed.rs"),
        ),
        (
            "task_json",
            include_str!("../../task_json/cline_nativepath/source_backed.rs"),
        ),
        (
            "trae",
            include_str!("../../trae/nativepath/source_backed.rs"),
        ),
        ("warp", include_str!("../../warp/source_backed.rs")),
        (
            "zed",
            include_str!("../../zed/native_path/source_backed.rs"),
        ),
    ];
    let forbidden = [
        concat!("MAX_BODY_", "PREVIEW_CHARS"),
        concat!("DIRECT_JSONL_LEXICAL_", "PREVIEW_CHARS"),
        concat!("MAX_LEXICAL_", "PREVIEW_CHARS"),
        concat!("bounded_", "lexical_body"),
        concat!("bounded_", "body"),
        concat!("lexical_", "preview"),
        concat!("body: event.", "preview"),
        concat!("ctx_history_", "store"),
        concat!("Store", "::"),
    ];

    for (provider, source) in sources {
        let has_body_assignment = source.lines().any(|line| {
            let line = line.trim();
            line == "body," || line.starts_with("body:")
        });
        assert!(
            source.contains("LexicalDocument {") && has_body_assignment,
            "{provider} no longer exposes an auditable LexicalDocument body assignment"
        );
        for token in forbidden {
            let contains_forbidden = source.lines().any(|line| {
                let line = line.trim();
                line.contains(token)
                    && !line.starts_with("assert!(!source.contains(")
                    && !line.starts_with("assert!(!provider_source.contains(")
            });
            assert!(
                !contains_forbidden,
                "{provider} source-backed path contains forbidden architecture token {token}"
            );
        }
    }

    assert!(sources
        .iter()
        .find(|(provider, _)| *provider == "warp")
        .unwrap()
        .1
        .contains("event.lexical_body"));
    assert!(sources
        .iter()
        .find(|(provider, _)| *provider == "zed")
        .unwrap()
        .1
        .contains("body: event.lexical_body"));
}

#[test]
fn direct_jsonl_family_streams_documents_and_owns_physical_work() {
    let module = include_str!("source_backed.rs");
    let adapter = include_str!("source_backed/adapter.rs");
    let hydration = include_str!("source_backed/hydration.rs");
    let lifecycle = include_str!("source_backed/lifecycle.rs");
    let registration = include_str!("source_backed/registration.rs");
    let projector = include_str!("reader.rs");
    let family = include_str!("../../../source_backed/family/jsonl.rs");

    for production in [module, adapter, hydration, lifecycle, registration] {
        assert!(production.lines().count() < 1_000);
        assert!(!production.contains("allow(dead_code)"));
        assert!(!production.contains("DirectJsonlSourcePage"));
        assert!(!production.contains("Vec<LexicalDocument>"));
        assert!(!production.contains("captured_route_driver"));
    }
    assert!(!projector.contains("reader_source"));
    assert!(registration.contains("emitter.emit_document(document)"));
    assert!(registration.contains("sink.run_parallel_leaf_scans_discovering_sources"));
    assert!(registration.contains("sink.recommended_leaf_workers"));
    assert!(!registration.contains("thread::scope"));
    assert!(!registration.contains("thread::spawn"));
    assert!(hydration.contains("open_hydration_catalog"));
    assert!(registration.contains(".with_batch_hydration"));
    assert!(hydration.contains("hydrate_resident_records"));
    assert!(!registration.contains("hydrate_legacy_single"));
    assert!(!adapter.contains("pub(super) source_file: Arc<OpenedProviderSourceFile>"));
    assert!(adapter.contains("open_verified"));
    assert!(family.contains("visit_page"));
    assert!(family.contains("visit_verified_ranges"));
    assert!(family.contains("complete_prefix_sha256"));
}

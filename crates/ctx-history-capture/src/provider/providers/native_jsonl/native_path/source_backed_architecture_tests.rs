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
        ("native_jsonl", include_str!("source_backed.rs")),
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
    let registration = include_str!("../../../source_backed/registration/families/jsonl/direct.rs");
    let projector = include_str!("reader.rs");
    let family = include_str!("../../../source_backed/family/jsonl.rs");
    let family_route = include_str!("../../../source_backed/family/jsonl/route.rs");
    let family_hydration = include_str!("../../../source_backed/family/jsonl/route/hydration.rs");
    let family_leaf = include_str!("../../../source_backed/family/jsonl/route/leaf.rs");

    for production in [module, registration, family_route] {
        assert!(production.lines().count() < 1_200);
        assert!(!production.contains("allow(dead_code)"));
        assert!(!production.contains("DirectJsonlSourcePage"));
        assert!(!production.contains("Vec<LexicalDocument>"));
        assert!(!production.contains("captured_route_driver"));
        assert!(!production.contains("DirectJsonlCheckpoint"));
        assert!(!production.contains("DirectJsonlSourceAdapter"));
        assert!(!production.contains("DirectJsonlRegistrationTestObserver"));
    }
    assert!(module.contains("impl JsonlFamilyAdapter for DirectJsonlFamilyAdapter"));
    assert!(!module.contains("mod adapter;"));
    assert!(!module.contains("mod hydration;"));
    assert!(!module.contains("mod lifecycle;"));
    assert!(!module.contains("mod registration;"));
    assert!(!projector.contains("reader_source"));
    assert!(registration.contains("jsonl_family_driver"));
    assert!(!registration.contains("DirectJsonlSourceAdapter"));
    assert!(family_leaf.contains("emitter.emit_document(document)"));
    assert!(family_leaf.contains(".run_parallel_leaf_scans("));
    assert!(family_leaf.contains("sink.recommended_leaf_workers"));
    assert!(!family_leaf.contains("thread::scope"));
    assert!(!family_leaf.contains("thread::spawn"));
    assert!(family_hydration.contains("hydrate_batch"));
    assert!(family_route.contains(".with_batch_hydration"));
    assert!(!registration.contains("hydrate_legacy_single"));
    assert!(!module.contains("pub(super) source_file: Arc<OpenedProviderSourceFile>"));
    assert!(module.contains("visit_verified_ranges"));
    assert!(family.contains("visit_page"));
    assert!(family.contains("visit_verified_ranges"));
    assert!(family.contains("complete_prefix_sha256"));
}

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
            "opencode",
            include_str!("../../opencode/native_path/source_backed.rs"),
        ),
        (
            "openhands",
            include_str!("../../openhands/nativepath/source_backed.rs"),
        ),
        ("pi", include_str!("../../pi/nativepath/source_backed.rs")),
        (
            "rovodev",
            include_str!("../../rovodev/native_path/source_backed.rs"),
        ),
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

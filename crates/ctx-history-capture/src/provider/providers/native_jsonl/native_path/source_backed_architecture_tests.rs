const OWNED_PROJECTORS: &[(&str, &str)] = &[
    (
        "claude",
        include_str!("../../claude/nativepath/source_backed.rs"),
    ),
    ("cursor", include_str!("../../cursor/source_backed.rs")),
    (
        "gemini",
        include_str!("../../gemini/nativepath/source_backed.rs"),
    ),
    (
        "junie",
        include_str!("../../junie/nativepath/source_backed.rs"),
    ),
    (
        "kimi",
        include_str!("../../kimi/native_path/source_backed/records.rs"),
    ),
    (
        "mistral-vibe",
        include_str!("../../mistral_vibe/native_path/source_backed.rs"),
    ),
    (
        "mux",
        include_str!("../../mux/native_path/source_backed/projection.rs"),
    ),
    ("native-jsonl", include_str!("source_backed.rs")),
    (
        "openclaw",
        include_str!("../../openclaw/native_path/source_backed.rs"),
    ),
    ("pi", include_str!("../../pi/nativepath/source_backed.rs")),
];

#[test]
fn every_owned_jsonl_projector_constructs_complete_core_records() {
    for (provider, source) in OWNED_PROJECTORS {
        assert!(source.contains("CoreRecord::new_selected("), "{provider}");
        assert!(source.contains("native_event_id = Some("), "{provider}");
        assert!(source.contains("validate_contract()"), "{provider}");
        assert!(
            !source.contains(concat!("Lexical", "Document")),
            "{provider}"
        );
        assert!(
            !source.contains(concat!("SourceRecord", "Locator")),
            "{provider}"
        );
        assert!(!source.contains("record.source_path"), "{provider}");
        assert!(!source.contains("truncate("), "{provider}");
        assert!(!source.contains("MAX_INDEXED_BODY"), "{provider}");
    }
}

#[test]
fn direct_core_projection_has_stable_revision_and_no_resolver_reread() {
    for (provider, source) in OWNED_PROJECTORS {
        assert!(source.contains("PARSER_REVISION"), "{provider}");
        assert!(
            !source.contains(concat!("Event", "Hydra", "tionRequest")),
            "{provider}"
        );
        assert!(
            !source.contains(concat!("Hydra", "tedProviderRecord")),
            "{provider}"
        );
        assert!(!source.contains(concat!("fn hydra", "tor")), "{provider}");
        assert!(!source.contains(concat!("fn hydra", "te_")), "{provider}");
        assert!(!source.contains("Utc::now"), "{provider}");
    }
}

#[test]
fn direct_core_body_is_selected_once_without_raw_duplication() {
    for (provider, source) in OWNED_PROJECTORS {
        let constructors = source.matches("CoreRecord::new_selected(").count();
        assert_eq!(constructors, 1, "{provider}");
        assert!(!source.contains("\"raw_record\""), "{provider}");
        assert!(!source.contains("\"source_path\""), "{provider}");
        assert!(!source.contains("\"locator\""), "{provider}");
    }
}

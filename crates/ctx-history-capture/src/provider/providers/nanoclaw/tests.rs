#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let scanner = include_str!("source.rs");
    let sources = [
        include_str!("../../../native_source.rs"),
        include_str!("position.rs"),
        include_str!("native_path/source_backed.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production
        .contains("let native_event_id = TypedKey::composite(native_event_parts.clone())"));
    assert!(production.contains("TypedKey::utf8(&message.id)"));
    assert!(production.contains("NANOCLAW_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let mut body = exact_text"));
    assert!(scanner.contains("nanoclaw_hydrate_native_message"));
    assert!(scanner.contains("message_rowid"));
    assert!(!scanner.contains(concat!("Native", "Locator")));
    assert!(!scanner.contains(concat!("nanoclaw_message_", "locator")));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("Native", "Locator"),
        concat!("nanoclaw_message_", "locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

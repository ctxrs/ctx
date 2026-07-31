#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("KIRO_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let body = complete_text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

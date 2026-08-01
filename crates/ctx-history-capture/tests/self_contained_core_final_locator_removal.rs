#[test]
fn nanoclaw_uses_native_message_identity_without_a_durable_row_locator() {
    let native_source = include_str!("../src/native_source.rs");
    let position = include_str!("../src/provider/providers/nanoclaw/position.rs");
    let scanner = include_str!("../src/provider/providers/nanoclaw/source.rs");
    let projection =
        include_str!("../src/provider/providers/nanoclaw/native_path/source_backed.rs");

    for source in [native_source, position, scanner, projection] {
        for removed in [
            concat!("Native", "Locator"),
            concat!("nanoclaw_message_", "locator"),
        ] {
            assert!(!source.contains(removed), "found {removed}");
        }
    }
    assert!(projection
        .contains("let native_event_id = TypedKey::composite(native_event_parts.clone())"));
    assert!(projection.contains("TypedKey::utf8(message_source)"));
    assert!(projection.contains("TypedKey::utf8(&message.id)"));
    assert!(!projection.contains("TypedKey::bytes"));

    assert!(scanner.contains("nanoclaw_hydrate_native_message"));
    assert!(scanner.contains("message_rowid"));
}

#[test]
fn cursor_claude_and_core_have_no_content_reference_seam() {
    let sources = [
        include_str!("../src/provider/providers/cursor/parser.rs"),
        include_str!("../src/provider/providers/cursor/projection.rs"),
        include_str!("../src/provider/providers/cursor/source_backed.rs"),
        include_str!("../src/provider/providers/claude.rs"),
        include_str!("../src/provider/providers/claude/nativepath/rows.rs"),
        include_str!("../src/provider/providers/claude/nativepath/record.rs"),
        include_str!("../src/provider/providers/claude/nativepath/record/value_decoding.rs"),
        include_str!("../../ctx-history-core/src/lib.rs"),
    ];

    for source in sources {
        for removed in [
            concat!("Content", "Ref"),
            concat!("complete_content", "_ref"),
            concat!("complete_body", "_ref"),
        ] {
            assert!(!source.contains(removed), "found {removed}");
        }
    }
}

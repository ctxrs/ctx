use super::{
    display_snippet, event_preview_text, fixed_time, sync_metadata, wrap_delimited, Event,
    EventRole, EventType, Uuid,
};

#[test]
fn local_snippets_preserve_transcript_text() {
    let snippet = display_snippet(
        "token=ghp_1234567890abcdef1234567890abcdef and password=hunter2",
        200,
    );

    assert!(snippet.contains("token=ghp_1234567890abcdef1234567890abcdef"));
    assert!(snippet.contains("password=hunter2"));
}

#[test]
fn events_render_payload_previews_when_payload_exists() {
    let event = Event {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({"text": "local payload should render"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    };

    let preview = event_preview_text(&event);
    assert!(preview.contains("local payload should render"));
}

#[test]
fn wrap_delimited_escapes_content_and_uses_nonce_in_both_delimiters() {
    let nonce = "abc123";
    let input = "<script>alert(\"xss\")&'hello'</script>";
    let result = wrap_delimited(input, nonce);

    // Build expected string with \u{} escapes so no rendering ambiguity.
    // \u{26} = ampersand, \u{3b} = semicolon, \u{3c} = less-than, \u{3e} = greater-than
    // \u{22} = double-quote, \u{27} = apostrophe
    let expected = "[[RECALLED_DATA nonce=abc123]]\
         \u{26}lt\u{3b}script\u{26}gt\u{3b}\
         alert(\u{26}quot\u{3b}xss\u{26}quot\u{3b})\
         \u{26}amp\u{3b}\u{26}apos\u{3b}hello\u{26}apos\u{3b}\
         \u{26}lt\u{3b}/script\u{26}gt\u{3b}\
         [[/RECALLED_DATA nonce=abc123]]"
        .to_string();

    assert_eq!(result, expected);
    assert!(result.starts_with("[[RECALLED_DATA nonce=abc123]]"));
    assert!(result.ends_with("[[/RECALLED_DATA nonce=abc123]]"));
    // input contained U+003C — output must NOT contain it
    assert!(!result.contains('\u{3c}'));
    // input contained U+003E — output must NOT contain it
    assert!(!result.contains('\u{3e}'));
    // output must contain ampersand + l + t + semicolon (the escaped form)
    assert!(result.contains("\u{26}lt\u{3b}"));
    // output must contain ampersand + a m p + semicolon
    assert!(result.contains("\u{26}amp\u{3b}"));
}

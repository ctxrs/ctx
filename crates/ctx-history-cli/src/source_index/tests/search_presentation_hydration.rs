#[test]
fn file_only_search_keeps_an_oversized_grapheme_body_without_requiring_a_body_match() {
    let temp = tempdir().unwrap();
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 94, 1);
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
    let mut stored = fixture_core_event(&event, oversized_cluster);
    stored.core_record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::File,
            value: "src/huge.rs".to_owned(),
        }],
    });
    stored.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&stored), 94);

    let mut source_request = request(RefreshArg::Off);
    source_request.query.clear();
    source_request.file = Some(PathBuf::from("src/huge.rs"));
    source_request.events = true;
    source_request.limit = 1;
    let (value, collection, _) = search_existing_generation(
        &source_request,
        open_index(temp.path()).unwrap(),
        temp.path(),
        source_request.semantic_weight,
        "existing_generation",
        1,
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 1);
    assert_eq!(
        value["results"][0]["ctx_event_id"],
        json!(event.event_id.as_uuid())
    );
    assert_eq!(value["results"][0]["snippet"], "…src/huge.rs");
    assert_eq!(value["results"][0]["snippet_truncated"], true);

    let (mcp, _) = mcp_search(source_request, temp.path(), history_snapshot(true, true)).unwrap();
    assert_eq!(
        mcp["results"][0]["ctx_event_id"],
        json!(event.event_id.as_uuid())
    );
    assert_eq!(mcp["results"][0]["snippet"], "…src/huge.rs");
    assert_eq!(mcp["results"][0]["snippet_truncated"], true);
}

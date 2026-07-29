use super::*;

#[test]
fn source_backed_projection_preserves_semantics_without_legacy_operations() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000002";
    let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    let long_message = "long-message-sentinel complete-message-tail".to_owned();
    let tool_record = tool_call_with_patch("touch-call");
    let failed_record = failed_tool_output("touch-call");
    fs::write(
        &session_path,
        format!(
            "{}\n{}\n{tool_record}\n{failed_record}\n",
            session_meta(native_session_id),
            message("assistant", &long_message)
        ),
    )
    .unwrap();

    let receipt = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_no_legacy_operations(receipt.counters);
    assert_eq!(receipt.counters.complete_records_scanned, 4);
    assert_eq!(receipt.counters.retained_records_scanned, 3);
    assert_eq!(receipt.counters.staged_documents, 3);
    assert_eq!(receipt.counters.structural_json_parses, 4);
    assert_eq!(receipt.counters.typed_json_parses, 3);

    let source_key = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source_key, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let events = verified.events_for_session(session_id.as_uuid()).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(events[0].event_type, EventType::Message.as_str());
    assert_eq!(events[1].event_type, EventType::ToolCall.as_str());
    assert_eq!(events[1].touched_files, vec!["src/source_backed.rs"]);
    assert_eq!(events[2].event_type, EventType::ToolOutput.as_str());
    assert_eq!(events[2].role.as_deref(), Some("tool"));
    assert!(verified
        .search_event_candidates("long message sentinel", 10)
        .unwrap()
        .iter()
        .any(|candidate| candidate.event.event_id == events[0].event_id));
    let hydrated = hydrate_codex_locator(&sessions, &events[0].locator).unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(long_message.as_str())
    );
}

#[test]
fn source_backed_scanner_keeps_full_message_tail_and_exact_display_text() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000022";
    let full_text = format!(
        "codex-full-{}-codex-tail-sentinel",
        "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 512)
    );
    write_session(
        &sessions,
        native_session_id,
        &[message("assistant", &full_text)],
    );

    let (catalog_summary, catalog_sessions) = discover_codex_session_catalog(&sessions).unwrap();
    assert_eq!(catalog_summary.failed_sessions, 0);
    let discovery = super::super::discover_codex_catalog_sources(&catalog_sessions);
    assert!(discovery.rejections.is_empty());
    let catalog_source = discovery.sources.into_iter().next().unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let mut scanner =
        CodexNativeScanner::new_source_backed_v0(catalog_source.clone(), None).unwrap();
    let mut documents = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        let CodexNativeOwnedPage::Core(page) = page;
        let owner = page.owner.unwrap();
        for row in page.source_backed_rows {
            documents.push(
                codex_lexical_document(&catalog_source, &source, session_id, &owner, row).unwrap(),
            );
        }
    }
    scanner.finish().unwrap();

    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, full_text);
    assert!(documents[0].body.ends_with("codex-tail-sentinel"));
    let hydrated = hydrate_codex_locator(&sessions, &documents[0].locator).unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(documents[0].body.as_str())
    );
}

#[test]
fn source_backed_codex_adapter_has_no_store_or_preview_body_fallback() {
    let adapter = [
        include_str!("../../source_backed.rs"),
        include_str!("../catalog.rs"),
        include_str!("../cold.rs"),
        include_str!("../hydration.rs"),
        include_str!("../identity.rs"),
        include_str!("../ingestion.rs"),
    ]
    .join("\n");
    let rows = include_str!("../../rows.rs");
    let store_dependency = ["ctx_history_", "store"].concat();
    let preview_body = [
        "return codex_local_preview(text, ",
        "CODEX_LEXICAL_PREVIEW_CHARS).0",
    ]
    .concat();
    assert!(!adapter.contains(&store_dependency));
    assert!(!rows.contains(&preview_body));
}

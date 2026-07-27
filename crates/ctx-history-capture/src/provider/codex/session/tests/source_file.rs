use super::*;

#[test]
fn codex_empty_source_is_a_rejected_source_without_store_scaffolding() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("empty.jsonl");
    fs::write(&path, "").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures[0].line, 1);
    assert!(summary.failures[0]
        .error
        .starts_with("codex session JSONL contained no real message content: "));
    assert!(summary.failures[0].error.contains("empty.jsonl"));
    assert!(!summary.has_accepted_content());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_complete_oversize_only_session_replays_rejection_without_store_scaffolding() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("oversize-only.jsonl");
    let mut source_bytes = session_meta("oversize-only-session", None);
    source_bytes.push_str(
            r#"{"timestamp":"2026-07-18T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":""#,
        );
    source_bytes.push_str(&"x".repeat(MAX_PROVIDER_JSONL_LINE_BYTES));
    source_bytes.push_str("\"}]}}\n");
    fs::write(&path, source_bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
        .unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].line, 2);
    assert!(!first.has_accepted_content());

    let replay =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert!(
        replay.failures.is_empty(),
        "certified replay must retain only the bounded rejection count"
    );
    assert_eq!(replay.imported, 0);
    assert!(!replay.has_accepted_content());

    assert!(store.list_records(1).unwrap().is_empty());
    assert_eq!(store.capture_source_count().unwrap(), 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_rewrites_large_header_outside_append_proof_and_resets_before_append() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("rewritten-large-header.jsonl");
    let header = |marker: &str| {
        jsonl_line(json!({
            "timestamp": "2026-07-18T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "rewritten-large-header-session",
                "timestamp": "2026-07-18T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli",
                "source": {
                    "marker": marker,
                    "padding": "h".repeat(72 * 1024)
                }
            }
        }))
    };
    let header_a = header("rewrite-a");
    let header_b = header("rewrite-b");
    assert_eq!(header_a.len(), header_b.len());
    assert!(header_a.len() > 64 * 1024);
    let large_message = jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "m".repeat(72 * 1024)
            }]
        }
    }));
    let initial = [header_a.as_str(), large_message.as_str()].concat();
    let rewritten_prefix = [header_b.as_str(), large_message.as_str()].concat();
    assert_eq!(initial.len(), rewritten_prefix.len());
    let suffix_start = initial.len() - 64 * 1024;
    assert!(suffix_start > header_a.len());
    assert_eq!(
        &initial.as_bytes()[suffix_start..],
        &rewritten_prefix.as_bytes()[suffix_start..]
    );
    let appended_message = message(1);
    let complete = [rewritten_prefix.as_str(), appended_message.as_str()].concat();
    let options = CodexSessionImportOptions {
        imported_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        ..CodexSessionImportOptions::default()
    };

    fs::write(&path, &initial).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 1);
    let path_identity = provider_path_identity(&path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &path_identity,
    );
    let first_cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let first_certified = CertifiedProviderCursor::decode(&first_cursor.cursor).unwrap();
    let first_checkpoint: CodexParserCheckpoint =
        first_certified.parser_checkpoint().deserialize().unwrap();
    let first_anchor = first_checkpoint.header_anchor.unwrap();

    fs::write(&path, &complete).unwrap();
    let replacement = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(replacement.failed, 0, "{:?}", replacement.failures);
    assert_eq!(replacement.imported_events, 1);
    assert_eq!(replacement.skipped_events, 1);

    let session = store
        .session_by_external_session(CaptureProvider::Codex, "rewritten-large-header-session")
        .unwrap()
        .unwrap();
    let source_preview = session.sync.metadata["metadata"]["source"]["json"]
        .as_str()
        .expect("large source metadata should be represented as capped JSON");
    assert!(source_preview.contains(r#""marker":"rewrite-b""#));
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);

    let final_cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let final_certified = CertifiedProviderCursor::decode(&final_cursor.cursor).unwrap();
    assert_eq!(
        jsonl_position_offset(final_certified.native_position()).unwrap(),
        complete.len() as u64
    );
    let final_checkpoint: CodexParserCheckpoint =
        final_certified.parser_checkpoint().deserialize().unwrap();
    let final_anchor = final_checkpoint.header_anchor.unwrap();
    assert_eq!(final_anchor.payload_bytes, first_anchor.payload_bytes);
    assert_ne!(final_anchor.sha256, first_anchor.sha256);
}

#[test]
fn codex_incomplete_verified_append_retains_exact_cursor_and_reports_diagnostic() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("incomplete-append.jsonl");
    let initial = [session_meta("incomplete-append-session", None), message(0)].concat();
    fs::write(&path, &initial).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = CodexSessionImportOptions::default();
    let first = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &provider_path_identity(&path).unwrap(),
    );
    let before = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"type":"response_item""#).unwrap();
    file.sync_all().unwrap();
    let incomplete = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();

    assert_eq!(incomplete.failed, 1, "{:?}", incomplete.failures);
    assert_eq!(incomplete.failures[0].line, 3);
    assert_eq!(
        incomplete.failures[0].error,
        "Codex session JSONL ended with an incomplete record"
    );
    let after = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "incomplete-append-session")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn codex_mtime_only_no_batch_retains_exact_cursor_and_replays() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("mtime-only.jsonl");
    let source_bytes = [session_meta("mtime-only-session", None), message(0)].concat();
    fs::write(&path, &source_bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = CodexSessionImportOptions::default();
    import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &provider_path_identity(&path).unwrap(),
    );
    let before = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let prior_revision = CodexFrozenFileMetadata::read(&path)
        .unwrap()
        .source_revision();
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        fs::write(&path, &source_bytes).unwrap();
        if CodexFrozenFileMetadata::read(&path)
            .unwrap()
            .source_revision()
            != prior_revision
        {
            break;
        }
    }
    assert_ne!(
        CodexFrozenFileMetadata::read(&path)
            .unwrap()
            .source_revision(),
        prior_revision
    );

    let replay = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.skipped_events, 1);
    let after = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
}

#[test]
fn codex_replays_leading_rejection_before_later_anchored_header() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("later-header.jsonl");
    let source_bytes = [
        "{\"type\":\"turn_context\",\"payload\":{}}\n",
        "{\"type\":\"session_meta\"\n",
        session_meta("later-header-session", None).as_str(),
        message(0).as_str(),
    ]
    .concat();
    fs::write(&path, source_bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
        .unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures[0].line, 2);
    assert_eq!(first.imported_events, 1);

    let replay =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert!(replay.failures.is_empty());
    assert_eq!(replay.skipped_events, 1);
}

#[test]
fn codex_oversized_header_replays_as_genuinely_headerless_checkpoint() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("oversized-header.jsonl");
    let mut source_bytes =
        r#"{"type":"session_meta","payload":{"id":"oversized-header","padding":""#.to_owned();
    source_bytes.push_str(&"x".repeat(MAX_PROVIDER_JSONL_LINE_BYTES));
    source_bytes.push_str("\"}}\n");
    fs::write(&path, source_bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
        .unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert!(store.list_sessions().unwrap().is_empty());

    let replay =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert!(replay.failures.is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

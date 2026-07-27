use super::*;

#[test]
fn codex_store_cursor_replays_exactly_without_reprojecting() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("replay.jsonl");
    fs::write(
        &path,
        [session_meta("replay-session", None), message(0)].concat(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let (first, opens) = count_codex_source_file_opens(|| {
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
    });
    let first = first.unwrap();
    assert_eq!(opens, 1);
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);

    let replay =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 1);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn codex_legacy_tail_starts_at_the_certified_file_boundary() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("legacy-tail.jsonl");
    let initial = [session_meta("legacy-tail-session", None), message(0)].concat();
    let appended = message(1);
    let complete = [initial.as_str(), appended.as_str()].concat();

    fs::write(&path, &complete).unwrap();
    let mut full_store = Store::open(temp.path().join("full.sqlite")).unwrap();
    let full =
        import_codex_session_jsonl(&path, &mut full_store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(full.failed, 0, "{:?}", full.failures);
    let full_session = full_store
        .session_by_external_session(CaptureProvider::Codex, "legacy-tail-session")
        .unwrap()
        .unwrap();
    let full_event_ids = full_store
        .events_for_session(full_session.id)
        .unwrap()
        .into_iter()
        .map(|event| event.id)
        .collect::<Vec<_>>();

    fs::write(&path, &initial).unwrap();
    let tail_options = CodexSessionImportOptions::default();
    let mut legacy_store = Store::open(temp.path().join("legacy.sqlite")).unwrap();
    let legacy =
        import_codex_session_jsonl(&path, &mut legacy_store, tail_options.clone()).unwrap();
    assert_eq!(legacy.failed, 0, "{:?}", legacy.failures);
    assert_eq!(legacy.imported_events, 1);
    let machine_id = tail_options.machine_id.clone();
    let path_identity = provider_path_identity(&path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &path_identity,
    );
    legacy_store
        .upsert_sync_cursor(&ctx_history_core::SyncCursor {
            id: ctx_history_core::new_id(),
            team_id: None,
            device_id: tail_options.machine_id.clone(),
            stream: stream.clone(),
            cursor: "line:2".to_owned(),
            last_synced_at: Some(tail_options.imported_at),
            timestamps: crate::provider::importer::timestamps(tail_options.imported_at),
        })
        .unwrap();

    fs::write(&path, &complete).unwrap();
    let tail = import_codex_session_jsonl_tail(
        &path,
        initial.len() as u64,
        &mut legacy_store,
        tail_options,
    )
    .unwrap();
    assert_eq!(tail.failed, 0, "{:?}", tail.failures);
    assert_eq!(tail.imported_events, 1);
    assert_eq!(tail.skipped_events, 0, "prefix event was reprojected");
    let published = legacy_store
        .get_sync_cursor(None, &machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&published.cursor).unwrap();
    assert_eq!(
        jsonl_position_offset(certified.native_position()).unwrap(),
        complete.len() as u64
    );
    let checkpoint: CodexParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 3);

    let legacy_session = legacy_store
        .session_by_external_session(CaptureProvider::Codex, "legacy-tail-session")
        .unwrap()
        .unwrap();
    let legacy_event_ids = legacy_store
        .events_for_session(legacy_session.id)
        .unwrap()
        .into_iter()
        .map(|event| event.id)
        .collect::<Vec<_>>();
    assert_eq!(legacy_event_ids, full_event_ids);
}

#[test]
fn codex_tail_at_exact_eof_replaces_legacy_cursor() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("legacy-eof.jsonl");
    let source_bytes = [session_meta("legacy-eof-session", None), message(0)].concat();
    fs::write(&path, &source_bytes).unwrap();
    let options = CodexSessionImportOptions::default();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &provider_path_identity(&path).unwrap(),
    );
    store
        .upsert_sync_cursor(&ctx_history_core::SyncCursor {
            id: ctx_history_core::new_id(),
            team_id: None,
            device_id: options.machine_id.clone(),
            stream: stream.clone(),
            cursor: "line:2".to_owned(),
            last_synced_at: Some(options.imported_at),
            timestamps: crate::provider::importer::timestamps(options.imported_at),
        })
        .unwrap();

    let tail = import_codex_session_jsonl_tail(
        &path,
        u64::try_from(source_bytes.len()).unwrap(),
        &mut store,
        options.clone(),
    )
    .unwrap();
    assert_eq!(tail, ProviderImportSummary::default());
    let published = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&published.cursor).unwrap();
    assert_eq!(
        jsonl_position_offset(certified.native_position()).unwrap(),
        u64::try_from(source_bytes.len()).unwrap()
    );
    let checkpoint: CodexParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 2);
    assert!(checkpoint.header_anchor.is_some());
}

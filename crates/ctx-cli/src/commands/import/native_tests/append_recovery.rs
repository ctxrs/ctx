fn write_valid_codex_session(root: &Path, session_id: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join(format!("{session_id}.jsonl")),
        format!(
            "{{\"timestamp\":\"2026-07-13T12:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session_id}\",\"timestamp\":\"2026-07-13T12:00:00Z\",\"cwd\":\"/repo\",\"originator\":\"codex-cli\",\"source\":\"cli\"}}}}\n\
             {{\"timestamp\":\"2026-07-13T12:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"supersession retry oracle\"}}]}}}}\n"
        ),
    )
    .unwrap();
}

#[test]
fn codex_full_rescan_retries_one_superseded_result_generation() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    write_valid_codex_session(&source_path, "resume-superseded-once");
    let source = explicit_path_source(CaptureProvider::Codex, source_path.clone());
    let source_root = source_path.display().to_string();
    let superseded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_superseded = Arc::clone(&superseded);
    let callback_db = db_path.clone();
    let progress: CodexSessionImportProgressCallback = Arc::new(move |progress| {
        if progress.done && !callback_superseded.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let competing = Store::open(&callback_db).unwrap();
            competing
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
                .unwrap();
        }
    });
    let mut store = Store::open(&db_path).unwrap();

    let summary = import_one_source_inner(
        &mut store,
        &source,
        Some(progress),
        false,
        true,
        &SourcePreinventory::None,
    )
    .unwrap();

    assert!(superseded.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        summary
            .imported_sessions
            .saturating_add(summary.skipped_sessions),
        1
    );
}

#[test]
fn codex_supersession_retry_exhaustion_stays_system_scoped() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    write_valid_codex_session(&source_path, "resume-superseded-always");
    let source = explicit_path_source(CaptureProvider::Codex, source_path.clone());
    let source_root = source_path.display().to_string();
    let callback_db = db_path.clone();
    let superseded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let callback_superseded = Arc::clone(&superseded);
    let progress: CodexSessionImportProgressCallback = Arc::new(move |progress| {
        if progress.done {
            let competing = Store::open(&callback_db).unwrap();
            competing
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
                .unwrap();
            callback_superseded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let mut store = Store::open(&db_path).unwrap();

    let error = import_one_source_inner(
        &mut store,
        &source,
        Some(progress),
        false,
        true,
        &SourcePreinventory::None,
    )
    .expect_err("three superseded result generations must exhaust the retry budget");

    assert!(superseded.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    assert!(is_inventory_superseded(&error));
    assert_eq!(import_error_scope(&error), ImportFailureScope::System);
}

use super::*;

#[test]
fn filtered_search_pages_past_fts_decoys() {
    let (_temp, store) = test_store();
    let query = "overflow-filter-needle";
    let old_time = fixed_time() - chrono::Duration::days(14);
    let mut records = Vec::new();

    for index in 0..501_u16 {
        let mut decoy = HistoryRecord::new(
            "Overflow filter shared title",
            format!("{query} identical body for paging regression"),
            Vec::new(),
            "task",
            None,
        );
        decoy.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8000-{index:012x}")).unwrap();
        decoy.created_at = old_time;
        decoy.updated_at = old_time;
        records.push(decoy);
    }

    let mut target = HistoryRecord::new(
        "Overflow filter shared title",
        format!("{query} identical body for paging regression"),
        Vec::new(),
        "task",
        Some("/workspace/ctx-filter-target".into()),
    );
    target.id = Uuid::parse_str("018f45d0-0000-7000-8000-ffffffffffff").unwrap();
    target.created_at = old_time;
    target.updated_at = fixed_time();
    records.push(target.clone());
    store.upsert_records(&records).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-fffffffffffe").unwrap(),
        history_record_id: Some(target.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("overflow-filter-session".into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    };
    store.upsert_session(&session).unwrap();

    let file = FileTouched {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-fffffffffffd").unwrap(),
        history_record_id: Some(target.id),
        run_id: None,
        event_id: None,
        vcs_workspace_id: None,
        path: "crates/search/src/overflow_filter.rs".into(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(3),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_file_touched(&file).unwrap();

    let first_raw_page = store.search_records(query, 500).unwrap();
    assert_eq!(first_raw_page.len(), 500);
    assert!(
        !first_raw_page.iter().any(|record| record.id == target.id),
        "regression setup must place the filtered hit behind the first 500 raw matches"
    );

    let cases = vec![
        (
            "provider",
            SearchFilters {
                provider: Some(CaptureProvider::Codex),
                ..SearchFilters::default()
            },
        ),
        (
            "repo",
            SearchFilters {
                repo: Some("ctx-filter-target".into()),
                ..SearchFilters::default()
            },
        ),
        (
            "file",
            SearchFilters {
                file: Some("overflow_filter.rs".into()),
                ..SearchFilters::default()
            },
        ),
        (
            "since",
            SearchFilters {
                since: Some(fixed_time() - chrono::Duration::hours(1)),
                ..SearchFilters::default()
            },
        ),
        (
            "combined",
            SearchFilters {
                provider: Some(CaptureProvider::Codex),
                repo: Some("ctx-filter-target".into()),
                since: Some(fixed_time() - chrono::Duration::hours(1)),
                file: Some("overflow_filter.rs".into()),
                ..SearchFilters::default()
            },
        ),
    ];

    for (name, filters) in cases {
        let packet = search_packet(
            &store,
            query,
            &PacketOptions {
                limit: 1,
                filters,
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            packet
                .results
                .iter()
                .map(|result| result.record_id)
                .collect::<Vec<_>>(),
            vec![target.id],
            "{name} filter failed to page past decoys"
        );
    }
}

#[test]
fn file_filter_matches_event_linked_file_touches_on_fast_path() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Event linked file touch",
        "record body without the event needle",
        Vec::new(),
        "task",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&record).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f101").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("event-linked-file-touch-session".into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    };
    store.upsert_session(&session).unwrap();

    let event = Event {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f102").unwrap(),
        seq: 7,
        history_record_id: None,
        session_id: Some(session.id),
        run_id: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({"text": "event-file-scope-needle apply patch"}),
        payload_blob_id: None,
        dedupe_key: None,
        redaction_state: RedactionState::SafePreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();
    for index in 0..(LARGE_EVENT_CORPUS_THRESHOLD - 1) {
        let decoy = Event {
            id: Uuid::parse_str(&format!("018f45d0-0000-7000-8000-00000001{index:04x}")).unwrap(),
            seq: 1000 + index as u64,
            history_record_id: None,
            session_id: Some(session.id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time() + chrono::Duration::milliseconds(index),
            capture_source_id: None,
            payload: serde_json::json!({"text": format!("decoy event {index}")}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&decoy).unwrap();
    }

    store
        .upsert_file_touched(&FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f103").unwrap(),
            history_record_id: None,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: "crates/ctx-cli/src/main.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        })
        .unwrap();

    let packet = search_packet(
        &store,
        "event-file-scope-needle",
        &PacketOptions {
            limit: 5,
            filters: SearchFilters {
                file: Some("src/main.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    assert_eq!(packet.results[0].event_id, Some(event.id));
    assert_eq!(packet.results[0].result_scope, SearchResultScope::Session);

    let wrong_file = search_packet(
        &store,
        "event-file-scope-needle",
        &PacketOptions {
            limit: 5,
            filters: SearchFilters {
                file: Some("src/lib.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert!(wrong_file.results.is_empty());
}

#[test]
fn file_filter_treats_like_wildcards_as_literal_path_characters() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Literal file wildcard test",
        "literal-file-wildcard-needle",
        Vec::new(),
        "task",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&record).unwrap();
    store
        .upsert_file_touched(&FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000f203").unwrap(),
            history_record_id: Some(record.id),
            run_id: None,
            event_id: None,
            vcs_workspace_id: None,
            path: "src/fooXbar.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        })
        .unwrap();

    let packet = search_packet(
        &store,
        "literal-file-wildcard-needle",
        &PacketOptions {
            limit: 5,
            filters: SearchFilters {
                file: Some("src/foo_bar.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert!(packet.results.is_empty());
}

#[test]
fn file_only_search_finds_old_sparse_file_touch_beyond_recent_scan_budget() {
    let (_temp, store) = test_store();
    let old_time = fixed_time() - chrono::Duration::days(30);
    let target_id = Uuid::parse_str("018f45d0-0000-7000-8003-ffffffffffff").unwrap();
    let mut target = HistoryRecord::new(
        "Old sparse file touch",
        "older session that only relates through file touch scope",
        Vec::new(),
        "task",
        Some("/workspace/ctx".into()),
    );
    target.id = target_id;
    target.created_at = old_time;
    target.updated_at = old_time;
    store.upsert_record(&target).unwrap();
    store
        .upsert_file_touched(&FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8003-fffffffffffe").unwrap(),
            history_record_id: Some(target_id),
            run_id: None,
            event_id: None,
            vcs_workspace_id: None,
            path: "crates/ctx-history-search/src/sparse_history.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(1),
            confidence: Confidence::Explicit,
            timestamps: EntityTimestamps {
                created_at: old_time,
                updated_at: old_time,
            },
            source_id: None,
            sync: sync_metadata(),
        })
        .unwrap();

    let mut decoys = Vec::new();
    for index in 0..=(FILTERED_SEARCH_PAGE_SIZE * FILTERED_SEARCH_MAX_PAGES) {
        let decoy_time = fixed_time() + chrono::Duration::seconds(index as i64);
        let mut decoy = HistoryRecord::new(
            "Recent unrelated session",
            format!("recent non-file decoy {index:05}"),
            Vec::new(),
            "task",
            Some("/workspace/other".into()),
        );
        decoy.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8004-{index:012x}")).unwrap();
        decoy.created_at = decoy_time;
        decoy.updated_at = decoy_time;
        decoys.push(decoy);
    }
    store.upsert_records(&decoys).unwrap();

    let old_scan_window = store
        .list_records_page(FILTERED_SEARCH_PAGE_SIZE * FILTERED_SEARCH_MAX_PAGES, 0)
        .unwrap();
    assert!(
        !old_scan_window.iter().any(|record| record.id == target_id),
        "regression setup must place the file match beyond the old recent-record scan window"
    );

    let packet = search_packet(
        &store,
        "",
        &PacketOptions {
            limit: 5,
            filters: SearchFilters {
                file: Some("sparse_history.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        packet
            .results
            .iter()
            .map(|result| result.record_id)
            .collect::<Vec<_>>(),
        vec![target_id]
    );
    assert!(!packet.truncation.truncated);
    assert!(packet.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "file_touched"));
}

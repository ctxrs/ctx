use super::{
    fixed_time, search_packet, sync_metadata, test_store, timestamps, AgentType, CaptureProvider,
    Confidence, EntityTimestamps, Event, EventRole, EventType, FileChangeKind, FileTouched,
    HistoryRecord, PacketOptions, RedactionState, SearchFilters, Session, SessionStatus, Uuid,
    FILTERED_SEARCH_MAX_PAGES, FILTERED_SEARCH_PAGE_SIZE, MAX_RESULT_LIMIT,
};

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

#[test]
fn search_ignores_agent_history_bookkeeping_terms_without_content_evidence() {
    let (_temp, store) = test_store();
    let mut record = HistoryRecord::new(
        "codex agent history",
        "Indexed local agent history from /tmp/codex/sessions.jsonl (codex_session_jsonl)",
        vec!["agent-history".into(), "codex".into()],
        "agent_history",
        Some("/tmp/codex".into()),
    );
    record.id = Uuid::parse_str("018f45d0-0000-7000-8005-000000000001").unwrap();
    record.created_at = fixed_time();
    record.updated_at = fixed_time();
    store.upsert_record(&record).unwrap();

    for query in [
        "Indexed local agent history",
        "agent-history",
        "codex_session_jsonl",
    ] {
        let packet = search_packet(&store, query, &PacketOptions::default()).unwrap();
        assert!(
            packet.results.is_empty(),
            "bookkeeping-only query {query:?} returned {:?}",
            packet.results
        );
    }

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8005-000000000002").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("bookkeeping-content-session".into()),
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
        id: Uuid::parse_str("018f45d0-0000-7000-8005-000000000003").unwrap(),
        seq: 1,
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({
            "text": "actual agent-history session evidence"
        }),
        payload_blob_id: None,
        dedupe_key: None,
        redaction_state: RedactionState::SafePreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();

    let packet = search_packet(&store, "agent-history", &PacketOptions::default()).unwrap();
    assert_eq!(packet.results.len(), 1);
    assert_eq!(packet.results[0].event_id, Some(event.id));
    assert!(packet.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "message"));
    assert!(!packet.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "title" || reason == "tag"));
}

#[test]
fn filtered_search_stops_at_scan_budget_when_no_candidates_match() {
    let (_temp, store) = test_store();
    let query = "scan-budget-needle";
    let mut records = Vec::new();
    for index in 0..=(FILTERED_SEARCH_PAGE_SIZE * FILTERED_SEARCH_MAX_PAGES) {
        let mut record = HistoryRecord::new(
            "Scan budget decoy",
            format!("{query} decoy record {index:05}"),
            Vec::new(),
            "task",
            Some("/workspace/no-match".into()),
        );
        record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8000-{index:012x}")).unwrap();
        record.created_at = fixed_time() - chrono::Duration::seconds(index as i64);
        record.updated_at = record.created_at;
        records.push(record);
    }
    store.upsert_records(&records).unwrap();

    let packet = search_packet(
        &store,
        query,
        &PacketOptions {
            limit: 1,
            filters: SearchFilters {
                repo: Some("workspace-that-does-not-exist".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert!(packet.results.is_empty());
    assert!(packet.truncation.truncated);
    assert_eq!(packet.truncation.reason.as_deref(), Some("scan_budget"));
}

#[test]
fn empty_query_filtered_search_returns_empty_without_scanning() {
    let (_temp, store) = test_store();
    let mut records = Vec::new();
    for index in 0..=(FILTERED_SEARCH_PAGE_SIZE * FILTERED_SEARCH_MAX_PAGES) {
        let mut record = HistoryRecord::new(
            "Empty query scan budget decoy",
            format!("empty query decoy record {index:05}"),
            Vec::new(),
            "task",
            Some("/workspace/no-match".into()),
        );
        record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8001-{index:012x}")).unwrap();
        record.created_at = fixed_time() - chrono::Duration::seconds(index as i64);
        record.updated_at = record.created_at;
        records.push(record);
    }
    store.upsert_records(&records).unwrap();

    let packet = search_packet(
        &store,
        "",
        &PacketOptions {
            limit: 1,
            filters: SearchFilters {
                repo: Some("workspace-that-does-not-exist".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert!(packet.results.is_empty());
    assert!(!packet.truncation.truncated);
    assert_eq!(packet.truncation.reason.as_deref(), None);
}

#[test]
fn no_token_query_returns_empty_without_recent_activity() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "No-token query decoy",
        "This record should not be returned for punctuation-only search.",
        Vec::new(),
        "task",
        Some("/workspace/punctuation".into()),
    );
    store.upsert_record(&record).unwrap();

    for query in ["!!!", "---", "___"] {
        let packet =
            search_packet(&store, query, &PacketOptions::default()).expect("search packet");

        assert!(packet.results.is_empty(), "{query}");
        assert!(!packet.truncation.truncated, "{query}");
    }
}

#[test]
fn search_result_limit_is_capped() {
    let (_temp, store) = test_store();
    let query = "limit-cap-needle";
    let mut records = Vec::new();
    for index in 0..250_usize {
        let mut record = HistoryRecord::new(
            "Limit cap candidate",
            format!("{query} candidate {index:03}"),
            Vec::new(),
            "task",
            Some("/workspace/limit-cap".into()),
        );
        record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8002-{index:012x}")).unwrap();
        record.created_at = fixed_time() - chrono::Duration::seconds(index as i64);
        record.updated_at = fixed_time() - chrono::Duration::seconds(index as i64);
        records.push(record);
    }
    store.upsert_records(&records).unwrap();

    let packet = search_packet(
        &store,
        query,
        &PacketOptions {
            limit: usize::MAX,
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), MAX_RESULT_LIMIT);
    assert!(packet.truncation.truncated);
    assert_eq!(packet.truncation.reason.as_deref(), Some("limit"));
}

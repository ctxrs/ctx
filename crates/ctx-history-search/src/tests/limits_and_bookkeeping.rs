use super::*;

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

#[test]
fn filtered_search_scores_full_fetched_page_before_limiting() {
    let (_temp, store) = test_store();
    let query = "samepagerankneedle";
    let workspace = Some("/workspace/same-page-rank".to_owned());
    let mut records = Vec::new();

    for (index, id) in [
        "018f45d0-0000-7000-8000-000000000101",
        "018f45d0-0000-7000-8000-000000000102",
        "018f45d0-0000-7000-8000-000000000103",
    ]
    .into_iter()
    .enumerate()
    {
        let mut record = HistoryRecord::new(
            "Same page filtered candidate",
            format!("{query} identical body for same page ranking"),
            Vec::new(),
            "task",
            workspace.clone(),
        );
        record.id = Uuid::parse_str(id).unwrap();
        record.created_at = fixed_time();
        record.updated_at = fixed_time() + chrono::Duration::seconds(index as i64);
        records.push(record);
    }

    let expected_best_id = records[2].id;
    store.upsert_records(&records).unwrap();

    let late_file_match = FileTouched {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000104").unwrap(),
        history_record_id: Some(expected_best_id),
        run_id: None,
        event_id: None,
        vcs_workspace_id: None,
        path: "crates/search/src/samepagerankneedle.rs".into(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_file_touched(&late_file_match).unwrap();

    let raw_page = store.search_records(query, 3).unwrap();
    assert_eq!(
        raw_page.iter().map(|record| record.id).collect::<Vec<_>>(),
        records.iter().map(|record| record.id).collect::<Vec<_>>(),
        "regression setup must put the best filtered hit after the first limit+1 raw matches"
    );

    let packet = search_packet(
        &store,
        query,
        &PacketOptions {
            limit: 1,
            filters: SearchFilters {
                repo: Some("same-page-rank".into()),
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
        vec![expected_best_id]
    );
    assert!(packet.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "file_touched"));
}

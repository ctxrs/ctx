use super::{
    fixed_time, search_packet, search_packet_terms, sync_metadata, test_store, timestamps,
    Confidence, Event, EventRole, EventType, FileChangeKind, FileTouched, HistoryRecord,
    PacketOptions, SearchFilters, Serialize, Uuid,
};

fn deterministic_tie_record(id: &str) -> HistoryRecord {
    let mut record = HistoryRecord::new(
        "Stable tie title",
        "stabletie exact equal body for deterministic ranking",
        vec!["stabletie".into()],
        "task",
        None,
    );
    record.id = Uuid::parse_str(id).unwrap();
    record.created_at = fixed_time();
    record.updated_at = fixed_time();
    record
}

fn packet_without_generated_at<T: Serialize>(packet: &T) -> serde_json::Value {
    let mut value = serde_json::to_value(packet).unwrap();
    value.as_object_mut().unwrap().remove("generated_at");
    value
}

#[test]
fn candidate_ranking_prefers_messages_and_summaries_and_honors_event_type_filter() {
    let (_temp, store) = test_store();
    let needle = "candidate-event-type-ranking-needle";
    let event_types = [
        EventType::ToolCall,
        EventType::CommandStarted,
        EventType::Message,
        EventType::Summary,
    ];
    for (index, event_type) in event_types.into_iter().enumerate() {
        let mut record = HistoryRecord::new(
            format!("Mixed event record {index}"),
            "agent history record",
            Vec::new(),
            "agent_history",
            None,
        );
        record.id =
            Uuid::parse_str(&format!("018f45d0-0000-7000-8000-00000008{index:04x}")).unwrap();
        store.insert_record(&record).unwrap();
        store
            .upsert_event(&Event {
                id: Uuid::parse_str(&format!("018f45d0-0000-7000-8000-00000009{index:04x}"))
                    .unwrap(),
                seq: index as u64,
                history_record_id: Some(record.id),
                session_id: None,
                run_id: None,
                event_type,
                role: Some(EventRole::Assistant),
                occurred_at: fixed_time(),
                capture_source_id: None,
                payload: serde_json::json!({"text": needle}),
                payload_blob_id: None,
                dedupe_key: None,
                sync: sync_metadata(),
            })
            .unwrap();
    }

    let packet = search_packet(&store, needle, &PacketOptions::default()).unwrap();
    assert_eq!(packet.results.len(), event_types.len());
    assert!(matches!(
        packet.results[0].why_matched.as_slice(),
        [reason] if reason == "message" || reason == "summary"
    ));
    assert!(matches!(
        packet.results[1].why_matched.as_slice(),
        [reason] if reason == "message" || reason == "summary"
    ));

    let filtered = search_packet(
        &store,
        needle,
        &PacketOptions {
            filters: SearchFilters {
                event_type: Some(EventType::ToolCall),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert_eq!(filtered.results.len(), 1);
    assert_eq!(filtered.results[0].why_matched, vec!["tool_call"]);
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

#[test]
fn search_packet_terms_merges_broad_queries_without_requiring_all_terms() {
    let (_temp, store) = test_store();
    for (id, title, body) in [
        (
            "018f45d0-0000-7000-8000-000000020001",
            "Signed metadata release",
            "signed metadata verification and trusted release manifests",
        ),
        (
            "018f45d0-0000-7000-8000-000000020002",
            "Buildkite worker setup",
            "buildkite pipeline worker provisioning and release queue setup",
        ),
    ] {
        let mut record = HistoryRecord::new(title, body, Vec::new(), "task", None);
        record.id = Uuid::parse_str(id).unwrap();
        record.created_at = fixed_time();
        record.updated_at = fixed_time();
        store.insert_record(&record).unwrap();
    }
    let options = PacketOptions {
        limit: 10,
        snippet_chars: 160,
        ..PacketOptions::default()
    };

    let exact = search_packet(&store, "signed metadata buildkite", &options).unwrap();
    assert_eq!(exact.results.len(), 0);

    let broad = search_packet_terms(
        &store,
        "signed metadata",
        &[String::from("buildkite")],
        &options,
    )
    .unwrap();
    let titles = broad
        .results
        .iter()
        .map(|result| result.title.as_str())
        .collect::<Vec<_>>();
    assert!(titles.contains(&"Signed metadata release"));
    assert!(titles.contains(&"Buildkite worker setup"));
    assert_eq!(broad.query, "signed metadata OR buildkite");
}

#[test]
fn search_packet_is_deterministic_for_large_history_and_equal_ties_use_record_id() {
    let (_temp, store) = test_store();
    for id in [
        "018f45d0-0000-7000-8000-000000010004",
        "018f45d0-0000-7000-8000-000000010001",
        "018f45d0-0000-7000-8000-000000010003",
        "018f45d0-0000-7000-8000-000000010002",
    ] {
        store.insert_record(&deterministic_tie_record(id)).unwrap();
    }

    let expected_order = vec![
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010001").unwrap(),
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010002").unwrap(),
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010003").unwrap(),
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010004").unwrap(),
    ];
    let options = PacketOptions {
        limit: 10,
        snippet_chars: 160,
        ..PacketOptions::default()
    };

    let first_search = search_packet(&store, "stabletie", &options).unwrap();
    let second_search = search_packet(&store, "stabletie", &options).unwrap();
    assert_eq!(
        first_search
            .results
            .iter()
            .map(|result| result.record_id)
            .collect::<Vec<_>>(),
        expected_order
    );
    assert_eq!(
        packet_without_generated_at(&first_search),
        packet_without_generated_at(&second_search)
    );
}

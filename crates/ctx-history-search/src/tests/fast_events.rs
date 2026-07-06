use super::*;

#[test]
fn large_agent_history_search_returns_event_hits() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Large provider history",
        "single imported agent-history record",
        Vec::new(),
        "agent_history",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&record).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000601").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("large-history-session".into()),
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

    let other_record = HistoryRecord::new(
        "Large provider history shard",
        "another imported agent-history record",
        Vec::new(),
        "agent_history",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&other_record).unwrap();
    let other_session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000602").unwrap(),
        history_record_id: Some(other_record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("large-history-session-shard".into()),
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
    store.upsert_session(&other_session).unwrap();

    let target_event_id = Uuid::parse_str("018f45d0-0000-7000-8000-0000000006ff").unwrap();
    for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
        let (event_record_id, event_session) = if index < 512 {
            (other_record.id, other_session.id)
        } else {
            (record.id, session.id)
        };
        let event_id = if index == LARGE_EVENT_CORPUS_THRESHOLD as u64 {
            target_event_id
        } else {
            let mut bytes = *event_session.as_bytes();
            bytes[14] = (index / 256) as u8;
            bytes[15] = index as u8;
            Uuid::from_bytes(bytes)
        };
        let text = if event_id == target_event_id {
            "large-fast-event-needle from one transcript"
        } else {
            "ordinary large history event"
        };
        store
            .upsert_event(&Event {
                id: event_id,
                seq: 10_000 + index,
                history_record_id: Some(event_record_id),
                session_id: Some(event_session),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::Assistant),
                occurred_at: fixed_time() + chrono::Duration::milliseconds(index as i64),
                capture_source_id: None,
                payload: serde_json::json!({
                    "cursor": format!("line:{index}"),
                    "body": { "text": text }
                }),
                payload_blob_id: None,
                dedupe_key: Some(format!("large-history-{index}")),
                redaction_state: RedactionState::SafePreview,
                sync: sync_metadata(),
            })
            .unwrap();
    }
    store.refresh_search_index().unwrap();

    let packet = search_packet(
        &store,
        "large-fast-event-needle",
        &PacketOptions {
            limit: 5,
            snippet_chars: 200,
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    let result = &packet.results[0];
    assert_eq!(result.result_scope, SearchResultScope::Session);
    assert_eq!(result.record_id, target_event_id);
    assert_eq!(result.event_id, Some(target_event_id));
    assert_eq!(result.session_id, Some(session.id));
    assert_eq!(result.provider, Some(CaptureProvider::Codex));
    assert_eq!(
        result.snippet,
        "large-fast-event-needle from one transcript"
    );
    assert_eq!(result.why_matched, vec!["message"]);
    assert!(result.citations.iter().any(|citation| {
        citation.citation_type == ContextCitationType::Event
            && citation.id == target_event_id
            && citation.cursor.as_deref() == Some("line:1024")
    }));

    let event_packet = search_packet(
        &store,
        "large-fast-event-needle",
        &PacketOptions {
            limit: 5,
            snippet_chars: 200,
            result_mode: SearchResultMode::Events,
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert_eq!(event_packet.results.len(), 1);
    assert_eq!(
        event_packet.results[0].result_scope,
        SearchResultScope::Event
    );
    assert_eq!(event_packet.results[0].event_id, Some(target_event_id));
}

#[test]
fn clustered_fast_search_pages_past_dominant_first_session() {
    let (_temp, store) = test_store();
    let dominant_record = HistoryRecord::new(
        "Dominant matching session",
        "dominant record",
        Vec::new(),
        "agent_history",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&dominant_record).unwrap();
    let dominant_session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000701").unwrap(),
        history_record_id: Some(dominant_record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("dominant-session".into()),
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
    store.upsert_session(&dominant_session).unwrap();

    let later_record = HistoryRecord::new(
        "Later matching session",
        "later record",
        Vec::new(),
        "agent_history",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&later_record).unwrap();
    let later_session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000702").unwrap(),
        history_record_id: Some(later_record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("later-session".into()),
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
    store.upsert_session(&later_session).unwrap();

    for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
        let (record_id, session_id, text, occurred_at) = if index < 600 {
            (
                dominant_record.id,
                dominant_session.id,
                "cluster-paging-needle dominant hit",
                fixed_time() + chrono::Duration::milliseconds(2_000 - index as i64),
            )
        } else if index == 600 {
            (
                later_record.id,
                later_session.id,
                "cluster-paging-needle later hit",
                fixed_time(),
            )
        } else {
            (
                dominant_record.id,
                dominant_session.id,
                "ordinary large history event",
                fixed_time() - chrono::Duration::milliseconds(index as i64),
            )
        };
        store
            .upsert_event(&Event {
                id: Uuid::parse_str(&format!("018f45d0-0000-7000-8000-0000001{index:05x}"))
                    .unwrap(),
                seq: 20_000 + index,
                history_record_id: Some(record_id),
                session_id: Some(session_id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::Assistant),
                occurred_at,
                capture_source_id: None,
                payload: serde_json::json!({
                    "cursor": format!("line:{index}"),
                    "body": { "text": text }
                }),
                payload_blob_id: None,
                dedupe_key: Some(format!("clustered-paging-{index}")),
                redaction_state: RedactionState::SafePreview,
                sync: sync_metadata(),
            })
            .unwrap();
    }
    store.refresh_search_index().unwrap();

    let packet = search_packet(
        &store,
        "cluster-paging-needle",
        &PacketOptions {
            limit: 2,
            snippet_chars: 200,
            ..PacketOptions::default()
        },
    )
    .unwrap();
    let sessions = packet
        .results
        .iter()
        .filter_map(|result| result.session_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(packet.results.len(), 2);
    assert!(sessions.contains(&dominant_session.id));
    assert!(sessions.contains(&later_session.id));
    assert!(!packet.truncation.truncated);
}

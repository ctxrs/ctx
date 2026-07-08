use super::{
    fixed_time, search_packet, sync_metadata, test_store, timestamps, AgentType, BTreeSet,
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence,
    ContextCitationType, Event, EventRole, EventType, FileChangeKind, FileTouched, HistoryRecord,
    PacketOptions, SearchFilters, SearchResultMode, SearchResultScope, Session, SessionStatus,
    SyncMetadata, Uuid, LARGE_EVENT_CORPUS_THRESHOLD,
};

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
fn large_event_fast_search_recalls_cjk_preview_term() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Large multilingual provider history",
        "single imported agent-history record",
        Vec::new(),
        "agent_history",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&record).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000611").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("large-multilingual-history-session".into()),
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

    let target_event_id = Uuid::parse_str("018f45d0-0000-7000-8000-0000000006fe").unwrap();
    for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
        let event_id = if index == LARGE_EVENT_CORPUS_THRESHOLD as u64 {
            target_event_id
        } else {
            Uuid::parse_str(&format!("018f45d0-0000-7000-8000-0000003{index:05x}")).unwrap()
        };
        let text = if event_id == target_event_id {
            "OAuth認証の検索状態をfast path previewから見つける"
        } else {
            "ordinary large multilingual history event"
        };
        store
            .upsert_event(&Event {
                id: event_id,
                seq: 50_000 + index,
                history_record_id: Some(record.id),
                session_id: Some(session.id),
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
                dedupe_key: Some(format!("large-multilingual-history-{index}")),
                sync: sync_metadata(),
            })
            .unwrap();
    }
    store.refresh_search_index().unwrap();

    let packet = search_packet(
        &store,
        "認証",
        &PacketOptions {
            limit: 5,
            snippet_chars: 200,
            result_mode: SearchResultMode::Events,
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert!(
        !packet.results.is_empty(),
        "large fast-path search should not return zero results for a literal CJK preview term"
    );
    assert!(packet
        .results
        .iter()
        .any(|result| result.event_id == Some(target_event_id)));
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
        let (record_id, session_id, text, occurred_at) = match index.cmp(&600) {
            std::cmp::Ordering::Less => (
                dominant_record.id,
                dominant_session.id,
                "cluster-paging-needle dominant hit",
                fixed_time() + chrono::Duration::milliseconds(2_000 - index as i64),
            ),
            std::cmp::Ordering::Equal => (
                later_record.id,
                later_session.id,
                "cluster-paging-needle later hit",
                fixed_time(),
            ),
            std::cmp::Ordering::Greater => (
                dominant_record.id,
                dominant_session.id,
                "ordinary large history event",
                fixed_time() - chrono::Duration::milliseconds(index as i64),
            ),
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

#[test]
fn fast_event_search_exposes_custom_history_source_identity() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Large custom plugin import",
        "ordinary body",
        Vec::new(),
        "agent_history",
        Some("/workspace/custom".into()),
    );
    store.insert_record(&record).unwrap();

    let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000481").unwrap();
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Custom,
                machine_id: "machine-1".into(),
                process_id: None,
                cwd: Some("/workspace/custom".into()),
                raw_source_path: Some("/tmp/large-dorkos/ctx-history-plugin.json".into()),
                source_format: Some("ctx-history-jsonl-v1".into()),
                source_root: Some("/tmp/large-dorkos".into()),
                source_identity: None,
                external_session_id: Some("ctx-history-jsonl-v1-large".into()),
            },
            started_at: fixed_time(),
            ended_at: None,
            sync: SyncMetadata {
                metadata: serde_json::json!({
                    "source_metadata": {
                        "ctx_history_plugin": {
                            "plugin_name": "dorkos",
                            "plugin_source_id": "default",
                            "history_source": "dorkos/default"
                        },
                        "ctx_history_jsonl_v1": {
                            "provider_key": "dorkos",
                            "source_id": "default",
                            "source_format": "dorkos-history-v1"
                        }
                    }
                }),
                ..sync_metadata()
            },
        })
        .unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000482").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Custom,
        external_session_id: Some("ctx-history-jsonl-v1-large".into()),
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

    let target_event_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000483").unwrap();
    for index in 0..=(LARGE_EVENT_CORPUS_THRESHOLD as u64) {
        let event_id = if index == LARGE_EVENT_CORPUS_THRESHOLD as u64 {
            target_event_id
        } else {
            Uuid::parse_str(&format!("018f45d0-0000-7000-8000-0000002{index:05x}")).unwrap()
        };
        let text = if event_id == target_event_id {
            "large-custom-source-identity-needle"
        } else {
            "ordinary large custom event"
        };
        store
            .upsert_event(&Event {
                id: event_id,
                seq: 40_000 + index,
                history_record_id: Some(record.id),
                session_id: Some(session.id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::Assistant),
                occurred_at: fixed_time() + chrono::Duration::milliseconds(index as i64),
                capture_source_id: Some(source_id),
                payload: serde_json::json!({
                    "body": { "text": text }
                }),
                payload_blob_id: None,
                dedupe_key: Some(format!("large-custom-source-identity-{index}")),
                sync: sync_metadata(),
            })
            .unwrap();
    }
    store.refresh_search_index().unwrap();

    let packet = search_packet(
        &store,
        "large-custom-source-identity-needle",
        &PacketOptions {
            limit: 5,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Custom),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    let result = &packet.results[0];
    assert_eq!(result.event_id, Some(target_event_id));
    assert_eq!(result.history_source.as_deref(), Some("dorkos/default"));
    assert_eq!(result.history_source_plugin.as_deref(), Some("dorkos"));
    assert_eq!(result.provider_key.as_deref(), Some("dorkos"));
    assert_eq!(result.source_id.as_deref(), Some("default"));
    assert_eq!(result.source_format.as_deref(), Some("dorkos-history-v1"));
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

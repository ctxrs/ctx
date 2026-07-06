use super::{
    fixed_time, search_packet, sync_metadata, test_store, timestamps, AgentType, CaptureProvider,
    CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence, ContextCitationType,
    Event, EventRole, EventType, FileChangeKind, FileTouched, HistoryRecord, PacketOptions,
    RedactionState, SearchFilters, Session, SessionStatus, SyncMetadata, Uuid,
};

#[test]
fn search_filters_and_citations_expose_source_metadata() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Source-backed session",
        "ordinary body",
        Vec::new(),
        "session",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&record).unwrap();

    let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000401").unwrap();
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine-1".into(),
            process_id: None,
            cwd: Some("/workspace/ctx".into()),
            raw_source_path: Some("/definitely/missing/source-filter.jsonl".into()),
            source_format: Some("codex_session_jsonl".into()),
            source_root: Some("/definitely/missing".into()),
            source_identity: None,
            external_session_id: Some("source-filter-session".into()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: SyncMetadata {
            metadata: serde_json::json!({
                "source_format": "codex_session_jsonl",
                "cursor": {
                    "after": {
                        "stream": "provider:codex:codex_session_jsonl",
                        "cursor": "line:8",
                        "observed_at": "2026-06-23T12:00:00Z"
                    }
                }
            }),
            ..sync_metadata()
        },
    };
    store.upsert_capture_source(&source).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000402").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some("source-filter-session".into()),
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
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000403").unwrap(),
        seq: 401,
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        run_id: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: Some(source_id),
        payload: serde_json::json!({
            "cursor": "line:8",
            "body": {
                "tool": "shell",
                "name": "exec_command",
                "arguments_preview": "source-filter-needle"
            }
        }),
        payload_blob_id: None,
        dedupe_key: Some("source-filter-event".into()),
        redaction_state: RedactionState::SafePreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();

    let file = FileTouched {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000404").unwrap(),
        history_record_id: Some(record.id),
        run_id: None,
        event_id: Some(event.id),
        vcs_workspace_id: None,
        path: "crates/search/src/source_filter.rs".into(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: Some(source_id),
        sync: sync_metadata(),
    };
    store.upsert_file_touched(&file).unwrap();
    store.upsert_record(&record).unwrap();

    let packet = search_packet(
        &store,
        "source-filter-needle",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Codex),
                repo: Some("ctx".into()),
                event_type: Some(EventType::ToolCall),
                file: Some("source_filter.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    let result = &packet.results[0];
    assert_eq!(result.provider, Some(CaptureProvider::Codex));
    assert_eq!(result.session_id, Some(session.id));
    assert_eq!(result.event_id, Some(event.id));
    assert_eq!(result.event_seq, Some(401));
    assert_eq!(
        result.raw_source_path.as_deref(),
        source.descriptor.raw_source_path.as_deref()
    );
    assert_eq!(result.raw_source_exists, Some(false));
    assert_eq!(result.cursor.as_deref(), Some("line:8"));
    assert!(result.citations.iter().any(|citation| {
        citation.citation_type == ContextCitationType::Event
            && citation.raw_source_path.as_deref() == source.descriptor.raw_source_path.as_deref()
            && citation.raw_source_exists == Some(false)
            && citation.cursor.as_deref() == Some("line:8")
    }));

    let file_only = search_packet(
        &store,
        "",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Codex),
                file: Some("source_filter.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert_eq!(file_only.results.len(), 1);
    assert!(file_only.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "file_touched"));
    assert!(!file_only.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "recent_activity"));
    assert!(file_only.results[0].citations.iter().any(|citation| {
        citation.citation_type == ContextCitationType::File && citation.id == file.id
    }));

    let wrong_provider = search_packet(
        &store,
        "source-filter-needle",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Pi),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert!(wrong_provider.results.is_empty());
}

#[test]
fn source_only_results_do_not_fallback_to_other_source_scoped_sessions() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Source collision import",
        "ordinary body",
        Vec::new(),
        "session",
        Some("/workspace/ctx".into()),
    );
    store.insert_record(&record).unwrap();

    let external_session_id = "reused-provider-session";
    let first_source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000441").unwrap();
    let second_source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000442").unwrap();
    for (source_id, raw_source_path, source_root) in [
        (
            first_source_id,
            "/tmp/source-collision/first/session.jsonl",
            "/tmp/source-collision/first",
        ),
        (
            second_source_id,
            "/tmp/source-collision/second/session.jsonl",
            "/tmp/source-collision/second",
        ),
    ] {
        store
            .upsert_capture_source(&CaptureSource {
                id: source_id,
                descriptor: CaptureSourceDescriptor {
                    kind: CaptureSourceKind::ProviderImport,
                    provider: CaptureProvider::Codex,
                    machine_id: "machine-1".into(),
                    process_id: None,
                    cwd: Some("/workspace/ctx".into()),
                    raw_source_path: Some(raw_source_path.into()),
                    source_format: Some("codex_session_jsonl".into()),
                    source_root: Some(source_root.into()),
                    source_identity: None,
                    external_session_id: Some(external_session_id.into()),
                },
                started_at: fixed_time(),
                ended_at: None,
                sync: sync_metadata(),
            })
            .unwrap();
    }

    let first_session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000443").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(first_source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some(external_session_id.into()),
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
    store.upsert_session(&first_session).unwrap();

    let file = FileTouched {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000444").unwrap(),
        history_record_id: Some(record.id),
        run_id: None,
        event_id: None,
        vcs_workspace_id: None,
        path: "src/source_collision_second.rs".into(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: Some(second_source_id),
        sync: sync_metadata(),
    };
    store.upsert_file_touched(&file).unwrap();

    let packet = search_packet(
        &store,
        "",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Codex),
                file: Some("source_collision_second.rs".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    let result = &packet.results[0];
    assert_eq!(result.session_id, None);
    assert_eq!(
        result.raw_source_path.as_deref(),
        Some("/tmp/source-collision/second/session.jsonl")
    );
}

#[test]
fn search_filters_custom_history_source_identity() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Custom plugin import",
        "ordinary body",
        Vec::new(),
        "session",
        Some("/workspace/custom".into()),
    );
    store.insert_record(&record).unwrap();

    let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000451").unwrap();
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Custom,
            machine_id: "machine-1".into(),
            process_id: None,
            cwd: Some("/workspace/custom".into()),
            raw_source_path: Some("/tmp/dorkos-plugin/ctx-history-plugin.json".into()),
            source_format: Some("ctx-history-jsonl-v1".into()),
            source_root: Some("/tmp/dorkos-plugin".into()),
            source_identity: None,
            external_session_id: Some("ctx-history-jsonl-v1-session".into()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: SyncMetadata {
            metadata: serde_json::json!({
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
            }),
            ..sync_metadata()
        },
    };
    store.upsert_capture_source(&source).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000452").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Custom,
        external_session_id: Some("ctx-history-jsonl-v1-session".into()),
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
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000453").unwrap(),
        seq: 451,
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: Some(source_id),
        payload: serde_json::json!({
            "body": {
                "text": "dorkos-source-filter-needle"
            }
        }),
        payload_blob_id: None,
        dedupe_key: Some("custom-history-source-filter-event".into()),
        redaction_state: RedactionState::SafePreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();
    store.upsert_record(&record).unwrap();

    let packet = search_packet(
        &store,
        "dorkos-source-filter-needle",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Custom),
                history_source: Some("dorkos/default".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    let result = &packet.results[0];
    assert_eq!(result.provider, Some(CaptureProvider::Custom));
    assert_eq!(result.history_source.as_deref(), Some("dorkos/default"));
    assert_eq!(result.history_source_plugin.as_deref(), Some("dorkos"));
    assert_eq!(result.provider_key.as_deref(), Some("dorkos"));
    assert_eq!(result.source_id.as_deref(), Some("default"));
    assert_eq!(result.source_format.as_deref(), Some("dorkos-history-v1"));

    let provider_source_packet = search_packet(
        &store,
        "dorkos-source-filter-needle",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Custom),
                provider_key: Some("dorkos".into()),
                source_id: Some("default".into()),
                source_format: Some("dorkos-history-v1".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert_eq!(provider_source_packet.results.len(), 1);

    let wrong_source = search_packet(
        &store,
        "dorkos-source-filter-needle",
        &PacketOptions {
            limit: 10,
            filters: SearchFilters {
                provider: Some(CaptureProvider::Custom),
                history_source: Some("openclaw/default".into()),
                ..SearchFilters::default()
            },
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert!(wrong_source.results.is_empty());
}

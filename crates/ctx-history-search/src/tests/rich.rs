use super::{
    fixed_time, maybe_write_synthetic_search_smoke_artifact, new_link_id, search_packet,
    sync_metadata, test_store, timestamps, AgentType, Artifact, ArtifactKind, CaptureProvider,
    Confidence, ContextCitationType, Event, EventRole, EventType, FileChangeKind, FileTouched,
    HistoryRecord, HistoryRecordLink, HistoryRecordLinkTargetType, HistoryRecordLinkType,
    PacketOptions, RedactionState, Run, RunStatus, RunType, Session, SessionStatus, Summary,
    SummaryKind, Uuid, VcsChange, VcsChangeKind, VcsHost, VcsKind, VcsWorkspace, Visibility,
};

#[test]
fn rich_search_matches_typed_metadata_with_citations_and_redaction() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Plain work",
        "ordinary body without the query",
        vec!["needle-tag".into()],
        "task",
        None,
    );
    store.insert_record(&record).unwrap();

    let artifact = Artifact {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000201").unwrap(),
        kind: ArtifactKind::Markdown,
        blob_hash: "hash-rich-search-artifact".into(),
        blob_path: "blobs/rich-search-artifact".into(),
        byte_size: 32,
        media_type: Some("text/markdown".into()),
        preview_text: Some("needle-artifact /home/example/private/repo".into()),
        redaction_state: RedactionState::SafePreview,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_artifact(&artifact).unwrap();

    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000202").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("needle-session".into()),
        external_agent_id: Some("agent-needle".into()),
        agent_type: AgentType::Primary,
        role_hint: Some("needle-role".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    };
    store.upsert_session(&session).unwrap();

    let run = Run {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000203").unwrap(),
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        run_type: RunType::Command,
        status: RunStatus::Failed,
        started_at: fixed_time(),
        ended_at: Some(fixed_time()),
        exit_code: Some(1),
        cwd: Some("/home/example/private/repo".into()),
        command_preview: Some("cargo test needle-run password=hunter2".into()),
        input_blob_id: None,
        output_blob_id: Some(artifact.id),
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_run(&run).unwrap();

    let event = Event {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000204").unwrap(),
        seq: 1,
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        run_id: Some(run.id),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({
            "tool": "shell",
            "arguments": "needle-event token=secretvalue"
        }),
        payload_blob_id: None,
        dedupe_key: Some("needle-dedupe".into()),
        redaction_state: RedactionState::SafePreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();

    let workspace = VcsWorkspace {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000205").unwrap(),
        kind: VcsKind::Git,
        root_path: "/repo".into(),
        repo_fingerprint: "git:needle".into(),
        primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
        host: VcsHost::Github,
        owner: Some("ctxrs".into()),
        name: Some("ctx".into()),
        monorepo_subpath: None,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();

    let change = VcsChange {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000206").unwrap(),
        vcs_workspace_id: workspace_id,
        kind: VcsChangeKind::GitCommit,
        change_id: "needle-change".into(),
        parent_change_ids: vec!["parent".into()],
        branch_or_bookmark: Some("ctx/needle-branch".into()),
        tree_hash: Some("tree".into()),
        author_time: Some(fixed_time()),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_vcs_change(&change).unwrap();

    let file = FileTouched {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000208").unwrap(),
        history_record_id: Some(record.id),
        run_id: Some(run.id),
        event_id: Some(event.id),
        vcs_workspace_id: Some(workspace_id),
        path: "crates/ctx-history-search/src/needle_file.rs".into(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(12),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_file_touched(&file).unwrap();

    let summary = Summary {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000209").unwrap(),
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        kind: SummaryKind::ImportedProviderSummary,
        model_or_source: Some("codex".into()),
        text: "needle summary password=hunter2".into(),
        citations: Vec::new(),
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    };
    store.upsert_summary(&summary).unwrap();

    for (target_type, target_id, link_type) in [
        (
            HistoryRecordLinkTargetType::VcsChange,
            change.id,
            HistoryRecordLinkType::References,
        ),
        (
            HistoryRecordLinkTargetType::Artifact,
            artifact.id,
            HistoryRecordLinkType::Produced,
        ),
    ] {
        store
            .upsert_history_record_link(&HistoryRecordLink {
                id: new_link_id(target_id),
                history_record_id: record.id,
                target_type,
                target_id,
                link_type,
                confidence: Confidence::Explicit,
                source_id: None,
                timestamps: timestamps(),
                sync: sync_metadata(),
            })
            .unwrap();
    }

    let packet = search_packet(
        &store,
        "needle",
        &PacketOptions {
            limit: 5,
            snippet_chars: 600,
            ..PacketOptions::default()
        },
    )
    .unwrap();

    assert_eq!(packet.results.len(), 1);
    let result = &packet.results[0];
    for reason in [
        "tag",
        "session_metadata",
        "run_command",
        "tool_call",
        "artifact",
        "file_touched",
        "vcs_change",
        "summary",
    ] {
        assert!(
            result.why_matched.iter().any(|value| value == reason),
            "missing why_matched reason {reason}: {:?}",
            result.why_matched
        );
    }
    for removed_reason in ["failed_command", "failed_evidence_output", "pull_request"] {
        assert!(
            !result
                .why_matched
                .iter()
                .any(|value| value == removed_reason),
            "removed reason {removed_reason} leaked into search result: {:?}",
            result.why_matched
        );
    }

    for citation_type in [
        ContextCitationType::HistoryRecord,
        ContextCitationType::Session,
        ContextCitationType::Run,
        ContextCitationType::Event,
        ContextCitationType::Artifact,
        ContextCitationType::File,
        ContextCitationType::VcsChange,
        ContextCitationType::Summary,
    ] {
        assert!(
            result
                .citations
                .iter()
                .any(|citation| citation.citation_type == citation_type),
            "missing citation type {citation_type:?}: {:?}",
            result.citations
        );
    }
    assert_eq!(result.visibility, Visibility::LocalOnly);
    assert!(!result.snippet.contains("hunter2"));
    assert!(!result.snippet.contains("ghp_123456"));
    assert!(!result.snippet.contains("secretvalue"));

    let secret_packet = search_packet(
        &store,
        "hunter2",
        &PacketOptions {
            limit: 1,
            snippet_chars: 600,
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert!(secret_packet.results.is_empty());

    maybe_write_synthetic_search_smoke_artifact();
}

#[test]
fn nested_provider_body_event_preview_drives_search() {
    let (_temp, store) = test_store();
    let record = HistoryRecord::new(
        "Provider event record",
        "ordinary body without event query",
        Vec::new(),
        "task",
        None,
    );
    store.insert_record(&record).unwrap();
    let session = Session {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000301").unwrap(),
        history_record_id: Some(record.id),
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some("codex-session".into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("worker".into()),
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
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000302").unwrap(),
        seq: 1,
        history_record_id: Some(record.id),
        session_id: Some(session.id),
        run_id: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({
            "provider": "codex",
            "body": {
                "tool": "shell",
                "name": "exec_command",
                "arguments_preview": "nested-search-needle token=secretvalue",
                "arguments": "unsafe-raw-needle password=hunter2"
            }
        }),
        payload_blob_id: None,
        dedupe_key: Some("nested-provider-event".into()),
        redaction_state: RedactionState::SafePreview,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();
    store.upsert_record(&record).unwrap();

    let packet = search_packet(
        &store,
        "nested-search-needle",
        &PacketOptions {
            limit: 5,
            snippet_chars: 600,
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert_eq!(packet.results.len(), 1);
    assert!(packet.results[0]
        .why_matched
        .iter()
        .any(|reason| reason == "tool_call"));
    assert!(packet.results[0]
        .snippet
        .contains("arguments_preview: nested-search-needle token=secretvalue"));
    assert!(!packet.results[0].snippet.contains("unsafe-raw-needle"));
    assert!(!packet.results[0].snippet.contains("hunter2"));

    let unsafe_packet = search_packet(
        &store,
        "unsafe-raw-needle",
        &PacketOptions {
            limit: 5,
            snippet_chars: 600,
            ..PacketOptions::default()
        },
    )
    .unwrap();
    assert!(unsafe_packet.results.is_empty());
}

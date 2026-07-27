use super::*;
use ctx_history_core::{
    new_id, AgentType, CaptureProvider, EntityTimestamps, Event, EventRole, EventType, Fidelity,
    Session, SessionStatus, SyncMetadata, SyncState, Visibility,
};

fn test_embedding(first: f32, second: f32) -> Vec<f32> {
    let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
    embedding[0] = first;
    embedding[1] = second;
    embedding
}

fn test_chunk(event_id: Uuid, seq: u64, source_hash: &str) -> SemanticChunkDocument {
    test_chunk_at(event_id, seq, source_hash, 0, 1)
}

fn test_daemon_run_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: Some(1),
        max_seconds: Some(1),
        force: false,
        start_mode: Some(DaemonStartModeArg::Manual),
        trigger_command: None,
        json: true,
    }
}

fn write_semantic_enabled_config(data_root: &Path) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let path = data_root.join(CONFIG_FILE);
    fs::write(
        path,
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )?;
    Ok(())
}

fn test_sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: json!({}),
    }
}

fn test_searchable_event(seq: u64) -> Event {
    Event {
        id: new_id(),
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: utc_now(),
        capture_source_id: None,
        payload: json!({ "text": format!("semantic daemon scheduling fixture {seq}") }),
        payload_blob_id: None,
        dedupe_key: None,
        sync: test_sync_metadata(),
    }
}

fn insert_test_session(store: &Store, session_id: Uuid) -> Result<()> {
    let now = utc_now();
    store.upsert_session(&Session {
        id: session_id,
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some(format!("session-{session_id}")),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: now,
        ended_at: None,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        sync: test_sync_metadata(),
    })?;
    Ok(())
}

fn test_session_message(seq: u64, session_id: Uuid, role: EventRole, text: &str) -> Event {
    let mut event = test_searchable_event(seq);
    event.session_id = Some(session_id);
    event.role = Some(role);
    event.payload = json!({ "text": text });
    event
}

fn write_searchable_store(data_root: &Path, count: usize) -> Result<Vec<EventEmbeddingDocument>> {
    fs::create_dir_all(data_root)?;
    let store = Store::open(database_path(data_root.to_path_buf()))?;
    for seq in 1..=count {
        store.upsert_event(&test_searchable_event(seq as u64))?;
    }
    store.refresh_event_embedding_document_count_cache()?;
    let docs = store.recent_event_embedding_documents(None, count)?;
    assert_eq!(docs.len(), count);
    Ok(docs)
}

fn write_late_activity_searchable_store(
    data_root: &Path,
    count: usize,
) -> Result<Vec<EventEmbeddingDocument>> {
    fs::create_dir_all(data_root)?;
    let store = Store::open(database_path(data_root.to_path_buf()))?;
    let base = utc_now() - chrono::Duration::days(30);
    let late_activity = utc_now() + chrono::Duration::days(30);
    store.begin_immediate_batch()?;
    for index in 0..count {
        let session_id = Uuid::new_v4();
        insert_test_session(&store, session_id)?;
        let user_seq = index as u64 * 2 + 1;
        let mut user = test_session_message(
            user_seq,
            session_id,
            EventRole::User,
            &format!("paged semantic prompt {index}"),
        );
        user.occurred_at = base + chrono::Duration::minutes(index as i64);
        let mut assistant = test_session_message(
            user_seq + 1,
            session_id,
            EventRole::Assistant,
            &format!("late semantic answer {index}"),
        );
        assistant.occurred_at = late_activity;
        store.upsert_event(&user)?;
        store.upsert_event(&assistant)?;
    }
    store.commit_batch()?;
    store.refresh_event_embedding_document_count_cache()?;
    let docs = store.recent_event_embedding_documents(None, count)?;
    assert_eq!(docs.len(), count);
    assert!(docs
        .iter()
        .all(|doc| doc.occurred_at_ms == late_activity.timestamp_millis()));
    Ok(docs)
}

fn daemon_history_completed_test_job() -> Value {
    daemon_history_refresh_job_json(
        "completed",
        1,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        None,
        None,
    )
}

fn daemon_semantic_indexed_test_job(data_root: &Path) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "budget_exhausted",
        None,
        utc_now().timestamp_millis(),
        &report,
        Some(1),
        None,
    )
}

fn install_test_daemon_jobs(
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    history_refresh: Option<Value>,
    semantic_index: Option<Value>,
) -> DaemonTestJobHookGuard {
    install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls,
        history_refresh,
        semantic_index,
    })
}

fn test_chunk_at(
    event_id: Uuid,
    seq: u64,
    source_hash: &str,
    chunk_index: usize,
    chunk_count: usize,
) -> SemanticChunkDocument {
    SemanticChunkDocument {
        event_id,
        history_record_id: None,
        session_id: None,
        seq,
        chunk_index,
        chunk_count,
        source_text_hash: source_hash.to_owned(),
        chunk_text_hash: format!("{source_hash}-chunk-{chunk_index}"),
        text: String::new(),
        start_char: chunk_index.saturating_mul(10),
        end_char: chunk_index.saturating_mul(10).saturating_add(12),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn write_test_semantic_cache(root: &Path) -> Result<()> {
    let snapshot = root
        .join(SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION);
    fs::create_dir_all(&snapshot)?;
    for file in SEMANTIC_REQUIRED_MODEL_FILES {
        let path = snapshot.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::File::create(path)?.set_len(file.size)?;
    }
    Ok(())
}

mod lifecycle;
mod locking;
mod search_daemon;
mod vector_store;
mod workflow;

#[path = "daemon_history_followup_tests.rs"]
mod daemon_history_followup_tests;

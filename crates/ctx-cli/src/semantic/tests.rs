#[cfg(all(test, ctx_sqlite_vec))]
mod tests {
    use super::*;
    use ctx_history_core::{
        new_id, AgentType, CaptureProvider, EntityTimestamps, Event, EventRole, EventType,
        Fidelity, Session, SessionStatus, SyncMetadata, SyncState, Visibility,
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

    fn write_searchable_store(
        data_root: &Path,
        count: usize,
    ) -> Result<Vec<EventEmbeddingDocument>> {
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
            .join("test-snapshot");
        fs::create_dir_all(&snapshot)?;
        fs::create_dir_all(root.join(SEMANTIC_HF_MODEL_CACHE_DIR).join("refs"))?;
        fs::write(
            root.join(SEMANTIC_HF_MODEL_CACHE_DIR)
                .join("refs")
                .join("main"),
            "test-snapshot\n",
        )?;
        for file in SEMANTIC_REQUIRED_MODEL_FILES {
            fs::write(snapshot.join(file), b"test")?;
        }
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn semantic_adaptive_policy_uses_one_memory_budget_formula() {
        let gib = 1024 * 1024 * 1024;
        let large = SemanticMemorySnapshot {
            total_bytes: Some(64 * gib),
            available_bytes: Some(32 * gib),
        };
        let small = SemanticMemorySnapshot {
            total_bytes: Some(8 * gib),
            available_bytes: Some(4 * gib),
        };
        let constrained = SemanticMemorySnapshot {
            total_bytes: Some(64 * gib),
            available_bytes: Some(3 * gib),
        };

        assert_eq!(
            semantic_adaptive_memory_budget_bytes(large),
            SEMANTIC_MEMORY_BUDGET_MAX_BYTES
        );
        assert_eq!(semantic_adaptive_embed_policy(large).batch_size, 128);
        assert_eq!(
            semantic_adaptive_memory_budget_bytes(small),
            1_717_986_918
        );
        assert_eq!(semantic_adaptive_embed_policy(small).batch_size, 16);
        assert_eq!(
            semantic_adaptive_memory_budget_bytes(constrained),
            1_610_612_736
        );
        assert_eq!(semantic_adaptive_embed_policy(constrained).batch_size, 16);
    }

    #[test]
    fn semantic_worker_report_preserves_embed_policy_from_status() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_worker_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "budget_exhausted",
                "model_key": SEMANTIC_MODEL_KEY,
                "pid": 1234,
                "searchable_items": 10,
                "embedded_items": 2,
                "embedded_chunks": 4,
                "dirty_items": 1,
                "embed_policy": {
                    "source": "fixture",
                    "threads": 7,
                    "batch_size": 96,
                    "memory_budget_bytes": 123,
                },
            }),
        )?;

        let report = semantic_worker_report_best_effort(temp.path()).to_json();
        assert_eq!(report["embed_policy"]["source"], "fixture");
        assert_eq!(report["embed_policy"]["threads"], 7);
        assert_eq!(report["coverage"]["embedded_chunks"], 4);
        Ok(())
    }

    #[test]
    fn semantic_worker_report_ignores_status_from_old_model_key() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_worker_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "ready",
                "model_key": "fastembed:old-model-key",
                "pid": 999,
                "last_error": "old failure",
                "searchable_items": 10,
                "embedded_items": 10,
                "embedded_chunks": 20,
                "dirty_items": 0,
                "embed_policy": {
                    "source": "old-fixture"
                },
            }),
        )?;

        let report = semantic_worker_report_best_effort(temp.path()).to_json();
        assert_eq!(report["status"], "unknown");
        assert_eq!(report["pid"], Value::Null);
        assert_eq!(report["last_error"], Value::Null);
        assert_ne!(report["embed_policy"]["source"], "old-fixture");
        assert_eq!(report["coverage"]["searchable_items"], 0);
        assert_eq!(report["coverage"]["searchable_items_known"], false);
        assert_eq!(report["coverage"]["embedded_items"], 0);
        Ok(())
    }

    #[test]
    fn semantic_incremental_slice_requires_previous_ready_status() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let stats = SemanticSidecarStats {
            embedded_items: 10,
            embedded_chunks: 20,
        };
        assert!(!semantic_worker_status_was_ready_for_stats(
            temp.path(),
            stats
        ));

        write_semantic_worker_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "completed",
                "model_key": SEMANTIC_MODEL_KEY,
                "searchable_items": 11,
                "embedded_items": 10,
                "embedded_chunks": 20,
                "dirty_items": 0,
            }),
        )?;
        assert!(!semantic_worker_status_was_ready_for_stats(
            temp.path(),
            stats
        ));

        write_semantic_worker_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "ready",
                "model_key": SEMANTIC_MODEL_KEY,
                "searchable_items": 10,
                "embedded_items": 10,
                "embedded_chunks": 20,
                "dirty_items": 0,
            }),
        )?;
        assert!(semantic_worker_status_was_ready_for_stats(
            temp.path(),
            stats
        ));
        Ok(())
    }

    #[test]
    fn daemon_semantic_status_ignores_job_from_old_model_key() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_daemon_job_status(
            &daemon_semantic_job_path(temp.path()),
            &json!({
                "schema_version": 1,
                "status": "ready",
                "model_key": "fastembed:old-model-key",
                "last_run_at_ms": 1234,
                "indexed_chunks": 99,
            }),
        )?;

        let daemon = daemon_report(temp.path(), &semantic_worker_report_best_effort(temp.path()));
        let semantic = &daemon["jobs"]["semantic_index"];
        assert_eq!(semantic["status"], "unknown");
        assert_eq!(semantic["reason"], "searchable_items_unknown");
        assert_eq!(semantic["last_run_status"], Value::Null);
        assert_eq!(semantic["indexed_chunks"], Value::Null);
        Ok(())
    }

    #[test]
    fn hybrid_semantic_readiness_requires_complete_coverage() {
        assert!(semantic_hybrid_coverage_ready(0, 0, 0));
        assert!(semantic_hybrid_coverage_ready(10, 10, 0));
        assert!(semantic_hybrid_coverage_ready(11, 10, 0));

        assert!(!semantic_hybrid_coverage_ready(0, 10, 0));
        assert!(!semantic_hybrid_coverage_ready(1_000, 100_000, 0));
        assert!(!semantic_hybrid_coverage_ready(99_999, 100_000, 0));
        assert!(!semantic_hybrid_coverage_ready(10, 10, 1));
    }

    #[test]
    fn daemon_recent_queue_marks_user_anchor_dirty_when_assistant_changes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path();
        let store = Store::open(database_path(data_root.to_path_buf()))?;
        let session_id = Uuid::new_v4();
        insert_test_session(&store, session_id)?;
        let user = test_session_message(1, session_id, EventRole::User, "semantic anchor prompt");
        let assistant = test_session_message(
            2,
            session_id,
            EventRole::Assistant,
            "original assistant answer",
        );
        store.upsert_event(&user)?;
        store.upsert_event(&assistant)?;
        store.refresh_event_embedding_document_count_cache()?;
        let docs = store.event_embedding_documents_by_ids(&[user.id])?;
        let doc = docs.first().expect("user lite-turn document");
        let source_text = semantic_source_text(&doc.text);
        let source_hash = semantic_document_hash(doc, &source_text);

        let vector_path = semantic_vector_path(data_root);
        let mut vector_store = SemanticVectorStore::open(&vector_path)?;
        vector_store.upsert_chunk_embeddings(&[(
            test_chunk(user.id, user.seq, &source_hash),
            test_embedding(1.0, 0.0),
        )])?;
        assert_eq!(vector_store.dirty_event_count()?, 0);
        drop(vector_store);

        let mut updated_assistant = assistant.clone();
        updated_assistant.payload = json!({ "text": "updated assistant answer" });
        updated_assistant.occurred_at = utc_now();
        store.upsert_event(&updated_assistant)?;

        assert_eq!(queue_recent_semantic_work(data_root, &store, "test_recent")?, 1);
        let vector_store = SemanticVectorStore::open(&vector_path)?;
        assert_eq!(vector_store.queued_dirty_event_ids(10)?, vec![user.id]);
        Ok(())
    }

    #[test]
    fn semantic_only_search_fails_fast_while_worker_is_running() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let docs = write_searchable_store(temp.path(), 1)?;
        let doc = docs.first().expect("searchable fixture doc");
        let source_text = semantic_source_text(&doc.text);
        let source_hash = semantic_document_hash(doc, &source_text);
        let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
        vector_store.upsert_chunk_embeddings(&[(
            test_chunk(doc.event_id, doc.seq, &source_hash),
            test_embedding(1.0, 0.0),
        )])?;
        drop(vector_store);

        let _lock = SemanticWorkerLock::acquire(temp.path())?
            .expect("test should acquire semantic worker lock");
        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        let err = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic daemon scheduling fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            1.0,
            RefreshArg::Off,
            false,
        )
        .expect_err("semantic-only search should fail while worker is running");
        assert!(format!("{err:#}").contains("semantic worker is currently indexing"));
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn semantic_cache_discovery_prefers_explicit_env_roots() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let explicit = temp.path().join("explicit");
        let fallback = temp.path().join("fallback");
        write_test_semantic_cache(&fallback)?;

        let env = SemanticCacheEnv {
            semantic_cache_dir: Some(explicit.clone()),
            hf_home: Some(temp.path().join("bad-hf-home")),
            current_dir: Some(temp.path().to_path_buf()),
            home: Some(temp.path().to_path_buf()),
            xdg_cache_home: Some(fallback.clone()),
            ..SemanticCacheEnv::default()
        };

        assert_eq!(semantic_worker_cache_dir_from_env(&data_root, &env), explicit);
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn daemon_allows_history_refresh_after_one_semantic_bootstrap_pass() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
        write_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            Some(daemon_semantic_indexed_test_job(temp.path())),
        );
        let mut runtime = DaemonRuntime::default();

        let first = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None)?;
        let second = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None)?;

        assert!(first.did_work);
        assert!(second.did_work);
        assert!(!first.failed);
        assert!(!second.failed);
        assert_eq!(
            *calls.borrow(),
            vec!["semantic_index", "history_refresh", "semantic_index"]
        );
        let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
        assert_eq!(daemon["jobs"]["history_refresh"]["status"], "completed");
        assert_ne!(
            daemon["jobs"]["history_refresh"]["reason"],
            "semantic_bootstrap_in_progress"
        );
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn semantic_cache_discovery_finds_repo_local_fastembed_cache() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let repo_cache = temp.path().join(".fastembed_cache");
        write_test_semantic_cache(&repo_cache)?;

        let env = SemanticCacheEnv {
            current_dir: Some(temp.path().to_path_buf()),
            home: Some(temp.path().join("home")),
            ..SemanticCacheEnv::default()
        };

        assert_eq!(
            semantic_worker_cache_dir_from_env(&data_root, &env),
            repo_cache
        );
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn semantic_cache_discovery_finds_common_home_cache() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let home = temp.path().join("home");
        let home_cache = home.join(".cache").join("huggingface").join("hub");
        write_test_semantic_cache(&home_cache)?;

        let env = SemanticCacheEnv {
            current_dir: Some(temp.path().join("repo")),
            home: Some(home),
            ..SemanticCacheEnv::default()
        };

        assert_eq!(
            semantic_worker_cache_dir_from_env(&data_root, &env),
            home_cache
        );
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn daemon_prioritizes_semantic_bootstrap_over_history_refresh() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
        write_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            Some(daemon_semantic_indexed_test_job(temp.path())),
        );

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None)?;

        assert!(iteration.did_work);
        assert!(!iteration.failed);
        assert_eq!(*calls.borrow(), vec!["semantic_index"]);
        let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
        assert_eq!(daemon["jobs"]["history_refresh"]["status"], "skipped");
        assert_eq!(
            daemon["jobs"]["history_refresh"]["reason"],
            "semantic_bootstrap_in_progress"
        );
        assert_eq!(
            daemon["jobs"]["semantic_index"]["last_run_status"],
            "budget_exhausted"
        );
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn daemon_history_refresh_runs_when_semantic_has_no_backlog() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
        let docs = write_searchable_store(temp.path(), 1)?;
        let doc = docs.first().expect("searchable fixture doc");
        let source_text = semantic_source_text(&doc.text);
        let source_hash = semantic_document_hash(doc, &source_text);
        let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
        vector_store.upsert_chunk_embeddings(&[(
            test_chunk(doc.event_id, doc.seq, &source_hash),
            test_embedding(1.0, 0.0),
        )])?;
        drop(vector_store);

        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            Some(daemon_semantic_indexed_test_job(temp.path())),
        );

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None)?;

        assert!(iteration.did_work);
        assert!(!iteration.failed);
        assert_eq!(*calls.borrow(), vec!["history_refresh", "semantic_index"]);
        let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
        assert_eq!(daemon["jobs"]["history_refresh"]["status"], "completed");
        assert_ne!(
            daemon["jobs"]["history_refresh"]["reason"],
            "semantic_bootstrap_in_progress"
        );
        Ok(())
    }

    #[test]
    fn daemon_history_refresh_runs_when_store_is_not_ready() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks =
            install_test_daemon_jobs(calls.clone(), Some(daemon_history_completed_test_job()), None);

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None)?;

        assert!(!iteration.failed);
        assert_eq!(calls.borrow().first(), Some(&"history_refresh"));
        let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
        assert_eq!(daemon["jobs"]["history_refresh"]["status"], "completed");
        assert_ne!(
            daemon["jobs"]["history_refresh"]["reason"],
            "semantic_bootstrap_in_progress"
        );
        assert_eq!(daemon["jobs"]["semantic_index"]["last_run_status"], "skipped");
        assert_eq!(daemon["jobs"]["semantic_index"]["last_run_reason"], "store_missing");
        Ok(())
    }

    #[test]
    fn sqlite_vec0_full_scan_matches_rust_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;

        assert!(store.sqlite_vec0_ready()?);

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let rust_hits = store.search_event_ids(&query, &[close_event, far_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(rust_hits.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_RUST));
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(rust_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, close_event);
        assert_eq!(rust_hits.hits[0].event_id, close_event);
        assert_eq!(sqlite_hits.hits[1].event_id, far_event);
        assert_eq!(rust_hits.hits[1].event_id, far_event);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_caps_large_k_without_falling_back() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;

        let search = store.search(&test_embedding(1.0, 0.0), SEMANTIC_SQLITE_VEC0_MAX_K + 1)?;

        assert_eq!(
            search.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(search.hits.len(), 2);
        assert_eq!(search.hits[0].event_id, close_event);
        Ok(())
    }

    #[test]
    fn rust_full_scan_requires_sidecar_within_cap_without_vec0() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let chunk_limit = semantic_rust_full_scan_chunk_limit();
        store.conn.execute(
            r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms)
            VALUES (?1, 1, ?2, 1)
            "#,
            params![SEMANTIC_MODEL_KEY, (chunk_limit + 1) as i64],
        )?;

        assert!(!semantic_full_corpus_vector_scan_ready(&store)?);

        store.conn.execute(
            r#"
            UPDATE semantic_index_stats
            SET embedded_chunks = ?2
            WHERE model_key = ?1
            "#,
            params![SEMANTIC_MODEL_KEY, chunk_limit as i64],
        )?;
        assert!(semantic_full_corpus_vector_scan_ready(&store)?);
        Ok(())
    }

    #[test]
    fn prune_ineligible_events_is_bounded_and_advances_cursor() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let docs = write_searchable_store(temp.path(), SEMANTIC_PRUNE_EVENTS_PER_PASS + 1)?;
        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
        let chunks = docs
            .iter()
            .map(|doc| {
                (
                    test_chunk(doc.event_id, doc.seq, "intentionally-stale"),
                    test_embedding(1.0, 0.0),
                )
            })
            .collect::<Vec<_>>();
        vector_store.upsert_chunk_embeddings(&chunks)?;
        assert_eq!(
            vector_store.cached_or_exact_stats()?.embedded_items,
            SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
        );

        let first = vector_store.prune_ineligible_events(&store)?;
        assert_eq!(first.queued_stale_events, SEMANTIC_PRUNE_EVENTS_PER_PASS);
        assert_eq!(
            vector_store.cached_or_exact_stats()?.embedded_items,
            1,
            "first pass should leave the oldest event for the next cursor page"
        );

        let second = vector_store.prune_ineligible_events(&store)?;
        assert_eq!(second.queued_stale_events, 1);
        assert_eq!(vector_store.cached_or_exact_stats()?.embedded_items, 0);
        assert_eq!(
            vector_store.dirty_event_count()?,
            SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
        );
        Ok(())
    }

    #[test]
    fn sqlite_vec0_overfetches_until_unique_events_match_rust_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let multi_chunk_event = Uuid::new_v4();
        let next_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 0, 3),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 1, 3),
                test_embedding(0.999, 0.044),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 2, 3),
                test_embedding(0.995, 0.099),
            ),
            (
                test_chunk_at(next_event, 1, "next", 0, 1),
                test_embedding(0.98, 0.199),
            ),
        ])?;

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let rust_hits = store.search_event_ids(&query, &[multi_chunk_event, next_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, multi_chunk_event);
        assert_eq!(sqlite_hits.hits[1].event_id, next_event);
        assert_eq!(
            sqlite_hits
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            rust_hits
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn sqlite_vec0_rebuilds_incompatible_derived_schema() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        {
            let conn = Connection::open(&vector_path)?;
            conn.execute_batch(
                r#"
                CREATE TABLE event_embedding_vec0_meta (
                    rowid INTEGER PRIMARY KEY,
                    event_id TEXT NOT NULL
                );
                CREATE TABLE event_embedding_vec0 (
                    rowid INTEGER PRIMARY KEY,
                    embedding BLOB
                );
                "#,
            )?;
        }

        let mut store = SemanticVectorStore::open(&vector_path)?;
        let close_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(close_event, 1, "close"),
            test_embedding(1.0, 0.0),
        )])?;

        assert!(store.sqlite_vec0_ready()?);
        let vec0_sql = sqlite_table_sql(&store.conn, "event_embedding_vec0")?.unwrap_or_default();
        assert!(vec0_sql.to_ascii_lowercase().contains("using vec0"));
        assert!(sqlite_table_has_columns(
            &store.conn,
            "event_embedding_vec0_meta",
            &["model_key", "source_text_sha256", "start_char", "end_char"]
        )?);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_rebuilds_when_same_count_meta_rowids_drift() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;
        assert!(store.sqlite_vec0_ready()?);

        let canonical_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
	            "UPDATE event_embedding_vec0_meta SET rowid = rowid + 1000 WHERE event_id = ?1 AND model_key = ?2",
	            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
	        )?;

        assert!(!store.sqlite_vec0_ready()?);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let repaired_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_vec0_meta WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(repaired_rowid, canonical_rowid);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_payload_drift_is_repaired_by_maintenance() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;
        assert!(store.sqlite_vec0_ready()?);

        let close_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
            "DELETE FROM event_embedding_vec0 WHERE rowid = ?1",
            params![close_rowid],
        )?;
        store.conn.execute(
            "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
            params![close_rowid, serialize_f32_blob(&test_embedding(0.0, 1.0))],
        )?;

        assert!(!store.sqlite_vec0_ready()?);
        assert!(
            store.sqlite_vec0_search_ready()?,
            "search hot path should use cheap count readiness and leave deep integrity checks to maintenance"
        );

        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);
        Ok(())
    }

    #[test]
    fn daemon_autostart_records_lifecycle_trigger_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let args = DaemonRunArgs {
            foreground: false,
            once: true,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Auto),
            trigger_command: Some(DaemonTriggerCommandArg::Setup),
            json: true,
        };

        write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;
        let status = read_daemon_status(temp.path()).expect("daemon status");
        assert_eq!(status["start_mode"], "auto");
        assert_eq!(status["trigger_command"], "setup");
        Ok(())
    }

    #[test]
    fn daemon_report_marks_orphaned_running_status_recoverable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let args = DaemonRunArgs {
            foreground: false,
            once: true,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Manual),
            trigger_command: None,
            json: true,
        };
        write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;

        let daemon = daemon_report(temp.path(), &semantic_worker_report_best_effort(temp.path()));

        assert_eq!(daemon["status"], "stale_lock");
        assert_eq!(daemon["running"], false);
        assert_eq!(daemon["recoverable"], true);
        assert_eq!(daemon["reason"], "daemon_status_stale");
        Ok(())
    }

}

#[cfg(all(test, ctx_semantic_fastembed))]
mod fastembed_policy_tests {
    use super::*;

    fn write_test_semantic_cache(root: &Path) -> Result<()> {
        let snapshot = root
            .join(SEMANTIC_HF_MODEL_CACHE_DIR)
            .join("snapshots")
            .join("test-snapshot");
        fs::create_dir_all(&snapshot)?;
        fs::create_dir_all(root.join(SEMANTIC_HF_MODEL_CACHE_DIR).join("refs"))?;
        fs::write(
            root.join(SEMANTIC_HF_MODEL_CACHE_DIR)
                .join("refs")
                .join("main"),
            "test-snapshot\n",
        )?;
        for file in SEMANTIC_REQUIRED_MODEL_FILES {
            fs::write(snapshot.join(file), b"test")?;
        }
        Ok(())
    }

    #[test]
    fn adaptive_policy_formula_runs_without_sqlite_vec() {
        let gib = 1024 * 1024 * 1024;
        let snapshot = SemanticMemorySnapshot {
            total_bytes: Some(64 * gib),
            available_bytes: Some(32 * gib),
        };

        let policy = semantic_adaptive_embed_policy(snapshot);

        assert_eq!(policy.memory_budget_bytes, SEMANTIC_MEMORY_BUDGET_MAX_BYTES);
        assert_eq!(policy.batch_size, SEMANTIC_EMBED_BATCH_ADAPTIVE_MAX);
        assert_eq!(policy.source, "adaptive");
    }

    #[test]
    fn semantic_cache_dir_override_beats_hf_home_without_sqlite_vec() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let explicit = temp.path().join("explicit");
        write_test_semantic_cache(&explicit)?;

        let env = SemanticCacheEnv {
            semantic_cache_dir: Some(explicit.clone()),
            hf_home: Some(temp.path().join("bad-hf-home")),
            ..SemanticCacheEnv::default()
        };

        assert_eq!(semantic_worker_cache_dir_from_env(&data_root, &env), explicit);
        Ok(())
    }
}

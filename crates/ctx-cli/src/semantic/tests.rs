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

    #[test]
    fn deadline_partial_batch_keeps_only_fully_embedded_events() {
        let first = Uuid::new_v4();
        let split = Uuid::new_v4();
        let last = Uuid::new_v4();
        let pending = vec![
            test_chunk_at(first, 1, "first", 0, 1),
            test_chunk_at(split, 2, "split", 0, 3),
            test_chunk_at(split, 2, "split", 1, 3),
            test_chunk_at(split, 2, "split", 2, 3),
            test_chunk_at(last, 3, "last", 0, 1),
        ];

        assert_eq!(semantic_complete_embedding_prefix(&pending, 0), 0);
        assert_eq!(semantic_complete_embedding_prefix(&pending, 1), 1);
        assert_eq!(semantic_complete_embedding_prefix(&pending, 2), 1);
        assert_eq!(semantic_complete_embedding_prefix(&pending, 3), 1);
        assert_eq!(semantic_complete_embedding_prefix(&pending, 4), 4);
        assert_eq!(semantic_complete_embedding_prefix(&pending, 5), 5);
        assert_eq!(semantic_complete_embedding_prefix(&pending, 99), 5);

        let considered = vec![first, split, last];
        assert_eq!(
            semantic_contiguous_consumed_event_ids(&considered, &[first, last]),
            vec![first]
        );
        assert_eq!(
            semantic_contiguous_consumed_event_ids(&considered, &[first, split, last]),
            considered
        );

        let cursors = vec![(first, (30, 3)), (split, (20, 2)), (last, (10, 1))];
        assert_eq!(
            semantic_contiguous_consumed_cursor(&cursors, &[first, last]),
            Some((30, 3)),
            "an unchanged event after an unfinished event cannot advance the cursor"
        );
        assert_eq!(
            semantic_contiguous_consumed_cursor(&cursors, &[first, split, last]),
            Some((10, 1))
        );
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

    #[test]
    fn e5_embedding_text_uses_query_and_passage_prefixes_once() {
        assert_eq!(
            semantic_e5_query_text_value("find a daemon failure"),
            "query: find a daemon failure"
        );
        assert_eq!(
            semantic_e5_query_text_value("  query: find a daemon failure"),
            "query: find a daemon failure"
        );
        assert_eq!(
            semantic_e5_passage_text("daemon failed to restart"),
            "passage: daemon failed to restart"
        );
        assert_eq!(
            semantic_e5_passage_text("  passage: daemon failed to restart"),
            "passage: daemon failed to restart"
        );
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn fixed_shape_settings_are_strict() {
        assert_eq!(semantic_fixed_shape_from_values(None, None).unwrap(), None);
        assert_eq!(
            semantic_fixed_shape_from_values(Some("16"), Some("512")).unwrap(),
            Some((16, 512))
        );
        for values in [
            (Some("16"), None),
            (None, Some("512")),
            (Some("0"), Some("512")),
            (Some("wat"), Some("512")),
            (Some("16"), Some("-1")),
        ] {
            assert!(semantic_fixed_shape_from_values(values.0, values.1).is_err());
        }
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn fixed_batch_padding_preserves_complete_batches() -> Result<()> {
        let make = |count| {
            (0..count)
                .map(|index| format!("passage: {index}"))
                .collect::<Vec<_>>()
        };
        assert!(pad_texts_to_exact_batch(make(0), 4)?.is_empty());
        assert_eq!(pad_texts_to_exact_batch(make(4), 4)?.len(), 4);
        let padded = pad_texts_to_exact_batch(make(5), 4)?;
        assert_eq!(padded.len(), 8);
        assert_eq!(&padded[..5], make(5));
        assert!(padded[5..]
            .iter()
            .all(|text| text == SEMANTIC_PASSAGE_PREFIX));
        assert!(pad_texts_to_exact_batch(make(1), 0).is_err());
        Ok(())
    }

    #[test]
    fn semantic_worker_report_preserves_embed_policy_from_status() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_worker_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "budget_exhausted",
                "model_key": semantic_model_key(),
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
                "model_key": semantic_model_key(),
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
                "model_key": semantic_model_key(),
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
    fn ready_index_requests_daemon_model_load_with_or_without_cache() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut report = SemanticWorkerReport::unavailable(temp.path(), "test");
        report.status = "ready".to_owned();
        report.searchable_items = 10;
        report.searchable_items_known = true;
        report.embedded_items = 10;
        report.queued_items_estimate = 0;
        report.model_cache_available = false;
        report.embedding_runtime = Some(json!({
            "backend": "cpu",
            "compute_class": "cpu",
        }));

        assert!(semantic_daemon_model_load_needed(&report, false));
        assert!(!semantic_daemon_model_load_needed(&report, true));
        report.model_cache_available = true;
        assert!(semantic_daemon_model_load_needed(&report, false));
        let status = daemon_semantic_job_report(temp.path(), &report, true);
        assert_eq!(status["embedding_runtime"]["backend"], "cpu");
        assert_eq!(status["embedding_runtime"]["compute_class"], "cpu");
        Ok(())
    }

    #[test]
    fn daemon_status_reports_retryable_memory_deferral() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_enabled_config(temp.path())?;
        let mut report = SemanticWorkerReport::unavailable(temp.path(), "test");
        report.status = "model_load_deferred".to_owned();
        report.searchable_items = 10;
        report.searchable_items_known = true;
        report.queued_items_estimate = 10;
        write_daemon_job_status(
            &daemon_semantic_job_path(temp.path()),
            &compact_json(json!({
                "schema_version": 1,
                "model_key": semantic_model_key(),
                "status": "skipped",
                "reason": "memory_pressure",
                "retryable": true,
                "available_memory_bytes": 1_610_612_736_u64,
                "required_available_memory_bytes": 2_147_483_648_u64,
            })),
        )?;

        let value = daemon_semantic_job_report(temp.path(), &report, true);
        assert_eq!(value["status"], "skipped");
        assert_eq!(value["reason"], "memory_pressure");
        assert_eq!(value["worker_status"], "model_load_deferred");
        assert_eq!(value["retryable"], true);
        assert_eq!(value["available_memory_bytes"], 1_610_612_736_u64);
        assert_eq!(
            value["required_available_memory_bytes"],
            2_147_483_648_u64
        );
        Ok(())
    }

    #[test]
    fn daemon_semantic_status_ignores_job_from_old_model_key() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_enabled_config(temp.path())?;
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
    fn semantic_only_search_does_not_reject_a_running_worker() -> Result<()> {
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
            true,
            1.0,
            RefreshArg::Off,
            false,
        )
        .expect_err("fixture has no daemon query service");
        let message = format!("{err:#}");
        assert!(message.contains("daemon semantic query service is not available"));
        assert!(!message.contains("semantic worker is currently indexing"));
        Ok(())
    }

    #[test]
    fn advisory_pid_lock_does_not_expire_or_trust_a_reused_pid() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
        let path = daemon_lock_path(temp.path());
        assert!(pid_lock_file_reports_running(
            &path,
            Some(ProcessState::Running),
            "running"
        ));
        assert!(!daemon_lock_is_stale(&path));
        assert_eq!(
            observe_pid_advisory_lock(&path),
            Some(PidAdvisoryLockObservation {
                held: true,
                released: false,
            })
        );
        assert!(DaemonLock::acquire(temp.path())?.is_none());

        drop(lock);
        assert!(!pid_lock_file_reports_running(
            &path,
            Some(ProcessState::Running),
            "running"
        ));
        assert!(daemon_lock_is_stale(&path));
        assert_eq!(
            observe_pid_advisory_lock(&path),
            Some(PidAdvisoryLockObservation {
                held: false,
                released: true,
            })
        );
        let replacement = DaemonLock::acquire(temp.path())?
            .expect("released advisory lock should be reusable despite live payload pid");
        assert!(pid_lock_file_reports_running(
            &path,
            Some(ProcessState::Running),
            "running"
        ));
        drop(replacement);

        fs::write(&path, b"{")?;
        assert!(!daemon_lock_is_stale(&path));
        Ok(())
    }

    #[test]
    fn advisory_pid_lock_allows_only_one_concurrent_reclaimer() -> Result<()> {
        let temp = tempfile::tempdir()?;
        drop(DaemonLock::acquire(temp.path())?.expect("seed lock"));
        let root = temp.path().to_path_buf();
        let contenders = 8;
        let start = Arc::new(std::sync::Barrier::new(contenders + 1));
        let finish = Arc::new(std::sync::Barrier::new(contenders + 1));
        let (send, receive) = std::sync::mpsc::channel();
        let mut threads = Vec::new();
        for _ in 0..contenders {
            let root = root.clone();
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            let send = send.clone();
            threads.push(std::thread::spawn(move || -> Result<()> {
                start.wait();
                let lock = DaemonLock::acquire(&root)?;
                send.send(lock.is_some())?;
                finish.wait();
                drop(lock);
                Ok(())
            }));
        }
        drop(send);
        start.wait();
        let acquired = (0..contenders)
            .map(|_| receive.recv())
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|acquired| *acquired)
            .count();
        finish.wait();
        for thread in threads {
            thread.join().expect("lock contender panicked")?;
        }
        assert_eq!(acquired, 1);
        Ok(())
    }

    #[test]
    fn advisory_pid_lock_waits_out_a_status_probe() -> Result<()> {
        let temp = tempfile::tempdir()?;
        drop(DaemonLock::acquire(temp.path())?.expect("seed lock"));
        let path = daemon_lock_path(temp.path());
        let probe = private_open_existing_lock_file(&pid_lock_guard_path(&path))?;
        fs2::FileExt::lock_shared(&probe)?;
        let root = temp.path().to_path_buf();
        let contender = std::thread::spawn(move || DaemonLock::acquire(&root));
        std::thread::sleep(StdDuration::from_millis(5));
        fs2::FileExt::unlock(&probe)?;
        let lock = contender
            .join()
            .expect("lock contender panicked")?
            .expect("status probe should not make acquisition give up");
        drop(lock);
        Ok(())
    }

    #[test]
    fn advisory_guard_survives_metadata_path_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = daemon_lock_path(temp.path());
        let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
        fs::remove_file(&path)?;
        fs::write(&path, serde_json::to_vec(&pid_lock_payload(json!({})))?)?;
        assert!(DaemonLock::acquire(temp.path())?.is_none());
        drop(lock);
        assert!(DaemonLock::acquire(temp.path())?.is_some());
        Ok(())
    }

    #[test]
    fn advisory_publication_does_not_overwrite_a_late_legacy_owner() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = daemon_lock_path(temp.path());
        create_private_dir_all(path.parent().expect("lock parent"))?;
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "pid": process::id(),
                "started_at_ms": utc_now().timestamp_millis(),
            }))?,
        )?;
        assert!(!publish_pid_lock_metadata(
            &path,
            &pid_lock_payload(json!({}))
        )?);
        assert!(!pid_lock_uses_advisory_protocol(
            &read_pid_lock_json(&path).expect("legacy metadata")
        ));
        Ok(())
    }

    #[test]
    fn advisory_lock_reclaims_dead_legacy_metadata_for_upgrade_handoff() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = daemon_lock_path(temp.path());
        create_private_dir_all(path.parent().expect("lock parent"))?;
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "pid": u32::MAX,
                "started_at_ms": 0,
            }))?,
        )?;
        assert!(daemon_lock_is_stale(&path));
        let lock = DaemonLock::acquire(temp.path())?
            .expect("dead legacy owner should be reclaimed during upgrade");
        assert!(pid_lock_uses_advisory_protocol(
            &read_pid_lock_json(&path).expect("advisory metadata")
        ));
        drop(lock);
        Ok(())
    }

    #[test]
    fn hybrid_search_with_semantic_disabled_uses_lexical_without_sidecar() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_searchable_store(temp.path(), 1)?;
        let vector_path = semantic_vector_path(temp.path());
        let store = Store::open(database_path(temp.path().to_path_buf()))?;

        let (packet, retrieval) = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic daemon scheduling fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            false,
            0.35,
            RefreshArg::Off,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(
            retrieval.to_json()["semantic_fallback_code"],
            "semantic_disabled"
        );
        assert_eq!(packet.query, "semantic daemon scheduling fixture");
        assert!(!vector_path.exists());
        Ok(())
    }

    #[cfg(ctx_semantic_fastembed)]
    #[test]
    fn hybrid_search_reports_missing_daemon_query_service() -> Result<()> {
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

        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        let (packet, retrieval) = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic daemon scheduling fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            true,
            0.35,
            RefreshArg::Off,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(
            retrieval.to_json()["semantic_fallback_code"],
            "daemon_query_service_unavailable"
        );
        assert_eq!(packet.query, "semantic daemon scheduling fixture");

        let err = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic daemon scheduling fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            true,
            1.0,
            RefreshArg::Off,
            false,
        )
        .expect_err("semantic-only search should require the daemon query service");
        assert!(format!("{err:#}").contains("daemon semantic query service is not available"));
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
        write_semantic_enabled_config(temp.path())?;
        write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
        write_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            Some(daemon_semantic_indexed_test_job(temp.path())),
        );
        let mut runtime = DaemonRuntime::default();

        let first = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None, true)?;
        let second = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None, true)?;

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
        write_semantic_enabled_config(temp.path())?;
        write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
        write_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            Some(daemon_semantic_indexed_test_job(temp.path())),
        );

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None, true)?;

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
        write_semantic_enabled_config(temp.path())?;
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
        let iteration = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None, true)?;

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
    fn daemon_skips_semantic_job_when_semantic_is_disabled() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_searchable_store(temp.path(), 2)?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            Some(daemon_semantic_indexed_test_job(temp.path())),
        );

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            false,
        )?;

        assert!(!iteration.failed);
        assert_eq!(*calls.borrow(), vec!["history_refresh"]);
        let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
        assert_eq!(daemon["jobs"]["semantic_index"]["status"], "disabled");
        assert_eq!(daemon["jobs"]["semantic_index"]["reason"], "semantic_disabled");
        assert!(!semantic_vector_path(temp.path()).exists());
        Ok(())
    }

    #[test]
    fn daemon_history_refresh_runs_when_store_is_not_ready() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_enabled_config(temp.path())?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks =
            install_test_daemon_jobs(calls.clone(), Some(daemon_history_completed_test_job()), None);

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(&test_daemon_run_args(), temp.path(), &mut runtime, None, true)?;

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
            params![semantic_model_key(), (chunk_limit + 1) as i64],
        )?;

        assert!(!semantic_full_corpus_vector_scan_ready(&store)?);

        store.conn.execute(
            r#"
            UPDATE semantic_index_stats
            SET embedded_chunks = ?2
            WHERE model_key = ?1
            "#,
            params![semantic_model_key(), chunk_limit as i64],
        )?;
        assert!(semantic_full_corpus_vector_scan_ready(&store)?);
        Ok(())
    }

    #[test]
    fn opening_vector_store_preserves_other_embedding_spaces_and_current_cursor() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        let old_model_key = "fastembed:all-MiniLM-L6-v2:old";
        {
            let store = SemanticVectorStore::open(&vector_path)?;
            store.conn.execute(
                r#"
                INSERT INTO embedding_models
                    (model_key, backend, model_id, dimensions, distance, normalized, created_at_ms)
                VALUES (?1, 'fastembed', 'sentence-transformers/all-MiniLM-L6-v2', 384, 'cosine', 1, 1)
                "#,
                [old_model_key],
            )?;
            store.conn.execute(
                r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, event_seq, chunk_index, chunk_count,
                     source_text_sha256, chunk_text_sha256, start_char, end_char,
                     dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, 1, 0, 1, 'source', 'chunk', 0, 5, 384, ?3, 1)
                "#,
                params![
                    Uuid::new_v4().to_string(),
                    old_model_key,
                    serialize_f32_blob(&test_embedding(1.0, 0.0))
                ],
            )?;
            store.set_backfill_cursor(Some((123, 456)))?;
        }

        let store = SemanticVectorStore::open(&vector_path)?;
        let old_rows = store.conn.query_row(
            "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1",
            [old_model_key],
            |row| row.get::<_, i64>(0),
        )?;
        let old_models = store.conn.query_row(
            "SELECT COUNT(*) FROM embedding_models WHERE model_key = ?1",
            [old_model_key],
            |row| row.get::<_, i64>(0),
        )?;

        assert_eq!(old_rows, 1);
        assert_eq!(old_models, 1);
        assert_eq!(store.backfill_cursor()?, Some((123, 456)));
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
            params![close_event.to_string(), semantic_model_key()],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
	            "UPDATE event_embedding_vec0_meta SET rowid = rowid + 1000 WHERE event_id = ?1 AND model_key = ?2",
	            params![close_event.to_string(), semantic_model_key()],
	        )?;

        assert!(!store.sqlite_vec0_ready()?);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let repaired_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_vec0_meta WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), semantic_model_key()],
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
            params![close_event.to_string(), semantic_model_key()],
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

#[cfg(all(test, not(ctx_semantic_fastembed)))]
mod unsupported_platform_tests {
    use super::*;
    use ctx_history_core::{
        new_id, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
    };

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

    fn insert_test_event(store: &Store, text: &str) -> Result<()> {
        store.upsert_event(&Event {
            id: new_id(),
            seq: 1,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: utc_now(),
            capture_source_id: None,
            payload: json!({ "text": text }),
            payload_blob_id: None,
            dedupe_key: None,
            sync: test_sync_metadata(),
        })?;
        Ok(())
    }

    #[test]
    fn hybrid_search_falls_back_to_lexical_on_unsupported_platform() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir_all(temp.path())?;
        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        insert_test_event(&store, "semantic unsupported platform lexical fallback fixture")?;

        let (packet, retrieval) = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic unsupported platform lexical fallback fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            true,
            0.35,
            RefreshArg::Off,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(retrieval.to_json()["semantic_status"], "unavailable");
        assert_eq!(retrieval.to_json()["semantic_fallback_code"], "unsupported_platform");
        assert_eq!(
            packet.query,
            "semantic unsupported platform lexical fallback fixture"
        );

        let error = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic unsupported platform lexical fallback fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            true,
            1.0,
            RefreshArg::Off,
            false,
        )
        .expect_err("explicit semantic search should fail on unsupported platforms");
        assert!(format!("{error:#}").contains("local semantic search is not supported"));
        Ok(())
    }
}

#[cfg(test)]
mod query_service_transport_tests {
    use super::*;

    #[cfg(any(unix, windows))]
    const TEST_QUERY_REQUEST_READ_TIMEOUT: StdDuration = StdDuration::from_millis(100);

    #[cfg(any(unix, windows))]
    fn start_test_query_service(data_root: &Path) -> Result<DaemonQueryService> {
        start_daemon_query_service_with_request_timeout(
            data_root,
            Arc::new(Mutex::new(None)),
            TEST_QUERY_REQUEST_READ_TIMEOUT,
        )
    }

    #[cfg(any(unix, windows))]
    fn wait_for_active_query(service: &DaemonQueryService) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < StdDuration::from_secs(2) {
            if service.activity.snapshot().0 == 1 {
                return Ok(());
            }
            std::thread::sleep(StdDuration::from_millis(5));
        }
        Err(anyhow!(
            "daemon query service did not accept the test client"
        ))
    }

    #[cfg(unix)]
    fn connect_stalled_query_client(data_root: &Path) -> Result<UnixStream> {
        let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
        let DaemonQueryEndpoint::Unix { path, .. } = endpoint;
        UnixStream::connect(&path)
            .with_context(|| format!("connect test query socket {}", path.display()))
    }

    #[cfg(unix)]
    fn connect_valid_nonreading_query_client(data_root: &Path) -> Result<UnixStream> {
        let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
        let DaemonQueryEndpoint::Unix { path, token } = endpoint;
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("connect test query socket {}", path.display()))?;
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "schema_version": 1,
                "op": "ping",
                "token": token,
            })))?
        )?;
        Ok(stream)
    }

    #[cfg(unix)]
    #[test]
    fn configured_unix_query_stream_drains_response_larger_than_socket_buffer() -> Result<()> {
        use std::io::{Read, Write};

        let (mut server, mut client) = UnixStream::pair()?;
        server.set_nonblocking(true)?;
        configure_daemon_query_stream_unix(&server, StdDuration::from_secs(2))?;
        let response = vec![b'x'; 1024 * 1024];
        let expected = response.len();
        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            server.write_all(&response)?;
            server.shutdown(Shutdown::Write)
        });
        std::thread::sleep(StdDuration::from_millis(25));
        let mut received = Vec::new();
        client.read_to_end(&mut received)?;
        writer.join().expect("query response writer panicked")?;
        assert_eq!(received.len(), expected);
        Ok(())
    }

    #[cfg(windows)]
    fn connect_stalled_query_client(data_root: &Path) -> Result<WindowsQueryHandle> {
        let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
        let DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, .. } = endpoint;
        let deadline = WindowsIoDeadline::new(StdDuration::from_secs(2));
        open_windows_daemon_query_pipe(&windows_wide_null(&pipe_name), &deadline)
    }

    #[cfg(windows)]
    fn connect_valid_nonreading_query_client(data_root: &Path) -> Result<WindowsQueryHandle> {
        let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
        let DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, token } = endpoint;
        let deadline = WindowsIoDeadline::new(StdDuration::from_secs(2));
        let pipe = open_windows_daemon_query_pipe(&windows_wide_null(&pipe_name), &deadline)?;
        let request = format!(
            "{}\n",
            serde_json::to_string(&compact_json(json!({
                "schema_version": 1,
                "op": "ping",
                "token": token,
            })))?
        );
        write_all_windows_daemon_query_pipe(&pipe, request.as_bytes(), &deadline)?;
        Ok(pipe)
    }

    #[test]
    fn daemon_query_activity_prevents_idle_shutdown_during_a_request() {
        let activity = Arc::new(DaemonQueryActivity::new());
        let request = activity.begin_request().expect("request accepted");
        let (active, generation) = activity.snapshot();

        assert_eq!(active, 1);
        assert!(!activity.try_stop_accepting_if_idle(generation));

        drop(request);
        let (active, completed_generation) = activity.snapshot();
        assert_eq!(active, 0);
        assert_ne!(completed_generation, generation);
        assert!(activity.try_stop_accepting_if_idle(completed_generation));
        assert!(activity.begin_request().is_none());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn stalled_query_client_is_discarded_and_next_query_is_served() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = start_test_query_service(temp.path())?;
        let stalled_client = connect_stalled_query_client(temp.path())?;
        wait_for_active_query(&service)?;

        let started = Instant::now();
        let response = daemon_query_request(
            temp.path(),
            compact_json(json!({
                "schema_version": 1,
                "op": "ping",
            })),
            StdDuration::from_secs(2),
            64 * 1024,
        )?
        .expect("query response");

        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
        assert!(started.elapsed() < StdDuration::from_secs(1));
        drop(stalled_client);
        drop(service);
        Ok(())
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn query_service_ping_stays_healthy_while_embedder_is_busy() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let embedder = Arc::new(Mutex::new(None));
        let service = start_daemon_query_service_with_request_timeout(
            temp.path(),
            embedder.clone(),
            TEST_QUERY_REQUEST_READ_TIMEOUT,
        )?;
        let _embedder_guard = embedder.lock().expect("test embedder lock");

        let started = Instant::now();
        let response = daemon_query_request(
            temp.path(),
            compact_json(json!({
                "schema_version": 1,
                "op": "ping",
            })),
            StdDuration::from_secs(1),
            64 * 1024,
        )?
        .expect("query response");

        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(response.get("busy").and_then(Value::as_bool), Some(true));
        assert!(response["embedding_runtime"].is_null());
        assert!(started.elapsed() < StdDuration::from_millis(500));
        drop(service);
        Ok(())
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn query_service_shutdown_is_bounded_with_stalled_client() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = start_test_query_service(temp.path())?;
        let stalled_client = connect_stalled_query_client(temp.path())?;
        wait_for_active_query(&service)?;

        let started = Instant::now();
        drop(service);

        assert!(started.elapsed() < StdDuration::from_secs(1));
        drop(stalled_client);
        Ok(())
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn valid_nonreading_client_does_not_block_later_queries_or_shutdown() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = start_test_query_service(temp.path())?;
        let nonreader = connect_valid_nonreading_query_client(temp.path())?;
        std::thread::sleep(StdDuration::from_millis(25));

        let response = daemon_query_request(
            temp.path(),
            compact_json(json!({
                "schema_version": 1,
                "op": "ping",
            })),
            StdDuration::from_secs(2),
            64 * 1024,
        )?
        .expect("query response");
        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));

        let started = Instant::now();
        drop(service);
        assert!(started.elapsed() < StdDuration::from_secs(1));
        drop(nonreader);
        Ok(())
    }

    #[test]
    fn observing_query_activity_resets_an_expired_idle_window() {
        let activity = Arc::new(DaemonQueryActivity::new());
        let request = activity.begin_request().expect("request accepted");
        let mut idle_since = Some(Instant::now() - StdDuration::from_secs(5));
        let mut observed_generation = 0;

        observe_daemon_query_activity(
            Some(activity.as_ref()),
            &mut idle_since,
            &mut observed_generation,
        );

        assert!(idle_since.is_none());
        assert!(!daemon_can_begin_idle_shutdown(
            Some(activity.as_ref()),
            observed_generation
        ));
        drop(request);
        observe_daemon_query_activity(
            Some(activity.as_ref()),
            &mut idle_since,
            &mut observed_generation,
        );
        assert!(idle_since.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn daemon_query_socket_uses_short_private_fallback_for_long_data_root() -> Result<()> {
        let data_root = PathBuf::from("/tmp").join("x".repeat(160));
        let (listener, path, runtime_dir) = bind_daemon_query_listener(&data_root)?;
        assert!(path.as_os_str().as_bytes().len() <= DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES);
        assert_ne!(path, daemon_query_socket_path(&data_root));
        let runtime_dir = runtime_dir.expect("long path should use a private runtime dir");
        assert_eq!(path.parent(), Some(runtime_dir.as_path()));

        drop(listener);
        fs::remove_file(&path)?;
        fs::remove_dir(&runtime_dir)?;
        Ok(())
    }

    #[test]
    fn daemon_query_request_reader_stops_at_newline() -> Result<()> {
        let mut cursor = std::io::Cursor::new(b"{\"op\":\"ping\"}\nignored".to_vec());

        let body = read_daemon_query_request(&mut cursor, 256)?;

        assert_eq!(body, "{\"op\":\"ping\"}");
        Ok(())
    }

    #[test]
    fn daemon_query_request_reader_rejects_oversized_request() {
        let mut cursor = std::io::Cursor::new(b"abcdef".to_vec());

        let error = read_daemon_query_request(&mut cursor, 3)
            .expect_err("oversized request should fail");

        assert!(format!("{error:#}").contains("daemon query request is too large"));
    }

    #[cfg(unix)]
    #[test]
    fn daemon_query_endpoint_roundtrips_unix_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let endpoint = DaemonQueryEndpoint::Unix {
            path: daemon_query_socket_path(temp.path()),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
        };

        write_daemon_query_endpoint(temp.path(), &endpoint)?;
        let loaded = read_daemon_query_endpoint(temp.path())?.expect("endpoint");

        match loaded {
            DaemonQueryEndpoint::Unix { path, token } => {
                assert_eq!(path, daemon_query_socket_path(temp.path()));
                assert_eq!(token, "0123456789abcdef0123456789abcdef");
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn daemon_query_endpoint_roundtrips_windows_named_pipe_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let pipe_name = daemon_query_pipe_name();
        assert!(windows_named_pipe_name_is_local(&pipe_name));
        let endpoint = DaemonQueryEndpoint::WindowsNamedPipe {
            pipe_name: pipe_name.clone(),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
        };

        write_daemon_query_endpoint(temp.path(), &endpoint)?;
        let loaded = read_daemon_query_endpoint(temp.path())?.expect("endpoint");

        match loaded {
            DaemonQueryEndpoint::WindowsNamedPipe {
                pipe_name: loaded_pipe_name,
                token,
            } => {
                assert_eq!(loaded_pipe_name, pipe_name);
                assert_eq!(token, "0123456789abcdef0123456789abcdef");
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn daemon_query_endpoint_rejects_nonlocal_windows_pipe_name() -> Result<()> {
        let temp = tempfile::tempdir()?;
        create_private_dir_all(&daemon_root_path(temp.path()))?;
        let endpoint = compact_json(json!({
            "schema_version": 1,
            "transport": "windows_named_pipe",
            "pipe_name": r"\\server\pipe\ctx-daemon-query-0123456789abcdef0123456789abcdef",
            "token": "0123456789abcdef0123456789abcdef",
        }));
        write_private_json_file(&daemon_query_endpoint_path(temp.path()), &endpoint)?;

        assert!(read_daemon_query_endpoint(temp.path())?.is_none());
        Ok(())
    }

    #[test]
    fn daemon_query_endpoint_rejects_short_tokens() -> Result<()> {
        let temp = tempfile::tempdir()?;
        create_private_dir_all(&daemon_root_path(temp.path()))?;
        let mut endpoint = compact_json(json!({
                "schema_version": 1,
                "transport": "unix",
                "token": "short",
        }));
        #[cfg(unix)]
        {
            endpoint["path"] = Value::String(
                daemon_query_socket_path(temp.path())
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        #[cfg(windows)]
        {
            endpoint["transport"] = Value::String("windows_named_pipe".to_owned());
            endpoint["pipe_name"] = Value::String(daemon_query_pipe_name());
        }
        write_private_json_file(&daemon_query_endpoint_path(temp.path()), &endpoint)?;

        assert!(read_daemon_query_endpoint(temp.path())?.is_none());
        Ok(())
    }
}

#[cfg(all(test, ctx_semantic_fastembed))]
mod fastembed_policy_tests {
    use super::*;

    #[test]
    fn cpu_model_file_verification_binds_size_and_sha256() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("model.bin");
        fs::write(&path, b"test")?;
        let expected = SemanticModelFile::new(
            "model.bin",
            4,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        );
        verify_semantic_cpu_file(&path, expected)?;

        let size_error = verify_semantic_cpu_file(
            &path,
            SemanticModelFile::new("model.bin", 5, expected.sha256),
        )
        .unwrap_err();
        assert!(
            size_error
                .downcast_ref::<SemanticCpuModelIntegrityError>()
                .is_some()
        );
        let hash_error = verify_semantic_cpu_file(
            &path,
            SemanticModelFile::new(
                "model.bin",
                4,
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
        )
        .unwrap_err();
        assert!(semantic_model_acquisition_integrity_error(&hash_error));
        Ok(())
    }

    #[test]
    fn cpu_model_publication_restores_old_root_and_preserves_siblings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
        let model_root = managed.join(SEMANTIC_HF_MODEL_CACHE_DIR);
        let shared = temp.path().join("shared-hf-cache");
        let partial_download = managed.join("download-cache/partial");
        fs::create_dir_all(&model_root)?;
        fs::create_dir_all(&shared)?;
        fs::create_dir_all(partial_download.parent().expect("partial parent"))?;
        fs::write(model_root.join("old"), b"old")?;
        fs::write(shared.join("keep"), b"shared")?;
        fs::write(&partial_download, b"partial")?;

        let missing_staging = managed.join("missing-staging");
        assert!(publish_semantic_cpu_model_root(&missing_staging, &model_root).is_err());
        assert_eq!(fs::read(model_root.join("old"))?, b"old");
        assert_eq!(fs::read(shared.join("keep"))?, b"shared");
        assert_eq!(fs::read(&partial_download)?, b"partial");

        let staging = managed.join("staging");
        fs::create_dir_all(&staging)?;
        fs::write(staging.join("new"), b"new")?;
        publish_semantic_cpu_model_root(&staging, &model_root)?;
        assert_eq!(fs::read(model_root.join("new"))?, b"new");
        assert!(!model_root.join("old").exists());
        assert_eq!(fs::read(shared.join("keep"))?, b"shared");
        assert_eq!(fs::read(&partial_download)?, b"partial");
        Ok(())
    }

    #[test]
    fn cpu_model_acquisition_lock_serializes_publishers() -> Result<()> {
        use fs2::FileExt;

        let temp = tempfile::tempdir()?;
        let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
        let first = lock_semantic_model_acquisition(&managed)?;
        let second_path = managed.join("acquisition.lock");
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&second_path)?;
        assert!(second.try_lock_exclusive().is_err());
        drop(first);
        second.lock_exclusive()?;
        FileExt::unlock(&second)?;
        Ok(())
    }

    #[test]
    fn cpu_model_load_defers_before_cache_or_runtime_access() {
        let temp = tempfile::tempdir().expect("tempdir");
        let policy = semantic_embed_policy_from_env_and_resources(
            SemanticComputeClass::Cpu,
            SemanticSystemResources {
                total_memory_bytes: Some(8 * 1024 * 1024 * 1024),
                available_memory_bytes: Some(1024),
                available_parallelism: 8,
            },
        );
        let error = match acquire_cpu_backend(
            temp.path(),
            policy,
            BackendPreference::Cpu,
            false,
        ) {
            Ok(_) => panic!("low-memory acquisition should defer"),
            Err(error) => error,
        };
        assert!(error.downcast_ref::<SemanticModelLoadDeferred>().is_some());
    }

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

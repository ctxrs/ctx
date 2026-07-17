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

    fn test_sha256(value: &str) -> String {
        format!("{:x}", Sha256::digest(value.as_bytes()))
    }

    fn test_source_hash(value: &str) -> String {
        if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            value.to_owned()
        } else {
            test_sha256(value)
        }
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
            source_text_hash: test_source_hash(source_hash),
            chunk_text_hash: test_sha256(&format!("{source_hash}-chunk-{chunk_index}")),
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
                "sidecar_trust_version": SEMANTIC_SIDECAR_TRUST_VERSION,
                "sidecar_generation": 7,
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
    fn semantic_worker_report_does_not_treat_untrusted_status_counts_as_zero() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_worker_status(
            temp.path(),
            &json!({
                "schema_version": 1,
                "status": "ready",
                "model_key": semantic_model_key(),
                "searchable_items": 10,
                "embedded_items": 10,
                "embedded_chunks": 20,
            }),
        )?;

        let report = semantic_worker_report_best_effort(temp.path()).to_json();
        assert_eq!(report["status"], "unknown");
        assert_eq!(report["coverage"]["sidecar_stats_known"], false);
        assert_eq!(report["coverage"]["coverage_ratio"], Value::Null);
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
        assert_eq!(value["required_available_memory_bytes"], 2_147_483_648_u64);
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

        let daemon = daemon_report(
            temp.path(),
            &semantic_worker_report_best_effort(temp.path()),
        );
        let semantic = &daemon["jobs"]["semantic_index"];
        assert_eq!(semantic["status"], "unknown");
        assert_eq!(semantic["reason"], "searchable_items_unknown");
        assert_eq!(semantic["last_run_status"], Value::Null);
        assert_eq!(semantic["indexed_chunks"], Value::Null);
        Ok(())
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
        assert_eq!(vector_store.bounded_dirty_event_count()?, 0);
        drop(vector_store);

        let mut updated_assistant = assistant.clone();
        updated_assistant.payload = json!({ "text": "updated assistant answer" });
        updated_assistant.occurred_at = utc_now();
        store.upsert_event(&updated_assistant)?;

        assert_eq!(
            queue_recent_semantic_work(data_root, &store, "test_recent")?,
            1
        );
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
        let query = ctx_protocol::SearchQuery::new(vec![ctx_protocol::SearchClause::semantic(
            "semantic daemon scheduling fixture",
        )])
        .canonicalized()?;
        let err = search_packet_query_with_backend(
            &store,
            temp.path(),
            &query,
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            true,
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
        let query = ctx_protocol::SearchQuery::new(vec![ctx_protocol::SearchClause::all(
            "semantic daemon scheduling fixture",
        )])
        .canonicalized()?;

        let (packet, retrieval) = search_packet_query_with_backend(
            &store,
            temp.path(),
            &query,
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            false,
            RefreshArg::Off,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert!(packet.query_execution.semantic.attempted);
        assert!(!packet.query_execution.semantic.required);
        assert_eq!(
            packet.query_execution.semantic.readiness,
            ctx_protocol::SearchSemanticReadiness::NotReady
        );
        assert_eq!(
            packet.query_execution.semantic.effective_backend,
            ctx_protocol::SearchEffectiveBackend::Lexical
        );
        assert_eq!(
            packet.query_execution.semantic.skip_reason,
            Some(ctx_protocol::SearchSemanticSkipReason::NotReady)
        );
        assert_eq!(packet.query, "semantic daemon scheduling fixture");
        assert!(!vector_path.exists());
        Ok(())
    }

    #[test]
    fn file_only_hybrid_search_stays_bounded_and_lexical() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        let mut options = ctx_history_search::PacketOptions::default();
        options.filters.file = Some("src/lib.rs".to_owned());

        let (packet, retrieval) = search_packet_file_filter_with_backend(
            &store,
            &options,
            SearchBackendArg::Hybrid,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(
            packet.query_execution.candidate_strategy,
            "indexed_file_touch_bounded"
        );
        assert!(!packet.query_execution.semantic.attempted);
        assert!(!packet.query_execution.semantic.required);
        assert_eq!(
            packet.query_execution.semantic.effective_backend,
            ctx_protocol::SearchEffectiveBackend::Lexical
        );
        assert_eq!(
            packet.query_execution.semantic.completeness,
            ctx_protocol::SearchSemanticCompleteness::NotAttempted
        );
        assert_eq!(
            packet.query_execution.semantic.skip_reason,
            Some(ctx_protocol::SearchSemanticSkipReason::QueryShapeNotEligible)
        );
        Ok(())
    }

    #[test]
    fn file_only_semantic_backend_requires_an_explicit_clause() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        let mut options = ctx_history_search::PacketOptions::default();
        options.filters.file = Some("src/lib.rs".to_owned());

        let error = search_packet_file_filter_with_backend(
            &store,
            &options,
            SearchBackendArg::Semantic,
            false,
        )
        .expect_err("file-only search has no explicit semantic clause");

        assert!(format!("{error:#}").contains("requires one explicit --semantic clause"));
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
        let lexical_query = ctx_protocol::SearchQuery::new(vec![ctx_protocol::SearchClause::all(
            "semantic daemon scheduling fixture",
        )])
        .canonicalized()?;
        let (packet, retrieval) = search_packet_query_with_backend(
            &store,
            temp.path(),
            &lexical_query,
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            true,
            RefreshArg::Off,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(
            packet.query_execution.semantic.effective_backend,
            ctx_protocol::SearchEffectiveBackend::Lexical
        );
        assert_eq!(
            packet.query_execution.semantic.readiness,
            ctx_protocol::SearchSemanticReadiness::Unavailable
        );
        assert_eq!(
            packet.query_execution.semantic.skip_reason,
            Some(ctx_protocol::SearchSemanticSkipReason::Unavailable)
        );
        assert_eq!(packet.query, "semantic daemon scheduling fixture");

        let semantic_query =
            ctx_protocol::SearchQuery::new(vec![ctx_protocol::SearchClause::semantic(
                "semantic daemon scheduling fixture",
            )])
            .canonicalized()?;
        let err = search_packet_query_with_backend(
            &store,
            temp.path(),
            &semantic_query,
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            true,
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

        assert_eq!(
            semantic_worker_cache_dir_from_env(&data_root, &env),
            explicit
        );
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

        let first = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
        )?;
        let second = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
        )?;

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
        let iteration = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
        )?;

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
        let iteration = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
        )?;

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
        assert_eq!(
            daemon["jobs"]["semantic_index"]["reason"],
            "semantic_disabled"
        );
        assert!(!semantic_vector_path(temp.path()).exists());
        Ok(())
    }

    #[test]
    fn daemon_history_refresh_runs_when_store_is_not_ready() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_enabled_config(temp.path())?;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            None,
        );

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
        )?;

        assert!(!iteration.failed);
        assert_eq!(calls.borrow().first(), Some(&"history_refresh"));
        let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
        assert_eq!(daemon["jobs"]["history_refresh"]["status"], "completed");
        assert_ne!(
            daemon["jobs"]["history_refresh"]["reason"],
            "semantic_bootstrap_in_progress"
        );
        assert_eq!(
            daemon["jobs"]["semantic_index"]["last_run_status"],
            "skipped"
        );
        assert_eq!(
            daemon["jobs"]["semantic_index"]["last_run_reason"],
            "store_missing"
        );
        Ok(())
    }

    #[test]
    fn terminal_sidecar_maintenance_degrades_semantic_without_stopping_daemon() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_semantic_enabled_config(temp.path())?;
        let _store = Store::open(database_path(temp.path().to_path_buf()))?;
        {
            let vector_path = semantic_vector_path(temp.path());
            let mut vector_store = SemanticVectorStore::open(&vector_path)?;
            vector_store.conn.execute(
                "UPDATE embedding_models SET dimensions = dimensions + 1 WHERE model_key = ?1",
                [semantic_model_key()],
            )?;
            let error = vector_store
                .run_maintenance_slice()
                .expect_err("model mismatch");
            let message =
                semantic_terminal_maintenance_message(&error).expect("terminal maintenance");
            vector_store.record_terminal_maintenance_failure(&message)?;
        }
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_test_daemon_jobs(
            calls.clone(),
            Some(daemon_history_completed_test_job()),
            None,
        );

        let mut runtime = DaemonRuntime::default();
        let iteration = run_daemon_once(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
        )?;

        assert!(!iteration.failed);
        assert_eq!(calls.borrow().first(), Some(&"history_refresh"));
        let semantic_job = read_daemon_job_status(&daemon_semantic_job_path(temp.path()))
            .expect("semantic job status");
        assert_eq!(semantic_job["status"], "degraded");
        assert_eq!(semantic_job["reason"], "sidecar_maintenance_terminal");
        assert_eq!(semantic_job["retryable"], false);
        let report = semantic_worker_report_for_daemon(temp.path());
        assert_eq!(report.status, "degraded");
        assert!(report.last_error.is_some());
        Ok(())
    }

    #[test]
    fn sqlite_vec0_binary_scan_matches_filtered_exact_rerank() -> Result<()> {
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
        store.sync_sqlite_vec0_from_chunks_if_needed()?;

        assert!(store.sqlite_vec0_ready()?);

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let filtered_hits = store.search_event_ids(&query, &[close_event, far_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(
            filtered_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(filtered_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, close_event);
        assert_eq!(filtered_hits.hits[0].event_id, close_event);
        assert_eq!(sqlite_hits.hits[1].event_id, far_event);
        assert_eq!(filtered_hits.hits[1].event_id, far_event);
        Ok(())
    }

    #[test]
    fn read_only_search_fails_closed_while_vec0_is_not_ready() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        let event_id = Uuid::new_v4();
        {
            let mut store = SemanticVectorStore::open(&vector_path)?;
            store.upsert_chunk_embeddings(&[(
                test_chunk(event_id, 1, "interrupted"),
                test_embedding(1.0, 0.0),
            )])?;
            store.sync_sqlite_vec0_from_chunks_if_needed()?;
            store.set_maintenance_state_i64(SQLITE_VEC0_READY_STATE_KEY, 0)?;
        }

        let store = SemanticVectorStore::open_read_only(&vector_path)?.expect("vector store");
        assert!(!store.sqlite_vec0_search_ready()?);
        let error = store
            .search(&test_embedding(1.0, 0.0), 1)
            .expect_err("read-only search must not scan canonical vectors");
        assert!(error
            .chain()
            .any(|cause| cause.downcast_ref::<SemanticVectorStorePending>().is_some()));
        assert_eq!(
            store.maintenance_state_i64(SQLITE_VEC0_READY_STATE_KEY)?,
            Some(0),
            "read-only search must not repair vec0"
        );
        drop(store);

        let mut store = SemanticVectorStore::open(&vector_path)?;
        assert!(!store.sqlite_vec0_search_ready()?);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);
        Ok(())
    }

    #[test]
    fn canonical_write_during_projection_rebuild_cannot_publish_stale_slot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let first_event = Uuid::new_v4();
        let second_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(first_event, 1, "first"),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk(second_event, 2, "second"),
                test_embedding(0.0, 1.0),
            ),
        ])?;
        store.set_maintenance_state_i64(MAINTENANCE_PAGE_UNITS_STATE_KEY, 1)?;

        store.run_maintenance_slice()?;
        store.run_maintenance_slice()?;
        store.run_maintenance_slice()?;
        let stale_target_generation = store
            .maintenance_state_i64(SQLITE_VEC0_BUILD_GENERATION_STATE_KEY)?
            .expect("projection build generation");
        assert!(!store.sqlite_vec0_search_ready()?);

        let replacement = test_embedding(0.8, 0.6);
        store.upsert_chunk_embeddings(&[(
            test_chunk(first_event, 3, "replacement"),
            replacement.clone(),
        )])?;
        let canonical_generation = store
            .maintenance_state_i64(CANONICAL_GENERATION_STATE_KEY)?
            .expect("canonical generation");
        assert_ne!(canonical_generation, stale_target_generation);

        let after_mutation = store.run_maintenance_slice()?;
        assert!(!after_mutation.is_ready());
        assert!(!store.sqlite_vec0_search_ready()?);
        assert_eq!(
            store.maintenance_state_i64(SQLITE_VEC0_BUILD_GENERATION_STATE_KEY)?,
            Some(canonical_generation)
        );

        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        let search = store.search(&replacement, 2)?;
        assert_eq!(
            search.hits.first().map(|hit| hit.event_id),
            Some(first_event)
        );
        assert!(!search.hits.iter().any(|hit| {
            hit.event_id == first_event && hit.source_text_hash == test_source_hash("first")
        }));
        Ok(())
    }

    #[test]
    fn projection_search_is_model_dimension_and_deadline_bounded() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let active_event = Uuid::new_v4();
        let foreign_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(active_event, 1, "active"),
            test_embedding(0.0, 1.0),
        )])?;
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        store.conn.execute(
            r#"
            INSERT INTO event_embedding_chunks
                (event_id, model_key, event_seq, chunk_index, chunk_count,
                 source_text_sha256, chunk_text_sha256, start_char, end_char,
                 dimensions, embedding_f32, embedded_at_ms)
            VALUES (?1, 'foreign:model', 2, 0, 1, 'foreign', 'foreign', 0, 1, ?2, ?3, 1)
            "#,
            params![
                foreign_event.to_string(),
                SEMANTIC_DIMENSIONS as i64,
                serialize_f32_blob(&test_embedding(1.0, 0.0))
            ],
        )?;

        let search = store.search_event_ids(&test_embedding(1.0, 0.0), &[active_event], 2)?;
        assert_eq!(
            search
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            vec![active_event]
        );
        let deadline_error = store
            .search_until(&test_embedding(1.0, 0.0), 1, Instant::now())
            .expect_err("elapsed retrieval deadline must fail closed");
        assert!(deadline_error
            .chain()
            .any(|cause| cause.downcast_ref::<SemanticVectorStorePending>().is_some()));
        let too_many_ids =
            vec![active_event; query_service_contract::SEMANTIC_QUERY_MAX_CANDIDATE_EVENT_IDS + 1];
        let cap_error = store
            .search_event_ids_until(
                &test_embedding(1.0, 0.0),
                &too_many_ids,
                1,
                Instant::now() + StdDuration::from_secs(1),
            )
            .expect_err("oversized candidate set must fail closed");
        assert!(cap_error
            .chain()
            .any(|cause| cause.downcast_ref::<SemanticVectorStorePending>().is_some()));
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
        store.sync_sqlite_vec0_from_chunks_if_needed()?;

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
    fn sqlite_vec0_respects_the_shared_candidate_row_allocation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let chunks = (0..8)
            .map(|index| {
                let event_id = Uuid::new_v4();
                (
                    test_chunk(event_id, index + 1, &format!("bounded-{index}")),
                    test_embedding(1.0 - (index as f32 * 0.05), index as f32 * 0.05),
                )
            })
            .collect::<Vec<_>>();
        store.upsert_chunk_embeddings(&chunks)?;
        store.sync_sqlite_vec0_from_chunks_if_needed()?;

        let search = store.search_until_bounded(
            &test_embedding(1.0, 0.0),
            8,
            3,
            Instant::now() + StdDuration::from_secs(1),
        )?;

        assert!(search.stats.events_scored <= 3);
        assert!(search.hits.len() <= 3);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_binary_scan_reports_and_enforces_its_byte_envelope() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let event_id = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(event_id, 1, "bounded"),
            test_embedding(1.0, 0.0),
        )])?;
        store.sync_sqlite_vec0_from_chunks_if_needed()?;

        let search = store.search(&test_embedding(1.0, 0.0), 1)?;
        assert_eq!(search.stats.chunks_scanned, 1);
        assert_eq!(
            search.stats.vector_bytes_read,
            SEMANTIC_BINARY_VECTOR_BYTES + semantic_exact_vector_bytes()
        );
        assert_eq!(search.stats.events_scored, 1);

        store.conn.execute(
            r#"
            UPDATE semantic_index_stats
            SET embedded_chunks = 1800000
            WHERE model_key = ?1
            "#,
            params![semantic_model_key()],
        )?;
        assert!(semantic_full_corpus_vector_scan_ready(&store)?);

        let exact_rerank_bytes = SEMANTIC_SQLITE_VEC0_MAX_K * semantic_exact_vector_bytes();
        let chunk_limit = (SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES - exact_rerank_bytes)
            / SEMANTIC_BINARY_VECTOR_BYTES;
        let oversized_stats = SemanticSidecarStats {
            embedded_items: 1,
            embedded_chunks: chunk_limit.saturating_add(1),
        };
        assert!(
            semantic_sqlite_vec0_scan_bytes(
                oversized_stats.embedded_chunks,
                semantic_sqlite_vec0_candidate_limit(
                    1,
                    SEMANTIC_SQLITE_VEC0_MAX_K,
                    oversized_stats.embedded_chunks
                ),
            )
            .is_some_and(|bytes| bytes <= SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES),
            "a small requested rerank must not weaken the full-scan envelope"
        );
        assert!(!semantic_sqlite_vec0_full_scan_ready(oversized_stats));
        store.conn.execute(
            r#"
            UPDATE semantic_index_stats
            SET embedded_chunks = ?2
            WHERE model_key = ?1
            "#,
            params![semantic_model_key(), oversized_stats.embedded_chunks as i64],
        )?;
        assert!(!semantic_full_corpus_vector_scan_ready(&store)?);
        let error = store
            .search(&test_embedding(1.0, 0.0), 1)
            .expect_err("oversized binary corpus must fail before vec0");
        assert!(error
            .to_string()
            .contains("exceeds the bounded sqlite vec0 binary scan"));
        assert!(error
            .chain()
            .any(|cause| cause.downcast_ref::<SemanticVectorStorePending>().is_some()));
        let filtered = store.search_event_ids(&test_embedding(1.0, 0.0), &[event_id], 1)?;
        assert_eq!(
            filtered.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(
            filtered.hits.first().map(|hit| hit.event_id),
            Some(event_id)
        );
        Ok(())
    }

    #[test]
    fn semantic_search_requires_the_trusted_active_projection() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let event_id = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(event_id, 1, "pending"),
            test_embedding(1.0, 0.0),
        )])?;
        assert!(!semantic_full_corpus_vector_scan_ready(&store)?);
        assert!(store
            .search_event_ids(&test_embedding(1.0, 0.0), &[event_id], 1)
            .is_err());
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(semantic_full_corpus_vector_scan_ready(&store)?);
        assert_eq!(
            store
                .search_event_ids(&test_embedding(1.0, 0.0), &[event_id], 1)?
                .hits[0]
                .event_id,
            event_id
        );
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
    fn cached_stats_track_multi_chunk_replacement_and_delete() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let multi_chunk_event = Uuid::new_v4();
        let single_chunk_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk_at(multi_chunk_event, 2, "multi-v1", 0, 3),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi-v1", 1, 3),
                test_embedding(0.9, 0.1),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi-v1", 2, 3),
                test_embedding(0.8, 0.2),
            ),
            (
                test_chunk(single_chunk_event, 1, "single"),
                test_embedding(0.0, 1.0),
            ),
        ])?;

        let stats = store.cached_stats()?.expect("cached stats");
        assert_eq!(stats.embedded_items, 2);
        assert_eq!(stats.embedded_chunks, 4);

        store.conn.execute(
            "DELETE FROM semantic_index_stats WHERE model_key = ?1",
            [semantic_model_key()],
        )?;
        drop(store);
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        assert_eq!(store.cached_stats()?, None);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        let stats = store.cached_stats()?.expect("rebuilt cached stats");
        assert_eq!(stats.embedded_items, 2);
        assert_eq!(stats.embedded_chunks, 4);

        store.upsert_chunk_embeddings(&[
            (
                test_chunk_at(multi_chunk_event, 3, "multi-v2", 0, 2),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk_at(multi_chunk_event, 3, "multi-v2", 1, 2),
                test_embedding(0.95, 0.05),
            ),
        ])?;
        let stats = store.cached_stats()?.expect("cached stats");
        assert_eq!(stats.embedded_items, 2);
        assert_eq!(stats.embedded_chunks, 3);

        assert_eq!(
            store
                .delete_embedding_chunks_for_event_ids(&[multi_chunk_event, multi_chunk_event,])?,
            2
        );
        assert!(store.sqlite_vec0_ready()?);
        let stats = store.cached_stats()?.expect("cached stats");
        assert_eq!(stats.embedded_items, 1);
        assert_eq!(stats.embedded_chunks, 1);

        assert_eq!(
            store.delete_embedding_chunks_for_event_ids(&[single_chunk_event])?,
            1
        );
        assert!(store.sqlite_vec0_ready()?);
        assert_eq!(
            store.delete_embedding_chunks_for_event_ids(&[single_chunk_event])?,
            0
        );
        let stats = store.cached_stats()?.expect("cached stats");
        assert_eq!(stats.embedded_items, 0);
        assert_eq!(stats.embedded_chunks, 0);
        Ok(())
    }

    #[test]
    fn cached_stats_underflow_invalidates_trust() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let event_id = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(event_id, 1, "underflow"),
            test_embedding(1.0, 0.0),
        )])?;
        store.conn.execute(
            "UPDATE semantic_index_stats SET embedded_items = 0, embedded_chunks = 0 WHERE model_key = ?1",
            [semantic_model_key()],
        )?;

        assert_eq!(store.delete_embedding_chunks_for_event_ids(&[event_id])?, 1);
        assert_eq!(store.cached_stats()?, None);
        let (items, chunks, trust) = store.conn.query_row(
            "SELECT embedded_items, embedded_chunks, trust_version FROM semantic_index_stats WHERE model_key = ?1",
            [semantic_model_key()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        assert_eq!((items, chunks, trust), (0, 0, 0));
        assert_eq!(
            store.sidecar_trust_state()?,
            SemanticSidecarTrustState::Pending
        );
        Ok(())
    }

    #[test]
    fn active_model_tuple_mismatch_is_terminal_until_fingerprint_changes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        store.conn.execute(
            "UPDATE embedding_models SET dimensions = dimensions + 1 WHERE model_key = ?1",
            [semantic_model_key()],
        )?;

        let error = store.run_maintenance_slice().expect_err("model mismatch");
        let message = semantic_terminal_maintenance_message(&error).expect("typed terminal error");
        store.record_terminal_maintenance_failure(&message)?;
        assert_eq!(
            store.active_terminal_maintenance_failure()?,
            Some(message.clone())
        );

        store.conn.execute(
            "UPDATE embedding_models SET dimensions = ?2 WHERE model_key = ?1",
            params![semantic_model_key(), SEMANTIC_DIMENSIONS as i64],
        )?;
        assert_eq!(store.active_terminal_maintenance_failure()?, None);
        Ok(())
    }

    #[test]
    fn reopening_vector_store_does_not_read_corpus_tables() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        {
            let mut store = SemanticVectorStore::open(&vector_path)?;
            store.upsert_chunk_embeddings(&[(
                test_chunk(Uuid::new_v4(), 1, "ready"),
                test_embedding(1.0, 0.0),
            )])?;
        }

        let conn = Connection::open(&vector_path)?;
        let mut store = SemanticVectorStore {
            conn,
            path: vector_path,
        };
        let denied_reads = Arc::new(Mutex::new(Vec::new()));
        let callback_denied_reads = Arc::clone(&denied_reads);
        store
            .conn
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if let rusqlite::hooks::AuthAction::Read { table_name, .. } = context.action {
                    if matches!(
                        table_name,
                        "event_embeddings"
                            | "event_embedding_chunks"
                            | "semantic_index_stats"
                            | "semantic_event_summary"
                            | "event_embedding_vec0_v3"
                            | "event_embedding_vec0_meta_v3"
                    ) {
                        callback_denied_reads
                            .lock()
                            .expect("denied-read lock")
                            .push(table_name.to_owned());
                        return rusqlite::hooks::Authorization::Deny;
                    }
                }
                rusqlite::hooks::Authorization::Allow
            }));

        store.ensure_schema()?;
        assert!(
            denied_reads.lock().expect("denied-read lock").is_empty(),
            "reopen attempted a corpus read"
        );
        Ok(())
    }

    #[test]
    fn plaintext_sanitation_cursor_resumes_after_reopen() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        {
            let mut store = SemanticVectorStore::open(&vector_path)?;
            let chunks = (0..=SEMANTIC_SIDECAR_MAINTENANCE_ROWS)
                .map(|index| {
                    (
                        test_chunk(Uuid::new_v4(), index as u64 + 1, "plaintext"),
                        test_embedding(1.0, 0.0),
                    )
                })
                .collect::<Vec<_>>();
            store.upsert_chunk_embeddings(&chunks)?;
            store.conn.execute(
                "UPDATE event_embedding_chunks SET chunk_text = 'legacy plaintext'",
                [],
            )?;
            let tx = store.conn.transaction()?;
            SemanticVectorStore::set_global_maintenance_state_i64_in_transaction(
                &tx,
                PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY,
                0,
            )?;
            SemanticVectorStore::set_global_maintenance_state_i64_in_transaction(
                &tx,
                PLAINTEXT_SANITIZE_CURSOR_VERSION_GLOBAL_STATE_KEY,
                0,
            )?;
            tx.commit()?;
            assert_eq!(store.run_maintenance_slice()?.rows_processed, 0);
            let first = store.run_maintenance_slice()?;
            assert_eq!(first.rows_processed, SEMANTIC_SIDECAR_MAINTENANCE_ROWS);
            let trailing_plaintext = store.conn.query_row(
                "SELECT chunk_text != '' FROM event_embedding_chunks ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get::<_, bool>(0),
            )?;
            assert!(trailing_plaintext);
        }

        let mut store = SemanticVectorStore::open(&vector_path)?;
        let resumed = store.run_maintenance_slice()?;
        assert_eq!(resumed.rows_processed, 1);
        let trailing_plaintext = store.conn.query_row(
            "SELECT chunk_text != '' FROM event_embedding_chunks ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        assert!(!trailing_plaintext);
        let published = store.run_maintenance_slice()?;
        assert!(!published.is_ready());
        assert_eq!(
            store.global_maintenance_state_i64(PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY)?,
            Some(PLAINTEXT_SANITIZED_STATE_VERSION)
        );
        Ok(())
    }

    #[test]
    fn successful_maintenance_restores_adaptive_page_units_toward_default() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        store.set_maintenance_state_i64(MAINTENANCE_PAGE_UNITS_STATE_KEY, 1)?;

        store.run_maintenance_slice()?;

        assert_eq!(
            store.maintenance_state_i64(MAINTENANCE_PAGE_UNITS_STATE_KEY)?,
            Some(2)
        );
        store.grow_maintenance_page_units_after_success(40)?;
        assert_eq!(
            store.maintenance_state_i64(MAINTENANCE_PAGE_UNITS_STATE_KEY)?,
            Some(SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64)
        );
        Ok(())
    }

    #[test]
    fn oversized_legacy_plaintext_is_terminal_without_mutation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let event_id = Uuid::new_v4();
        store.conn.execute(
            r#"
            INSERT INTO event_embeddings
                (event_id, model_key, event_seq, text_sha256, preview_text,
                 dimensions, embedding_f32, embedded_at_ms)
            VALUES (?1, ?2, 1, 'legacy', zeroblob(?3), ?4, ?5, 1)
            "#,
            params![
                event_id.to_string(),
                semantic_model_key(),
                SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES as i64 + 1,
                SEMANTIC_DIMENSIONS as i64,
                serialize_f32_blob(&test_embedding(1.0, 0.0)),
            ],
        )?;
        let tx = store.conn.transaction()?;
        SemanticVectorStore::set_global_maintenance_state_i64_in_transaction(
            &tx,
            PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY,
            0,
        )?;
        tx.commit()?;

        let error = store
            .run_maintenance_slice()
            .expect_err("oversized row must fail closed");
        assert!(semantic_terminal_maintenance_message(&error).is_some());
        let remaining = store.conn.query_row(
            "SELECT length(preview_text) FROM event_embeddings WHERE event_id = ?1",
            [event_id.to_string()],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(remaining, SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES as i64 + 1);
        assert_eq!(
            store.global_maintenance_state_i64(PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY)?,
            Some(0)
        );
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
            vector_store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_items,
            SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
        );
        assert_eq!(
            vector_store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_chunks,
            SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
        );

        let slot = vector_store
            .maintenance_state_i64(SUMMARY_ACTIVE_SLOT_STATE_KEY)?
            .expect("summary slot");
        let first_plan = {
            let mut stmt = vector_store.conn.prepare(
                r#"
                EXPLAIN QUERY PLAN
                SELECT event_id, source_text_sha256, single_source_hash, event_seq, chunk_count
                FROM semantic_event_summary
                WHERE slot = ?1 AND model_key = ?2
                ORDER BY event_seq DESC, event_id DESC
                LIMIT ?3
                "#,
            )?;
            let rows = stmt.query_map(
                params![
                    slot,
                    semantic_model_key(),
                    SEMANTIC_PRUNE_EVENTS_PER_PASS as i64
                ],
                |row| row.get::<_, String>(3),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?.join("\n")
        };
        assert!(
            first_plan.contains("idx_semantic_event_summary_prune"),
            "{first_plan}"
        );
        assert!(
            !first_plan.contains("SCAN semantic_event_summary"),
            "{first_plan}"
        );
        assert!(!first_plan.contains("USE TEMP B-TREE"), "{first_plan}");
        let continuation_plan = {
            let mut stmt = vector_store.conn.prepare(
                r#"
                EXPLAIN QUERY PLAN
                SELECT event_id, source_text_sha256, single_source_hash, event_seq, chunk_count
                FROM semantic_event_summary
                WHERE slot = ?1 AND model_key = ?2
                  AND (event_seq, event_id) < (?3, ?4)
                ORDER BY event_seq DESC, event_id DESC
                LIMIT ?5
                "#,
            )?;
            let rows = stmt.query_map(
                params![
                    slot,
                    semantic_model_key(),
                    i64::MAX,
                    Uuid::nil().to_string(),
                    SEMANTIC_PRUNE_EVENTS_PER_PASS as i64
                ],
                |row| row.get::<_, String>(3),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?.join("\n")
        };
        assert!(
            continuation_plan.contains("idx_semantic_event_summary_prune"),
            "{continuation_plan}"
        );
        assert!(
            !continuation_plan.contains("SCAN semantic_event_summary"),
            "{continuation_plan}"
        );
        assert!(
            !continuation_plan.contains("USE TEMP B-TREE"),
            "{continuation_plan}"
        );

        let first = vector_store.prune_ineligible_events(&store)?;
        assert_eq!(first.queued_stale_events, SEMANTIC_PRUNE_EVENTS_PER_PASS);
        assert_eq!(
            vector_store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_items,
            1,
            "first pass should leave the oldest event for the next cursor page"
        );
        assert_eq!(
            vector_store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_chunks,
            1
        );

        let second = vector_store.prune_ineligible_events(&store)?;
        assert_eq!(second.queued_stale_events, 1);
        assert_eq!(
            vector_store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_items,
            0
        );
        assert_eq!(
            vector_store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_chunks,
            0
        );
        assert_eq!(
            vector_store.bounded_dirty_event_count()?,
            SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
        );
        Ok(())
    }

    #[test]
    fn prune_rejects_an_event_larger_than_one_row_slice() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let docs = write_searchable_store(temp.path(), 1)?;
        let history = Store::open(database_path(temp.path().to_path_buf()))?;
        let event_id = docs[0].event_id;
        let mut store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
        let chunks = (0..=SEMANTIC_SIDECAR_MAINTENANCE_ROWS)
            .map(|chunk_index| {
                (
                    test_chunk_at(
                        event_id,
                        docs[0].seq,
                        "oversized-prune",
                        chunk_index,
                        SEMANTIC_SIDECAR_MAINTENANCE_ROWS + 1,
                    ),
                    test_embedding(1.0, 0.0),
                )
            })
            .collect::<Vec<_>>();
        store.upsert_chunk_embeddings(&chunks)?;

        let error = store
            .prune_ineligible_events(&history)
            .expect_err("oversized summary event must fail closed");

        assert!(semantic_terminal_maintenance_message(&error).is_some());
        assert_eq!(
            store
                .cached_stats()?
                .expect("trusted stats")
                .embedded_chunks,
            SEMANTIC_SIDECAR_MAINTENANCE_ROWS + 1
        );
        Ok(())
    }

    #[test]
    fn maintenance_and_dirty_pages_use_bounded_composite_indexes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        assert!(store.ensure_sqlite_vec0_schema_for_maintenance()?);

        for (sql, query_params, expected_index) in [
            (
                format!(
                    r#"
                    EXPLAIN QUERY PLAN
                    SELECT rowid FROM {SQLITE_VEC0_META_TABLE}
                    WHERE slot = ?1 AND model_key = ?2
                    ORDER BY rowid
                    LIMIT ?3
                    "#
                ),
                vec![
                    SqlValue::from(0_i64),
                    SqlValue::from(semantic_model_key().to_owned()),
                    SqlValue::from(SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64),
                ],
                SQLITE_VEC0_WORK_INDEX,
            ),
            (
                format!(
                    r#"
                    EXPLAIN QUERY PLAN
                    SELECT rowid, canonical_rowid FROM {SQLITE_VEC0_META_TABLE}
                    WHERE slot = ?1 AND model_key = ?2 AND rowid > ?3
                    ORDER BY rowid
                    LIMIT ?4
                    "#
                ),
                vec![
                    SqlValue::from(0_i64),
                    SqlValue::from(semantic_model_key().to_owned()),
                    SqlValue::from(1_i64),
                    SqlValue::from(SEMANTIC_SIDECAR_MAINTENANCE_ROWS as i64),
                ],
                SQLITE_VEC0_WORK_INDEX,
            ),
        ] {
            let mut stmt = store.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(query_params), |row| {
                row.get::<_, String>(3)
            })?;
            let plan = rows.collect::<rusqlite::Result<Vec<_>>>()?.join("\n");
            assert!(plan.contains(expected_index), "{plan}");
            assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");
        }

        for sql in [
            r#"
            EXPLAIN QUERY PLAN
            SELECT event_id, priority_seq, queued_at_ms
            FROM semantic_dirty_events
            WHERE model_key = ?1 AND priority_seq IS NOT NULL
            ORDER BY priority_seq DESC, queued_at_ms DESC
            LIMIT ?2
            "#,
            r#"
            EXPLAIN QUERY PLAN
            SELECT event_id
            FROM semantic_dirty_events
            WHERE model_key = ?1 AND priority_seq IS NULL
            ORDER BY queued_at_ms ASC
            LIMIT ?2
            "#,
        ] {
            let mut stmt = store.conn.prepare(sql)?;
            let rows = stmt.query_map(
                params![
                    semantic_model_key(),
                    SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT as i64
                ],
                |row| row.get::<_, String>(3),
            )?;
            let plan = rows.collect::<rusqlite::Result<Vec<_>>>()?.join("\n");
            assert!(
                plan.contains("idx_semantic_dirty_events_model_priority"),
                "{plan}"
            );
            assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");
        }
        Ok(())
    }

    #[test]
    fn vec0_schema_validation_rejects_wrong_metric_missing_and_extra_columns() -> Result<()> {
        {
            let temp = tempfile::tempdir()?;
            let store = SemanticVectorStore::open(&temp.path().join("wrong-metric.sqlite"))?;
            assert!(store.ensure_sqlite_vec0_schema_for_maintenance()?);
            store
                .conn
                .execute_batch(&format!("DROP TABLE {SQLITE_VEC0_TABLE};"))?;
            store.conn.execute_batch(&format!(
                r#"
                CREATE VIRTUAL TABLE {SQLITE_VEC0_TABLE}
                USING vec0(
                    embedding float[{SEMANTIC_DIMENSIONS}] distance_metric=l2,
                    embedding_coarse bit[{SEMANTIC_DIMENSIONS}],
                    slot INTEGER PARTITION KEY,
                    model_key TEXT PARTITION KEY
                );
                "#
            ))?;
            assert!(!store.sqlite_vec0_schema_compatible()?);
        }

        {
            let temp = tempfile::tempdir()?;
            let store = SemanticVectorStore::open(&temp.path().join("missing-column.sqlite"))?;
            assert!(store.ensure_sqlite_vec0_schema_for_maintenance()?);
            store.conn.execute_batch(&format!(
                "ALTER TABLE {SQLITE_VEC0_META_TABLE} DROP COLUMN end_char;"
            ))?;
            assert!(!store.sqlite_vec0_schema_compatible()?);
        }

        {
            let temp = tempfile::tempdir()?;
            let store = SemanticVectorStore::open(&temp.path().join("extra-column.sqlite"))?;
            assert!(store.ensure_sqlite_vec0_schema_for_maintenance()?);
            store.conn.execute_batch(&format!(
                "ALTER TABLE {SQLITE_VEC0_META_TABLE} ADD COLUMN unexpected TEXT;"
            ))?;
            assert!(!store.sqlite_vec0_schema_compatible()?);
        }
        Ok(())
    }

    #[test]
    fn partial_vec0_schema_is_terminal_and_never_dropped() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        store.conn.execute_batch(&format!(
            r#"
            CREATE TABLE {SQLITE_VEC0_META_TABLE} (
                rowid INTEGER PRIMARY KEY,
                marker TEXT NOT NULL
            );
            INSERT INTO {SQLITE_VEC0_META_TABLE}(rowid, marker) VALUES (7, 'preserve');
            "#
        ))?;

        let error = store
            .ensure_sqlite_vec0_schema_for_maintenance()
            .expect_err("partial vec0 schema must fail closed");

        assert!(semantic_terminal_maintenance_message(&error).is_some());
        assert!(!sqlite_table_exists(&store.conn, SQLITE_VEC0_TABLE)?);
        assert_eq!(
            store.conn.query_row(
                &format!("SELECT marker FROM {SQLITE_VEC0_META_TABLE} WHERE rowid = 7"),
                [],
                |row| row.get::<_, String>(0),
            )?,
            "preserve"
        );
        Ok(())
    }

    #[test]
    fn terminal_fingerprint_tracks_the_offending_non_head_row() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let first_event = Uuid::new_v4();
        let offending_event = Uuid::new_v4();
        let embedding = serialize_f32_blob(&test_embedding(1.0, 0.0));
        for (event_id, dimensions, seq) in [
            (first_event, SEMANTIC_DIMENSIONS as i64, 1_i64),
            (offending_event, SEMANTIC_DIMENSIONS as i64 + 1, 2_i64),
        ] {
            store.conn.execute(
                r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, event_seq, chunk_index, chunk_count,
                     source_text_sha256, chunk_text_sha256, start_char, end_char,
                     dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, ?3, 0, 1, ?4, ?4, 0, 1, ?5, ?6, 1)
                "#,
                params![
                    event_id.to_string(),
                    semantic_model_key(),
                    seq,
                    event_id.to_string(),
                    dimensions,
                    &embedding
                ],
            )?;
        }
        store.conn.execute(
            "UPDATE semantic_index_stats SET trust_version = 0 WHERE model_key = ?1",
            [semantic_model_key()],
        )?;

        let mut terminal_error = None;
        for _ in 0..16 {
            match store.run_maintenance_slice() {
                Ok(_) => {}
                Err(error) => {
                    terminal_error = Some(error);
                    break;
                }
            }
        }
        let error = terminal_error.expect("malformed second row must become terminal");
        let message = semantic_terminal_maintenance_message(&error).expect("typed terminal error");
        store.record_terminal_maintenance_failure(&message)?;
        assert_eq!(
            store.active_terminal_maintenance_failure()?,
            Some(message.clone())
        );

        store.conn.execute(
            r#"
            UPDATE event_embedding_chunks
            SET dimensions = ?2
            WHERE event_id = ?1 AND model_key = ?3
            "#,
            params![
                offending_event.to_string(),
                SEMANTIC_DIMENSIONS as i64,
                semantic_model_key()
            ],
        )?;
        assert_eq!(store.active_terminal_maintenance_failure()?, None);
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
        store.sync_sqlite_vec0_from_chunks_if_needed()?;

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
    fn sqlite_vec0_partition_excludes_changed_inactive_vectors_from_top_k() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "active-close"),
                test_embedding(0.9, 0.1),
            ),
            (
                test_chunk(far_event, 1, "active-far"),
                test_embedding(0.0, 1.0),
            ),
        ])?;
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        let active_slot = store
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .expect("active slot");
        let inactive_slot = 1 - active_slot;
        let tx = store.conn.transaction()?;
        for index in 0..8_i64 {
            let inactive_event = Uuid::new_v4().to_string();
            tx.execute(
                &format!(
                    r#"
                    INSERT INTO {SQLITE_VEC0_META_TABLE}
                        (slot, canonical_rowid, event_id, model_key, event_seq, chunk_index,
                         source_text_sha256, start_char, end_char)
                    VALUES (?1, ?2, ?3, ?4, ?5, 0, 'inactive', 0, 1)
                    "#
                ),
                params![
                    inactive_slot,
                    10_000 + index,
                    inactive_event,
                    semantic_model_key(),
                    10_000 + index
                ],
            )?;
            let projection_rowid = tx.last_insert_rowid();
            tx.execute(
                &format!(
                    "INSERT INTO {SQLITE_VEC0_TABLE}(rowid, embedding, embedding_coarse, slot, model_key) VALUES (?1, ?2, vec_quantize_binary(?2), ?3, ?4)"
                ),
                params![
                    projection_rowid,
                    serialize_f32_blob(&test_embedding(1.0, 0.0)),
                    inactive_slot,
                    semantic_model_key()
                ],
            )?;
        }
        tx.commit()?;

        let query = test_embedding(1.0, 0.0);
        let projected = store.search(&query, 2)?;
        let canonical = store.search_event_ids(&query, &[close_event, far_event], 2)?;
        assert_eq!(
            projected
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            canonical
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn sqlite_vec0_model_partition_excludes_old_model_vectors_and_changes_the_plan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "current-close"),
                test_embedding(0.9, 0.1),
            ),
            (
                test_chunk(far_event, 1, "current-far"),
                test_embedding(0.0, 1.0),
            ),
        ])?;
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        let active_slot = store
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .expect("active slot");
        let old_model_key = "old:model-transition";
        let tx = store.conn.transaction()?;
        for index in 0..8_i64 {
            tx.execute(
                &format!(
                    r#"
                    INSERT INTO {SQLITE_VEC0_META_TABLE}
                        (slot, canonical_rowid, event_id, model_key, event_seq, chunk_index,
                         source_text_sha256, start_char, end_char)
                    VALUES (?1, ?2, ?3, ?4, ?5, 0, 'old', 0, 1)
                    "#
                ),
                params![
                    active_slot,
                    20_000 + index,
                    Uuid::new_v4().to_string(),
                    old_model_key,
                    20_000 + index
                ],
            )?;
            let projection_rowid = tx.last_insert_rowid();
            tx.execute(
                &format!(
                    "INSERT INTO {SQLITE_VEC0_TABLE}(rowid, embedding, embedding_coarse, slot, model_key) VALUES (?1, ?2, vec_quantize_binary(?2), ?3, ?4)"
                ),
                params![
                    projection_rowid,
                    serialize_f32_blob(&test_embedding(1.0, 0.0)),
                    active_slot,
                    old_model_key
                ],
            )?;
        }
        tx.commit()?;

        let query = test_embedding(1.0, 0.0);
        let projected = store.search(&query, 2)?;
        let canonical = store.search_event_ids(&query, &[close_event, far_event], 2)?;
        assert_eq!(
            projected
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            canonical
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>()
        );

        let query_blob = serialize_f32_blob(&query);
        let plan_with_model = {
            let mut stmt = store.conn.prepare(&format!(
                r#"
                EXPLAIN QUERY PLAN
                SELECT rowid
                FROM {SQLITE_VEC0_TABLE}
                WHERE slot = ?1 AND model_key = ?2
                  AND embedding MATCH ?3 AND k = ?4
                ORDER BY distance
                "#
            ))?;
            let rows = stmt.query_map(
                params![active_slot, semantic_model_key(), &query_blob, 2_i64],
                |row| row.get::<_, String>(3),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?.join("\n")
        };
        let plan_without_model = {
            let mut stmt = store.conn.prepare(&format!(
                r#"
                EXPLAIN QUERY PLAN
                SELECT rowid
                FROM {SQLITE_VEC0_TABLE}
                WHERE slot = ?1 AND embedding MATCH ?2 AND k = ?3
                ORDER BY distance
                "#
            ))?;
            let rows = stmt.query_map(params![active_slot, &query_blob, 2_i64], |row| {
                row.get::<_, String>(3)
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?.join("\n")
        };
        assert!(
            plan_with_model.contains("VIRTUAL TABLE INDEX"),
            "{plan_with_model}"
        );
        assert_ne!(plan_with_model, plan_without_model);
        Ok(())
    }

    #[test]
    fn open_preserves_legacy_projection_tables() -> Result<()> {
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
        assert!(sqlite_table_exists(&store.conn, "event_embedding_vec0")?);
        assert!(sqlite_table_exists(
            &store.conn,
            "event_embedding_vec0_meta"
        )?);
        let close_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(close_event, 1, "close"),
            test_embedding(1.0, 0.0),
        )])?;
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);
        assert!(sqlite_table_exists(&store.conn, SQLITE_VEC0_TABLE)?);
        assert!(sqlite_table_exists(&store.conn, SQLITE_VEC0_META_TABLE)?);
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
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let canonical_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), semantic_model_key()],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
            &format!(
                "UPDATE {SQLITE_VEC0_META_TABLE} SET rowid = rowid + 1000 WHERE event_id = ?1 AND model_key = ?2"
            ),
            params![close_event.to_string(), semantic_model_key()],
        )?;

        assert!(store.sqlite_vec0_search_ready()?);
        for _ in 0..3 {
            store.run_maintenance_slice()?;
            if !store.sqlite_vec0_search_ready()? {
                break;
            }
        }
        assert!(!store.sqlite_vec0_search_ready()?);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let active_slot = store
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .expect("active slot");
        let repaired_canonical_rowid = store.conn.query_row(
            &format!(
                "SELECT canonical_rowid FROM {SQLITE_VEC0_META_TABLE} WHERE slot = ?1 AND event_id = ?2 AND model_key = ?3"
            ),
            params![active_slot, close_event.to_string(), semantic_model_key()],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(repaired_canonical_rowid, canonical_rowid);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_coarse_payload_drift_is_repaired_by_maintenance() -> Result<()> {
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
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let active_slot = store
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .expect("active slot");
        let close_rowid = store.conn.query_row(
            &format!(
                "SELECT rowid FROM {SQLITE_VEC0_META_TABLE} WHERE slot = ?1 AND event_id = ?2 AND model_key = ?3"
            ),
            params![active_slot, close_event.to_string(), semantic_model_key()],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
            &format!("DELETE FROM {SQLITE_VEC0_TABLE} WHERE rowid = ?1"),
            params![close_rowid],
        )?;
        store.conn.execute(
            &format!(
                "INSERT INTO {SQLITE_VEC0_TABLE}(rowid, embedding, embedding_coarse, slot, model_key) VALUES (?1, ?2, vec_quantize_binary(?3), ?4, ?5)"
            ),
            params![
                close_rowid,
                serialize_f32_blob(&test_embedding(1.0, 0.0)),
                serialize_f32_blob(&test_embedding(0.0, 1.0)),
                active_slot,
                semantic_model_key()
            ],
        )?;

        assert!(
            store.sqlite_vec0_search_ready()?,
            "search hot path should trust the durable readiness marker"
        );
        for _ in 0..3 {
            store.run_maintenance_slice()?;
            if !store.sqlite_vec0_search_ready()? {
                break;
            }
        }
        assert!(!store.sqlite_vec0_search_ready()?);
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

        let daemon = daemon_report(
            temp.path(),
            &semantic_worker_report_best_effort(temp.path()),
        );

        assert_eq!(daemon["status"], "stale_lock");
        assert_eq!(daemon["running"], false);
        assert_eq!(daemon["recoverable"], true);
        assert_eq!(daemon["reason"], "daemon_status_stale");
        Ok(())
    }
}

#[cfg(test)]
mod model_retry_tests {
    use super::*;

    fn retry_policy() -> model_retry::SemanticModelRetryPolicy {
        model_retry::SemanticModelRetryPolicy {
            initial_backoff: StdDuration::from_millis(10),
            max_backoff: StdDuration::from_millis(80),
        }
    }

    #[test]
    fn retryable_model_failures_back_off_indefinitely_and_persist_status() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let retry_path = temp.path().join("model-retry.json");
        let store = model_retry::SemanticModelRetryStore::new(&retry_path, retry_policy());
        let mut now_ms = 1_000_i64;
        let mut final_next_retry_at_ms = None;

        for expected_attempt in 1..=8 {
            let (_, eligibility) = store.record_failure(
                semantic_model_key(),
                now_ms,
                model_retry::SemanticModelFailure::retryable(
                    model_retry::SemanticModelFailureClass::Acquisition,
                    "transient acquisition failure",
                ),
            )?;
            let model_retry::SemanticModelRetryEligibility::Deferred {
                attempt,
                next_eligible_at_ms,
                ..
            } = eligibility
            else {
                panic!("retryable failure must remain deferred");
            };
            assert_eq!(attempt, expected_attempt);
            assert!(next_eligible_at_ms > now_ms);
            assert_eq!(
                next_eligible_at_ms - now_ms,
                10_i64 << expected_attempt.saturating_sub(1).min(3)
            );
            final_next_retry_at_ms = Some(next_eligible_at_ms);
            if expected_attempt < 8 {
                now_ms = next_eligible_at_ms;
            }
        }

        let status = model_retry::SemanticModelRetryStore::new(&retry_path, retry_policy())
            .status(semantic_model_key(), now_ms)?;
        assert_eq!(status.attempt, 8);
        assert_eq!(status.next_retry_at_ms, final_next_retry_at_ms);
        assert!(status.retryable);
        assert!(!status.terminal);
        let readiness =
            readiness::SemanticReadinessDiagnostics::evaluate(readiness::SemanticReadinessInputs {
                enabled: true,
                supported: true,
                model_available: false,
                sidecar_available: true,
                vector_backend_available: true,
                coverage: readiness::SemanticCoverageDiagnostics {
                    indexed_items: 1,
                    indexed_chunks: 1,
                    searchable_items: Some(1),
                    dirty_items: 0,
                    queued_items: 0,
                },
                model_retry: status.clone(),
            });
        assert_eq!(
            readiness.state,
            readiness::SemanticReadinessState::RetryDeferred
        );
        assert!(matches!(
            readiness.primary_blocker(),
            Some(readiness::SemanticReadinessBlocker::ModelRetryDeferred { attempt: 8, .. })
        ));
        let status_json = serde_json::to_value(&status)?;
        assert!(status_json.get("max_attempts").is_none());
        assert!(status_json.get("exhausted").is_none());
        Ok(())
    }

    #[test]
    fn terminal_integrity_failure_does_not_retry() -> Result<()> {
        let mut state = model_retry::SemanticModelRetryState::new(semantic_model_key());
        let eligibility = state.record_failure(
            semantic_model_key(),
            1_000,
            model_retry::SemanticModelFailure::terminal(
                model_retry::SemanticModelFailureClass::Integrity,
                "digest mismatch",
            ),
            retry_policy(),
        )?;
        assert!(matches!(
            eligibility,
            model_retry::SemanticModelRetryEligibility::Terminal { attempt: 1, .. }
        ));
        let status = state.status(1_000, retry_policy())?;
        assert_eq!(status.attempt, 1);
        assert!(!status.retryable);
        assert!(status.terminal);
        assert_eq!(status.next_retry_at_ms, None);
        let readiness =
            readiness::SemanticReadinessDiagnostics::evaluate(readiness::SemanticReadinessInputs {
                enabled: true,
                supported: true,
                model_available: false,
                sidecar_available: true,
                vector_backend_available: true,
                coverage: readiness::SemanticCoverageDiagnostics {
                    indexed_items: 1,
                    indexed_chunks: 1,
                    searchable_items: Some(1),
                    dirty_items: 0,
                    queued_items: 0,
                },
                model_retry: status,
            });
        assert_eq!(readiness.state, readiness::SemanticReadinessState::Failed);
        assert!(matches!(
            readiness.primary_blocker(),
            Some(readiness::SemanticReadinessBlocker::ModelFailureTerminal { attempt: 1, .. })
        ));
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
        insert_test_event(
            &store,
            "semantic unsupported platform lexical fallback fixture",
        )?;
        let lexical_query = ctx_protocol::SearchQuery::new(vec![ctx_protocol::SearchClause::all(
            "semantic unsupported platform lexical fallback fixture",
        )])
        .canonicalized()?;

        let (packet, retrieval) = search_packet_query_with_backend(
            &store,
            temp.path(),
            &lexical_query,
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            true,
            RefreshArg::Off,
            false,
        )?;

        assert_eq!(retrieval.effective_mode(), SearchBackendArg::Lexical);
        assert_eq!(retrieval.to_json()["semantic_status"], "unsupported");
        assert!(packet.query_execution.semantic.attempted);
        assert_eq!(
            packet.query_execution.semantic.readiness,
            ctx_protocol::SearchSemanticReadiness::Unsupported
        );
        assert_eq!(
            packet.query_execution.semantic.effective_backend,
            ctx_protocol::SearchEffectiveBackend::Lexical
        );
        assert_eq!(
            packet.query_execution.semantic.skip_reason,
            Some(ctx_protocol::SearchSemanticSkipReason::Unsupported)
        );
        assert_eq!(
            packet.query,
            "semantic unsupported platform lexical fallback fixture"
        );

        let semantic_query =
            ctx_protocol::SearchQuery::new(vec![ctx_protocol::SearchClause::semantic(
                "semantic unsupported platform lexical fallback fixture",
            )])
            .canonicalized()?;
        let error = search_packet_query_with_backend(
            &store,
            temp.path(),
            &semantic_query,
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            true,
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
            query_priority::SemanticQueryPriorityGate::default(),
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

    #[test]
    fn typed_query_authentication_fails_before_foreground_priority() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let embedder = Arc::new(Mutex::new(None));
        let priority = query_priority::SemanticQueryPriorityGate::default();
        let request = query_service_contract::SemanticQueryServiceRequest::new(
            semantic_model_key(),
            readiness::SemanticRetrievalRequestMode::ExplicitSemantic,
            10_000,
            vec![query_service_contract::SemanticQueryClauseRequest::new(
                0,
                "authentication boundary",
                10,
            )],
        );
        let mut response_bytes = Vec::new();

        handle_daemon_query_stream_inner(
            temp.path(),
            &embedder,
            &priority,
            "0123456789abcdef0123456789abcdef",
            &mut response_bytes,
            &serde_json::to_string(&request)?,
        )?;

        let response: query_service_contract::SemanticQueryServiceResponse =
            serde_json::from_slice(&response_bytes)?;
        assert_eq!(
            response.error.map(|error| error.code),
            Some(query_service_contract::SemanticQueryFailureCode::AuthenticationFailed)
        );
        let snapshot = priority.snapshot();
        assert_eq!(snapshot.waiting_foreground_queries, 0);
        assert_eq!(snapshot.active_foreground_queries, 0);
        assert!(!snapshot.document_batch_active);
        Ok(())
    }

    #[test]
    fn typed_query_deadline_is_required_and_bounded() -> Result<()> {
        let clause = || {
            vec![query_service_contract::SemanticQueryClauseRequest::new(
                0,
                "shared deadline",
                10,
            )]
        };
        let zero = query_service_contract::SemanticQueryServiceRequest::new(
            semantic_model_key(),
            readiness::SemanticRetrievalRequestMode::ExplicitSemantic,
            0,
            clause(),
        );
        assert!(zero.validate_for_model(semantic_model_key()).is_err());

        let excessive = query_service_contract::SemanticQueryServiceRequest::new(
            semantic_model_key(),
            readiness::SemanticRetrievalRequestMode::ExplicitSemantic,
            query_service_contract::SEMANTIC_QUERY_MAX_EXECUTION_TIMEOUT_MS + 1,
            clause(),
        );
        assert!(excessive.validate_for_model(semantic_model_key()).is_err());

        let token = "0123456789abcdef0123456789abcdef";
        let mut bounded = query_service_contract::SemanticQueryServiceRequest::new(
            semantic_model_key(),
            readiness::SemanticRetrievalRequestMode::ExplicitSemantic,
            137,
            clause(),
        );
        bounded.token = token.to_owned();
        let authenticated = bounded.authenticate_and_validate(token, semantic_model_key())?;
        assert_eq!(authenticated.execution_timeout_ms(), 137);
        Ok(())
    }

    #[test]
    fn foreground_waiter_wins_before_next_background_batch_without_sleep() -> Result<()> {
        let priority = query_priority::SemanticQueryPriorityGate::default();
        let active_batch = priority.begin_document_batch(None)?;
        let foreground_priority = priority.clone();
        let (foreground_acquired_tx, foreground_acquired_rx) = std::sync::mpsc::channel();
        let (release_foreground_tx, release_foreground_rx) = std::sync::mpsc::channel();
        let foreground = std::thread::spawn(move || {
            let permit = foreground_priority
                .begin_test_foreground_query(None)
                .expect("foreground permit");
            foreground_acquired_tx
                .send(())
                .expect("signal foreground permit");
            release_foreground_rx
                .recv()
                .expect("release foreground permit");
            drop(permit);
        });
        while priority.snapshot().waiting_foreground_queries == 0 {
            std::thread::yield_now();
        }

        let background_priority = priority.clone();
        let (background_acquired_tx, background_acquired_rx) = std::sync::mpsc::channel();
        let background = std::thread::spawn(move || {
            let permit = background_priority
                .begin_document_batch(None)
                .expect("background permit");
            background_acquired_tx
                .send(())
                .expect("signal background permit");
            drop(permit);
        });
        drop(active_batch);

        foreground_acquired_rx.recv()?;
        assert!(matches!(
            background_acquired_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        release_foreground_tx.send(())?;
        background_acquired_rx.recv()?;
        foreground.join().expect("foreground thread");
        background.join().expect("background thread");
        Ok(())
    }

    #[test]
    fn semantic_query_retryability_is_explicit_and_fail_closed() {
        let pending = anyhow::Error::new(SemanticVectorStorePending::new("pending"));
        let terminal = anyhow::Error::new(SemanticVectorStoreTerminal::new("terminal"));
        let busy = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ));
        let interrupted = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
            None,
        ));
        let corrupt = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None,
        ));

        assert!(semantic_query_error_retryable(&pending));
        assert!(semantic_query_error_retryable(&busy));
        assert!(semantic_query_error_retryable(&interrupted));
        assert!(!semantic_query_error_retryable(&terminal));
        assert!(!semantic_query_error_retryable(&corrupt));
        assert!(!semantic_query_error_retryable(&anyhow!("unknown")));
        assert!(!semantic_deterministic_sidecar_error(&busy));
        assert!(!semantic_deterministic_sidecar_error(&interrupted));
        assert!(semantic_deterministic_sidecar_error(&corrupt));
    }

    #[test]
    fn typed_query_never_initializes_a_missing_model() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let _store = Store::open(database_path(temp.path().to_path_buf()))?;
        let _vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
        let embedder = Arc::new(Mutex::new(None));
        let priority = query_priority::SemanticQueryPriorityGate::default();
        let token = "0123456789abcdef0123456789abcdef";
        let mut request = query_service_contract::SemanticQueryServiceRequest::new(
            semantic_model_key(),
            readiness::SemanticRetrievalRequestMode::ExplicitSemantic,
            10_000,
            vec![query_service_contract::SemanticQueryClauseRequest::new(
                0,
                "resident model only",
                10,
            )],
        );
        request.token = token.to_owned();
        let mut response_bytes = Vec::new();

        handle_daemon_query_stream_inner(
            temp.path(),
            &embedder,
            &priority,
            token,
            &mut response_bytes,
            &serde_json::to_string(&request)?,
        )?;

        let response: query_service_contract::SemanticQueryServiceResponse =
            serde_json::from_slice(&response_bytes)?;
        assert!(!response.ok);
        assert_eq!(
            response.error.map(|error| error.code),
            Some(query_service_contract::SemanticQueryFailureCode::NotReady)
        );
        assert!(embedder.lock().expect("embedder lock").is_none());
        let snapshot = priority.snapshot();
        assert_eq!(snapshot.active_foreground_queries, 0);
        assert!(!snapshot.document_batch_active);
        Ok(())
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
            query_priority::SemanticQueryPriorityGate::default(),
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

        let error =
            read_daemon_query_request(&mut cursor, 3).expect_err("oversized request should fail");

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
        let error = match acquire_cpu_backend(temp.path(), policy, BackendPreference::Cpu, false) {
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

        assert_eq!(
            semantic_worker_cache_dir_from_env(&data_root, &env),
            explicit
        );
        Ok(())
    }
}

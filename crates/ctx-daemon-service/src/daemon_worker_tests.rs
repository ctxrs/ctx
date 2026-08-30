use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, EventRole, EventType, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{CoreEventPageBudget, GenerationWriter, VerifiedIndex, WriterOptions};
use ctx_semantic_index::{
    source_backed_semantic_vector_path, SemanticDocumentBuilder, SemanticVectorStore,
    SourceBackedSemanticDocumentBuilder,
};
use ctx_semantic_model::{
    semantic_model_contract, ExternalSemanticSpace, PreparedSemanticDocuments,
    PreparedSemanticQuery, SemanticEmbeddingExecutorConfig, SemanticEmbeddingExecutorHandle,
    SemanticModelContract, SharedSemanticRuntime,
};

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "gnu"
    ),
    all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "freebsd", target_arch = "x86_64")
))]
use ctx_semantic_model::{
    semantic_model_cache_available,
    test_support::{
        load_missing_semantic_onnxruntime as load_missing_semantic_onnxruntime_for_test,
        map_daemon_coreml_load_error, write_test_semantic_cache,
    },
};

use super::*;
use crate::{
    daemon_retry::{DaemonRetryBackoff, SemanticFailureClass},
    daemon_scheduler::record_daemon_job_retry,
    CONFIG_FILE,
};

struct RecordingSemanticExecutor {
    contract: SemanticModelContract,
    documents: std::sync::Mutex<Vec<(Vec<String>, Option<Instant>)>>,
}

impl SemanticEmbeddingExecutor for RecordingSemanticExecutor {
    fn contract(&self) -> &SemanticModelContract {
        &self.contract
    }

    fn embed_query(&self, _query: PreparedSemanticQuery) -> Result<Vec<f32>> {
        unreachable!("document execution test must not invoke query inference")
    }

    fn embed_documents(
        &self,
        documents: PreparedSemanticDocuments,
        deadline: Option<Instant>,
    ) -> Result<Vec<Vec<f32>>> {
        let documents = documents.into_texts();
        self.documents
            .lock()
            .expect("record semantic documents")
            .push((documents.clone(), deadline));
        Ok(documents
            .iter()
            .map(|_| {
                let mut embedding = vec![0.0; self.contract.dimensions()];
                embedding[0] = 1.0;
                embedding
            })
            .collect())
    }
}

#[test]
fn daemon_document_execution_uses_the_pluggable_prepared_input_seam() -> Result<()> {
    let executor = RecordingSemanticExecutor {
        contract: semantic_model_contract().clone(),
        documents: std::sync::Mutex::new(Vec::new()),
    };
    let deadline = Instant::now() + std::time::Duration::from_secs(1);

    let embeddings = execute_document_embeddings(
        &executor,
        vec!["raw document".to_owned(), "passage: prepared".to_owned()],
        Some(deadline),
    )?;

    assert_eq!(embeddings.len(), 2);
    assert_eq!(
        *executor.documents.lock().expect("recorded documents"),
        [(
            vec![
                "passage: raw document".to_owned(),
                "passage: prepared".to_owned(),
            ],
            Some(deadline),
        )]
    );
    Ok(())
}

#[test]
fn external_document_execution_uses_each_selected_space_without_builtin_prefixes() -> Result<()> {
    for (space_id, dimensions) in [("space-96", 96), ("space-768", 768)] {
        let config = SemanticEmbeddingExecutorConfig::http(
            "http://127.0.0.1:41020",
            ExternalSemanticSpace::new(space_id, dimensions)?,
        )?;
        let executor = RecordingSemanticExecutor {
            contract: config.contract().clone(),
            documents: std::sync::Mutex::new(Vec::new()),
        };

        let embeddings =
            execute_document_embeddings(&executor, vec!["raw document".to_owned()], None)?;

        assert_eq!(embeddings.len(), 1);
        assert_eq!(embeddings[0].len(), dimensions);
        assert_eq!(
            *executor.documents.lock().expect("recorded documents"),
            [(vec!["raw document".to_owned()], None)]
        );
    }
    Ok(())
}

fn contract_response_endpoint(body: &str) -> Result<(String, thread::JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let body = body.to_owned();
    let server = thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8(request)?;
        assert!(
            request.starts_with("GET /v2/contract HTTP/1.1\r\n"),
            "unexpected semantic verification request: {request}"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes())?;
        Ok(())
    });
    Ok((endpoint, server))
}

fn assert_contract_verification_failure_preserves_store(
    response_body: &str,
    expected_error: &str,
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (endpoint, server) = contract_response_endpoint(response_body)?;
    let old_config = SemanticEmbeddingExecutorConfig::http(
        &endpoint,
        ExternalSemanticSpace::new("old-space", 64)?,
    )?;
    let selected_config = SemanticEmbeddingExecutorConfig::http(
        &endpoint,
        ExternalSemanticSpace::new("selected-space", 128)?,
    )?;
    let vector_path = source_backed_semantic_vector_path(temp.path());
    let old_index_contract = semantic_index_contract(old_config.contract())?;
    drop(SemanticVectorStore::open(
        &vector_path,
        &old_index_contract,
    )?);
    let executor = Arc::new(SemanticEmbeddingExecutorHandle::build(
        selected_config,
        SharedSemanticRuntime::default(),
        crate::test_support::CONFIG.semantic_model_config(temp.path()),
    )?);

    let error = match open_selected_semantic_vector_store(&vector_path, &executor) {
        Ok(_) => panic!("endpoint verification must fail before writable store open"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains(expected_error), "{error:#}");
    assert_eq!(
        classify_semantic_failure(&error),
        SemanticFailureClass::Permanent
    );
    let job = daemon_semantic_failed_job(temp.path(), error);
    assert_eq!(job["failure_class"], "permanent");
    assert_eq!(job["retryable"], false);
    assert!(job["last_error"]
        .as_str()
        .is_some_and(|error| error.contains(expected_error)));
    assert!(
        SemanticVectorStore::open_read_only(&vector_path, &old_index_contract)?.is_some(),
        "verification failure must preserve the previous contract's store"
    );
    server.join().expect("contract response server panicked")?;
    Ok(())
}

#[test]
fn remote_space_drift_fails_permanently_before_store_reset() -> Result<()> {
    assert_contract_verification_failure_preserves_store(
        r#"{"schema_version":2,"space_id":"drifted-space","dimensions":128}"#,
        "asserted a different semantic space",
    )
}

#[test]
fn malformed_contract_output_fails_permanently_before_store_reset() -> Result<()> {
    assert_contract_verification_failure_preserves_store(
        r#"{"schema_version":2,"space_id":17,"dimensions":"bad"}"#,
        "contract response is malformed",
    )
}

#[test]
fn daemon_job_json_keeps_outcomes_without_live_worker_snapshots() {
    let job = daemon_semantic_job_json("budget_exhausted", None, 1234, Some(7), None);

    assert_eq!(job["status"], "budget_exhausted");
    assert_eq!(job["indexed_chunks"], 7);
    for field in [
        "enabled",
        "model_cache_available",
        "model_acquisition",
        "embed_policy",
        "embedding_runtime",
        "worker_status",
        "coverage",
    ] {
        assert!(
            job.get(field).is_none(),
            "unexpected live snapshot: {field}"
        );
    }
}

#[test]
fn daemon_acquisition_failure_is_explicit_retryable_and_fail_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let startup = run_daemon_semantic_model_startup_with(
        1234,
        || Err(anyhow!("signed model input unavailable")),
        |_| -> Result<SemanticDaemonModelAcquisition> {
            unreachable!("failed initial acquisition must not request CPU fallback")
        },
        |_| -> Result<()> { unreachable!("failed acquisition must never initialize the runtime") },
    )?;
    let DaemonSemanticModelStartup::Finished(job) = startup else {
        panic!("failed acquisition must stop daemon model startup");
    };
    assert_eq!(job["status"], "skipped");
    assert_eq!(job["reason"], "model_acquisition_failed");

    let mut backoff = DaemonRetryBackoff::default();
    let job = record_daemon_job_retry(&mut backoff, job);
    assert_eq!(job["failure_class"], "retryable");
    assert_eq!(job["retryable"], true);
    assert!(job["retry_after_ms"]
        .as_u64()
        .is_some_and(|delay| delay > 0));
    assert!(
        !source_backed_semantic_vector_path(temp.path()).exists(),
        "failed model acquisition must not claim a semantic projection"
    );
    Ok(())
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "gnu"
    ),
    all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "freebsd", target_arch = "x86_64")
))]
#[test]
fn verified_cache_missing_runtime_reports_model_load_failed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache_dir = temp.path().join("semantic-model-cache");
    write_test_semantic_cache(&cache_dir)?;
    let missing_runtime = temp.path().join("missing-libonnxruntime.so");

    let startup = run_daemon_semantic_model_startup_with(
        1234,
        || Ok(SemanticDaemonModelAcquisition::verified_cpu_cache_for_test()),
        |_| -> Result<SemanticDaemonModelAcquisition> {
            unreachable!("CPU runtime load failure must not request Core ML fallback")
        },
        |_| -> Result<()> {
            load_missing_semantic_onnxruntime_for_test(&cache_dir, &missing_runtime)?;
            unreachable!("missing explicit runtime must fail deterministically")
        },
    )?;
    let DaemonSemanticModelStartup::Finished(job) = startup else {
        panic!("missing ONNX Runtime must stop daemon model startup");
    };
    assert_eq!(job["status"], "skipped");
    assert_eq!(job["reason"], "model_load_failed");
    assert_eq!(job["failure_class"], "retryable");
    assert!(job["last_error"]
        .as_str()
        .is_some_and(|message| message.contains("failed to load ONNX Runtime")));
    assert!(semantic_model_cache_available(&cache_dir));
    Ok(())
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "gnu"
    ),
    all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "freebsd", target_arch = "x86_64")
))]
#[test]
fn auto_coreml_load_failure_acquires_cpu_and_preserves_fallback_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )?;
    let cpu_cache = temp.path().join("semantic-model-cache");
    assert!(!cpu_cache.exists(), "CPU fallback cache must start empty");
    let cpu_acquired = std::cell::Cell::new(false);
    let load_attempts = std::cell::Cell::new(0_u8);

    let startup = run_daemon_semantic_model_startup_with(
        1234,
        || Ok(SemanticDaemonModelAcquisition::verified_coreml_cache_for_test()),
        |fallback| {
            assert_eq!(fallback, "coreml_load_error");
            assert!(
                !cpu_cache.exists(),
                "forced Core ML load failure must precede CPU acquisition"
            );
            std::fs::create_dir_all(cpu_cache.join("daemon-authorized-cpu-acquisition"))?;
            cpu_acquired.set(true);
            Ok(SemanticDaemonModelAcquisition::downloaded_cpu_fallback_for_test(fallback))
        },
        |acquisition| {
            load_attempts.set(load_attempts.get() + 1);
            if acquisition.fallback().is_none() {
                return Err(map_daemon_coreml_load_error(
                    acquisition,
                    anyhow!("forced Core ML runtime load failure"),
                ));
            }
            assert!(
                cpu_acquired.get(),
                "cache-only CPU load must follow daemon-authorized acquisition"
            );
            assert_eq!(acquisition.source(), "download");
            assert_eq!(acquisition.fallback(), Some("coreml_load_error"));
            Ok(())
        },
    )?;

    assert!(matches!(startup, DaemonSemanticModelStartup::Loaded));
    assert!(cpu_acquired.get());
    assert_eq!(load_attempts.get(), 2);
    Ok(())
}

struct CoreFixture {
    temp: tempfile::TempDir,
    source: SourceKey,
    session_id: ctx_history_core::StableEntityId,
}

impl CoreFixture {
    fn new() -> Self {
        let source = SourceKey::derive(
            "gemini",
            "gemini_cli_chat_recording_jsonl",
            "session",
            1,
            SourceAnchor::CatalogLineage([41; 32]),
        )
        .unwrap();
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("gemini-session").unwrap())
                .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        Self {
            temp: tempfile::tempdir().unwrap(),
            source,
            session_id,
        }
    }

    fn record(&self, sequence: u64, role: EventRole, body: impl Into<String>) -> CoreRecord {
        self.record_in_session("gemini-session", sequence, role, body)
    }

    fn tool_record(&self, sequence: u64) -> CoreRecord {
        let mut record = self.record(sequence, EventRole::Tool, "tool payload");
        record.event_type = EventType::ToolOutput.as_str().to_owned();
        record.validate_contract().unwrap();
        record
    }

    fn record_in_session(
        &self,
        native_session_id: &str,
        sequence: u64,
        role: EventRole,
        body: impl Into<String>,
    ) -> CoreRecord {
        let native_session_key = TypedKey::utf8(native_session_id).unwrap();
        let session_key = NativeSessionKey::native_id("session", native_session_key).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &self.source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let item = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap();
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            self.source.clone(),
            sequence,
            EventType::Message.as_str(),
            "semantic-daemon-test-v1",
            body,
        )
        .unwrap();
        record.provider_session_id = Some(native_session_id.to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.occurred_at_unix_ms = Some(sequence as i64);
        record.role = Some(role.as_str().to_owned());
        record.agent_scope = Some(AgentScope::Primary);
        record.validate_contract().unwrap();
        record
    }

    fn index(&self, records: Vec<CoreRecord>) -> VerifiedIndex {
        let count = records.len() as u64;
        let mut writer =
            GenerationWriter::open(self.temp.path().join("index"), WriterOptions::default())
                .unwrap()
                .into_writer()
                .unwrap();
        writer.begin_source(self.source.clone()).unwrap();
        for record in records {
            writer.add_core_record(record).unwrap();
        }
        let observation =
            SourceObservation::new(self.source.clone(), "fixture-v1", vec![1]).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "fixture-parser-v1",
                    [1; 32],
                    ScannedSourceCounts {
                        complete_records: count,
                        retained_records: count,
                        indexed_documents: count,
                        certified_bytes: count * 80,
                        ..ScannedSourceCounts::default()
                    },
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
        VerifiedIndex::open_pinned(self.temp.path().join("index")).unwrap()
    }
}

#[test]
fn core_builder_combines_complete_lite_turn_with_provider_source_absent() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "exact Gemini question"),
        fixture.record(2, EventRole::Assistant, "early answer"),
        fixture.record(3, EventRole::Assistant, "final exact Gemini answer"),
        fixture.record(4, EventRole::User, "next question"),
    ]);
    assert!(!fixture
        .temp
        .path()
        .join("provider-source-was-removed.jsonl")
        .exists());
    let anchor = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|record| record.event_sequence == 1)
        .unwrap();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(document.event_id(), anchor.event_id.as_uuid());
    assert_eq!(document.provider(), Some(CaptureProvider::Gemini));
    assert_eq!(document.occurred_at_ms(), 3);
    assert_eq!(
        document.text(),
        "user:\nexact Gemini question\n\nassistant:\nfinal exact Gemini answer"
    );
}

#[test]
fn core_builder_preserves_semantic_tail_beyond_sixteen_kib() {
    const TAIL: &str = "semantic-tail-token-7f0d";
    let fixture = CoreFixture::new();
    let body = format!("{} {TAIL}", "prefix ".repeat(2_500));
    assert!(body.len() > 16 * 1024);
    let index = fixture.index(vec![fixture.record(1, EventRole::User, body.clone())]);
    let page = index.core_semantic_event_page(None, 1).unwrap();
    let record = page.items.first().unwrap();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);

    let document = builder.build_document(record).unwrap().unwrap();

    assert!(record.core_record.content.meaningful_text().ends_with(TAIL));
    assert!(document.text().ends_with(TAIL));
    assert!(document.text().len() > 16 * 1024);
}

#[test]
fn core_builder_pairs_multiple_lite_turns_with_bounded_forward_queries() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "first question"),
        fixture.record(2, EventRole::Assistant, "first answer"),
        fixture.record(3, EventRole::User, "second question"),
        fixture.record(4, EventRole::Assistant, "second answer"),
    ]);
    let anchors = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .filter(|record| record.role.as_deref() == Some(EventRole::User.as_str()))
        .collect::<Vec<_>>();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);

    let first = builder.build_document(&anchors[0]).unwrap().unwrap();
    let second = builder.build_document(&anchors[1]).unwrap().unwrap();

    assert_eq!(
        first.text(),
        "user:\nfirst question\n\nassistant:\nfirst answer"
    );
    assert_eq!(
        second.text(),
        "user:\nsecond question\n\nassistant:\nsecond answer"
    );
}

#[test]
fn core_builder_streams_multiple_pairing_pages_to_the_final_assistant() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "bounded question"),
        fixture.record(2, EventRole::Assistant, "early bounded answer"),
        fixture.record(3, EventRole::Assistant, "late bounded answer"),
    ]);
    let anchor = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|record| record.event_sequence == 1)
        .unwrap();
    let mut builder = SourceBackedSemanticDocumentBuilder::with_pairing_limits_for_test(
        &index,
        1,
        CoreEventPageBudget::new(64 * 1024 * 1024, 16 * 1024 * 1024),
    );

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(
        document.text(),
        "user:\nbounded question\n\nassistant:\nlate bounded answer"
    );
    assert_eq!(document.occurred_at_ms(), 3);
}

#[test]
fn core_builder_pairs_many_sessions_without_retaining_a_session_cache() {
    let fixture = CoreFixture::new();
    let mut records = Vec::new();
    for session in 0..12_u64 {
        let native_session_id = format!("gemini-session-{session}");
        records.push(fixture.record_in_session(
            &native_session_id,
            session * 2 + 1,
            EventRole::User,
            format!("question {session}"),
        ));
        records.push(fixture.record_in_session(
            &native_session_id,
            session * 2 + 2,
            EventRole::Assistant,
            format!("answer {session}"),
        ));
    }
    let index = fixture.index(records);
    let anchors = index.core_semantic_event_page(None, 64).unwrap().items;
    assert_eq!(anchors.len(), 12);
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);

    for anchor in &anchors {
        let session = (anchor.event_sequence - 1) / 2;
        let document = builder.build_document(anchor).unwrap().unwrap();
        assert_eq!(
            document.text(),
            format!("user:\nquestion {session}\n\nassistant:\nanswer {session}")
        );
    }
}

#[test]
fn core_builder_returns_user_only_when_pairing_byte_budget_is_exhausted() {
    let fixture = CoreFixture::new();
    let index = fixture.index(vec![
        fixture.record(1, EventRole::User, "byte bounded question"),
        fixture.record(2, EventRole::Assistant, "first answer"),
        fixture.record(3, EventRole::Assistant, "second answer"),
    ]);
    let anchor = index
        .core_semantic_event_page(None, 1)
        .unwrap()
        .items
        .remove(0);
    let mut builder = SourceBackedSemanticDocumentBuilder::with_pairing_limits_for_test(
        &index,
        64,
        CoreEventPageBudget::new(ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES, 1),
    );

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(document.text(), "user:\nbyte bounded question");
    assert_eq!(document.occurred_at_ms(), 1);
}

#[test]
fn core_builder_preserves_assistant_after_more_than_sixty_four_tool_events() {
    const TOOL_EVENTS: u64 = 96;

    let fixture = CoreFixture::new();
    let mut records = Vec::with_capacity(TOOL_EVENTS as usize + 3);
    records.push(fixture.record(1, EventRole::User, "tool-heavy question"));
    for sequence in 2..=TOOL_EVENTS + 1 {
        records.push(fixture.tool_record(sequence));
    }
    records.push(fixture.record(
        TOOL_EVENTS + 2,
        EventRole::Assistant,
        "answer beyond the old window",
    ));
    records.push(fixture.record(TOOL_EVENTS + 3, EventRole::User, "next question"));
    let index = fixture.index(records);
    let anchor = index
        .core_events_for_session(fixture.session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|record| record.event_sequence == 1)
        .unwrap();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);

    let document = builder.build_document(&anchor).unwrap().unwrap();

    assert_eq!(
        document.text(),
        "user:\ntool-heavy question\n\nassistant:\nanswer beyond the old window"
    );
    assert_eq!(document.occurred_at_ms(), (TOOL_EVENTS + 2) as i64);
}

#[test]
fn daemon_lifecycle_receipt_preserves_service_trigger_metadata() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let args = DaemonRunArgs {
        loop_interval_seconds: None,
        max_chunks: None,
        handle_process_signals: false,
        force: false,
        profile: crate::DaemonRunProfile::Persistent,
        start_mode: Some(crate::DaemonStartMode::Auto),
        trigger_command: Some(crate::DaemonTrigger::Setup),
        supervisor: crate::DaemonSupervisor::User,
    };

    write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;
    let status = crate::paths_status::read_daemon_status(temp.path()).expect("daemon status");
    assert_eq!(status["start_mode"], "auto");
    assert_eq!(status["trigger_command"], "setup");
    assert_eq!(status["started_at_ms"], 123);
    Ok(())
}

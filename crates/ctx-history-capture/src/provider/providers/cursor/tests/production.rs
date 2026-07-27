use super::*;

#[test]
fn missing_source_candidates_require_a_completed_exact_root_inventory() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    write_transcript(&root, "project", "live-session", &jsonl([user("live")]));
    let inventory = discover_cursor_transcripts(&root);
    let frozen = freeze_cursor_source(&inventory.transcripts[0]).unwrap();
    let known = [
        CursorKnownSource {
            canonical_source_key: "live-key".to_owned(),
            locator_identity: frozen.observation().locator_identity.clone(),
            native_session_id: "live-session".to_owned(),
        },
        CursorKnownSource {
            canonical_source_key: "missing-key".to_owned(),
            locator_identity: "provider-path-v1:missing".to_owned(),
            native_session_id: "missing-session".to_owned(),
        },
    ];

    let exact =
        CursorCompletedExactInventory::from_discovery(&inventory, &[frozen.observation().clone()])
            .unwrap();
    assert_eq!(
        resolve_cursor_missing_sources(&known, Some(&exact)),
        [
            CursorMissingSourceDisposition::Present {
                canonical_source_key: "live-key".to_owned(),
            },
            CursorMissingSourceDisposition::RouteUnavailableCandidate {
                canonical_source_key: "missing-key".to_owned(),
                locator_identity: "provider-path-v1:missing".to_owned(),
            },
        ]
    );

    let mut incomplete = inventory;
    incomplete.completed = false;
    assert!(CursorCompletedExactInventory::from_discovery(
        &incomplete,
        &[frozen.observation().clone()],
    )
    .is_none());
    assert_eq!(
        resolve_cursor_missing_sources(&known, None),
        [
            CursorMissingSourceDisposition::RetainWithoutCompletedInventory {
                canonical_source_key: "live-key".to_owned(),
            },
            CursorMissingSourceDisposition::RetainWithoutCompletedInventory {
                canonical_source_key: "missing-key".to_owned(),
            },
        ]
    );

    let arbitrary_empty_root = temp.path().join("not-a-cursor-projects-root");
    fs::create_dir(&arbitrary_empty_root).unwrap();
    let arbitrary_inventory = discover_cursor_transcripts(&arbitrary_empty_root);
    assert!(arbitrary_inventory.completed);
    assert!(
        CursorCompletedExactInventory::from_discovery(&arbitrary_inventory, &[]).is_none(),
        "a completed but inexact empty directory must not authorize deletion"
    );
}

#[test]
fn unknown_future_rows_fail_closed_without_body_or_hash_construction() {
    let parsed = parsed(
        &jsonl([json!({
            "timestamp": "2026-07-24T12:00:00Z",
            "type": "future_cursor_event",
            "future_payload": "must not become a body"
        })]),
        None,
    );

    assert!(parsed.events.is_empty());
    assert_eq!(parsed.stats.native_result_records, 1);
    assert_eq!(parsed.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(parsed.stats.result_hashes_created, 0);
}

#[test]
fn interrupted_65_part_record_replays_from_pre_record_frontier_without_loss() {
    const MACHINE: &str = "cursor-65-part-frontier-machine";
    let temp = tempdir();
    let root = temp.path().join("projects");
    let mut rows = (0..1_984)
        .map(|index| user(&format!("prefix-{index}")))
        .collect::<Vec<_>>();
    rows.push(json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": (0..65)
                .map(|index| json!({"type": "text", "text": format!("sibling-{index}")}))
                .collect::<Vec<_>>()
        }
    }));
    write_transcript(&root, "project", "frontier", &jsonl(rows));
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let first =
        import_cursor_with_limit(&root, &mut store, MACHINE, CaptureWorkLimit::OneSafeGroup);
    assert!(first.work_remaining);
    let session = store
        .session_by_external_session(CaptureProvider::Cursor, "frontier")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2_048);

    let second = import_cursor_with_limit(&root, &mut store, MACHINE, CaptureWorkLimit::Drain);
    assert!(!second.work_remaining);
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2_049);
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        2_049
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event
                .payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| text.starts_with("sibling-")))
            .count(),
        65
    );
}

#[test]
fn production_reports_all_rejections_beyond_the_sample_cap() {
    const MACHINE: &str = "cursor-rejection-total-machine";
    let temp = tempdir();
    let root = temp.path().join("projects");
    let rejection_count = CURSOR_REJECTION_SAMPLE_LIMIT + 17;
    let mut bytes = b"{\"malformed\"\n".repeat(rejection_count);
    bytes.extend_from_slice(&jsonl([user("survivor")]));
    write_transcript(&root, "project", "rejections", &bytes);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let summary = import_cursor_proof(&root, &mut store, MACHINE, ImportProfile::CoreOnly);

    assert_eq!(summary.failed, rejection_count);
    assert_eq!(summary.failures.len(), CURSOR_REJECTION_SAMPLE_LIMIT);
    let unchanged = import_cursor_proof(&root, &mut store, MACHINE, ImportProfile::CoreOnly);
    assert_eq!(unchanged.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(unchanged.failed, rejection_count);
    assert_eq!(unchanged.failures.len(), CURSOR_REJECTION_SAMPLE_LIMIT);
    let session = store
        .session_by_external_session(CaptureProvider::Cursor, "rejections")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn production_long_message_publishes_exact_locator_and_fails_closed_after_mutation() {
    const MACHINE: &str = "cursor-complete-message-machine";
    let temp = tempdir();
    let root = temp.path().join("projects");
    let complete_text = format!("{}tail", "x".repeat(PROVIDER_MAX_TEXT_CHARS + 97));
    let transcript = write_transcript(
        &root,
        "project",
        "long-message",
        &jsonl([user(&complete_text)]),
    );
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let summary = import_cursor_proof(&root, &mut store, MACHINE, ImportProfile::CoreOnly);
    assert_eq!(summary.imported_events, 1);
    let session = store
        .session_by_external_session(CaptureProvider::Cursor, "long-message")
        .unwrap()
        .unwrap();
    let event = store.events_for_session(session.id).unwrap().pop().unwrap();
    let indexed_text = event.payload["text"].as_str().unwrap().to_owned();
    assert_eq!(indexed_text.chars().count(), PROVIDER_MAX_TEXT_CHARS);
    assert_eq!(
        event.payload.pointer("/text_retention/truncated"),
        Some(&json!(true))
    );
    let collection = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = collection
        .locator(VerifiedContentRole::MessageBody)
        .unwrap()
        .clone();
    let source_locator = locator.source_locator().unwrap();
    let frozen = freeze_cursor_source(&one_source(&root)).unwrap();
    let importer_revision = cursor_complete_content_source_revision(frozen.observation());
    let (broker_revision, broker_identity) = cursor_complete_content_source_from_admitted(
        &fs::metadata(&transcript).unwrap(),
        crate::provider::importer::provider_path_identity(&fs::canonicalize(&transcript).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(broker_revision, importer_revision);
    assert_eq!(broker_identity, frozen.observation().locator_identity);
    let locator_value = source_locator.value();
    let mut revision_digest = Sha256::new();
    revision_digest.update(b"ctx-complete-content-source-revision-v1\0");
    revision_digest.update((importer_revision.len() as u64).to_be_bytes());
    revision_digest.update(importer_revision.as_bytes());
    assert_eq!(
        &locator_value[16..48],
        revision_digest.finalize().as_slice()
    );
    let mut identity_digest = Sha256::new();
    identity_digest.update(b"ctx-complete-content-path-identity-v1\0");
    identity_digest.update((broker_identity.len() as u64).to_be_bytes());
    identity_digest.update(broker_identity.as_bytes());
    assert_eq!(&locator_value[48..], identity_digest.finalize().as_slice());
    let stored_route = store.authorized_source_route_for_event(event.id).unwrap();
    let route = AuthorizedSourceRoute {
        source_id: stored_route.capture_source_id(),
        provider: stored_route.provider(),
        source_format: stored_route.source_format().to_owned(),
        family: CompleteContentSourceFamily::Jsonl,
        raw_source_path: stored_route.path().to_path_buf(),
        source_root: Some(root.clone()),
        source_identity: Some(stored_route.canonical_source_identity().to_owned()),
        source_snapshot: SourceSnapshot::default(),
    };
    let broker = SourceAccessBroker::new();
    let source_access = broker
        .admit_for_source_locators(
            route.clone(),
            std::slice::from_ref(&source_locator),
            event.id,
        )
        .unwrap();
    let request = CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Cursor,
        source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: locator.content_profile().to_owned(),
        source_locator: Some(source_locator.clone()),
        provider_session_id: session.external_session_id.clone(),
        source_record_ordinal: event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap(),
        source_record_subrecord_index: event.sync.metadata["source_record_subrecord_index"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .unwrap(),
        expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(locator.native_record_id().to_owned()),
        expected_record_digest: Some(locator.record_sha256().clone()),
        expected_content_ref: Some(locator.content_ref().clone()),
        indexed_text,
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    };
    let resolved = JsonlCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(resolved[0].text, complete_text);

    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let changed_access = broker
        .admit_for_source_locators(route, std::slice::from_ref(&source_locator), event.id)
        .unwrap();
    let mut changed_request = request;
    changed_request.source_access = changed_access;
    let error = JsonlCompleteContentResolver::new()
        .resolve(&[changed_request])
        .unwrap_err();
    assert!(matches!(
        error.kind,
        CompleteContentErrorKind::SourceChanged
            | CompleteContentErrorKind::ContentVerificationFailed
    ));
}

#[test]
fn cursor_nativepath_production_core_first_output_replay_is_independent_and_lifecycle_safe() {
    const MACHINE: &str = "cursor-nativepath-output-proof-machine";
    const INITIAL_BODY: &str = "cursor-nativepath-initial-success";
    const APPEND_BODY: &str = "cursor-nativepath-appended-success";
    const REWRITE_BODY: &str = "cursor-nativepath-rewritten-success";
    const REPLACEMENT_BODY: &str = "cursor-nativepath-replacement-success";
    const REDACTED_BODY: &str = "cursor-nativepath-redacted-secret";
    const CORE_PROMPT: &str = "Core-visible Cursor prompt";

    let temp = tempdir();
    let root = temp.path().join("projects");
    let mut rows = vec![user(CORE_PROMPT), call(0)];
    rows.extend((0..64).map(|index| result(index, INITIAL_BODY)));
    rows.push(json!({
        "timestamp": "2026-07-24T12:00:03Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "call_id": "preferred-call-64",
                "tool_use_id": "fallback-call-64",
                "content": format!("{INITIAL_BODY}-64"),
                "execution": {"exitCode": 9}
            }]
        }
    }));
    rows.push(json!({
        "timestamp": "2026-07-24T12:00:03Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "redacted-call",
                "content": REDACTED_BODY,
                "is_error": false,
                "redacted": true
            }]
        }
    }));
    let transcript = write_transcript(&root, "project", "output-proof", &jsonl(rows));
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(CursorRecordingOutputSink::new(store_path.clone()));
    sink.fail_pages.store(true, Ordering::SeqCst);

    let fresh = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::CoreAndPro(sink.clone()),
    );
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    assert!(sink.progress.lock().unwrap().is_none());

    let session = store
        .session_by_external_session(CaptureProvider::Cursor, "output-proof")
        .unwrap()
        .unwrap();
    let core_events = store.events_for_session(session.id).unwrap();
    let core_json = serde_json::to_string(&core_events).unwrap();
    assert!(core_events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    for secret in [INITIAL_BODY, REDACTED_BODY] {
        assert!(!core_json.contains(secret));
    }

    sink.fail_pages.store(false, Ordering::SeqCst);
    let catch_up = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(catch_up.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.pages.load(Ordering::SeqCst) >= 2);
    let outputs = sink.outputs.lock().unwrap();
    assert_eq!(outputs.len(), 65);
    assert_eq!(outputs[0].content, format!("{INITIAL_BODY}-0").as_bytes());
    assert_eq!(outputs[0].semantic_ordinal, 2);
    assert_eq!(outputs[0].subrecord_index, 0);
    assert_eq!(outputs[0].call_id.as_deref(), Some("call-0"));
    assert_eq!(outputs[0].outcome, OutputOutcome::Success);
    assert_eq!(outputs[64].call_id.as_deref(), Some("preferred-call-64"));
    assert_eq!(outputs[64].outcome, OutputOutcome::Failure);
    assert!(outputs.iter().all(|output| !output
        .content
        .windows(REDACTED_BODY.len())
        .any(|window| window == REDACTED_BODY.as_bytes())));
    drop(outputs);
    let pages_after_catch_up = sink.pages.load(Ordering::SeqCst);

    let idempotent = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(idempotent.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_catch_up);

    let mut append = OpenOptions::new().append(true).open(&transcript).unwrap();
    append
        .write_all(&jsonl([call(65), result(65, APPEND_BODY)]))
        .unwrap();
    drop(append);
    let behind_before_append = sink.behind.load(Ordering::SeqCst);
    let pages_before_append = sink.pages.load(Ordering::SeqCst);
    import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_before_append);
    assert_eq!(sink.behind.load(Ordering::SeqCst), behind_before_append + 1);

    let append_import = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::CoreAndPro(sink.clone()),
    );
    assert_eq!(
        append_import.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(sink.outputs.lock().unwrap().len(), 66);
    assert_eq!(
        sink.outputs.lock().unwrap().last().unwrap().content,
        format!("{APPEND_BODY}-65").as_bytes()
    );
    assert_eq!(*sink.epochs.lock().unwrap().last().unwrap(), 0);
    assert_eq!(
        *sink.dispositions.lock().unwrap().last().unwrap(),
        ProOutputSourceDisposition::AppendOrResume
    );

    fs::write(
        &transcript,
        jsonl([user(CORE_PROMPT), call(0), result(0, REWRITE_BODY)]),
    )
    .unwrap();
    let rewrite = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::CoreAndPro(sink.clone()),
    );
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.lock().unwrap().len(), 1);
    assert_eq!(
        sink.outputs.lock().unwrap()[0].content,
        format!("{REWRITE_BODY}-0").as_bytes()
    );
    assert_eq!(*sink.epochs.lock().unwrap().last().unwrap(), 1);
    assert_eq!(
        *sink.dispositions.lock().unwrap().last().unwrap(),
        ProOutputSourceDisposition::Rewrite
    );

    fs::write(&transcript, jsonl([user(CORE_PROMPT)])).unwrap();
    let truncation = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::CoreAndPro(sink.clone()),
    );
    assert_eq!(truncation.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.outputs.lock().unwrap().is_empty());
    assert_eq!(*sink.epochs.lock().unwrap().last().unwrap(), 2);

    let replacement = transcript.with_extension("replacement");
    fs::write(
        &replacement,
        jsonl([user(CORE_PROMPT), call(0), result(0, REPLACEMENT_BODY)]),
    )
    .unwrap();
    fs::remove_file(&transcript).unwrap();
    fs::rename(&replacement, &transcript).unwrap();
    let replaced = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::CoreAndPro(sink.clone()),
    );
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.lock().unwrap().len(), 1);
    assert_eq!(
        sink.outputs.lock().unwrap()[0].content,
        format!("{REPLACEMENT_BODY}-0").as_bytes()
    );
    assert_eq!(*sink.epochs.lock().unwrap().last().unwrap(), 3);

    let final_core = store.events_for_session(session.id).unwrap();
    let final_core_json = serde_json::to_string(&final_core).unwrap();
    for secret in [
        INITIAL_BODY,
        APPEND_BODY,
        REWRITE_BODY,
        REPLACEMENT_BODY,
        REDACTED_BODY,
    ] {
        assert!(!final_core_json.contains(secret));
    }

    fs::remove_file(&transcript).unwrap();
    let disappeared = import_cursor_proof(&root, &mut store, MACHINE, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    let pages_before_missing_replay = sink.pages.load(Ordering::SeqCst);
    let missing_replay = import_cursor_proof(
        &root,
        &mut store,
        MACHINE,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(missing_replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        sink.pages.load(Ordering::SeqCst),
        pages_before_missing_replay
    );
}

pub(super) fn import_cursor_proof(
    root: &Path,
    store: &mut Store,
    machine_id: &str,
    import_profile: ImportProfile,
) -> crate::ProviderImportSummary {
    import_cursor_native_history(
        root,
        store,
        CursorNativeImportOptions {
            machine_id: machine_id.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap()
}

fn import_cursor_with_limit(
    root: &Path,
    store: &mut Store,
    machine_id: &str,
    capture_work_limit: CaptureWorkLimit,
) -> crate::ProviderImportSummary {
    import_cursor_native_history(
        root,
        store,
        CursorNativeImportOptions {
            machine_id: machine_id.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            capture_work_limit,
            import_profile: ImportProfile::CoreOnly,
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap()
}

struct RecordedCursorOutput {
    content: Vec<u8>,
    semantic_ordinal: u64,
    subrecord_index: u32,
    call_id: Option<String>,
    outcome: OutputOutcome,
}

struct CursorRecordingOutputSink {
    store_path: PathBuf,
    fail_pages: AtomicBool,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    behind: AtomicUsize,
    saw_core_before_page: AtomicBool,
    outputs: Mutex<Vec<RecordedCursorOutput>>,
    epochs: Mutex<Vec<u64>>,
    dispositions: Mutex<Vec<ProOutputSourceDisposition>>,
    sources: Mutex<Vec<OutputSourceIdentity>>,
}

impl CursorRecordingOutputSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            fail_pages: AtomicBool::new(false),
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            outputs: Mutex::new(Vec::new()),
            epochs: Mutex::new(Vec::new()),
            dispositions: Mutex::new(Vec::new()),
            sources: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for CursorRecordingOutputSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "cursor-nativepath-output-proof-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .iter()
            .any(|session| session.provider == CaptureProvider::Cursor)
        {
            self.saw_core_before_page.store(true, Ordering::SeqCst);
        }
        if self.fail_pages.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "intentional Cursor output failure",
            ));
        }

        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        if matches!(
            page.disposition,
            ProOutputSourceDisposition::NewSource | ProOutputSourceDisposition::Rewrite
        ) {
            self.outputs.lock().unwrap().clear();
        }
        self.outputs
            .lock()
            .unwrap()
            .extend(
                page.observations
                    .into_iter()
                    .map(|output| RecordedCursorOutput {
                        content: output.content,
                        semantic_ordinal: output.coordinate.native_sequence,
                        subrecord_index: output.coordinate.source_record_subrecord_index.unwrap(),
                        call_id: output.call_id,
                        outcome: output.outcome.outcome,
                    }),
            );
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        self.epochs.lock().unwrap().push(page.source_epoch);
        self.dispositions.lock().unwrap().push(page.disposition);
        self.sources.lock().unwrap().push(page.source);
        self.pages.fetch_add(1, Ordering::SeqCst);
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs,
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

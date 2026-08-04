#[derive(Clone, Default)]
struct SharedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("shared test writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_ui() -> (Ui, SharedWriter) {
    let stdout = SharedWriter::default();
    let copy = stdout.clone();
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    (
        Ui::with_writers(stdout, context, SharedWriter::default(), stderr_context),
        copy,
    )
}

fn fixture_event(
    provider: CaptureProvider,
    source_format: &str,
    lineage: u8,
    sequence: u64,
) -> EventRecord {
    let source = SourceKey::derive(
        provider.as_str(),
        source_format,
        "fixture",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap();
    let native_session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8(format!("fixture-session-{lineage}")).unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    EventRecord {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source,
        provider: provider.as_str().to_owned(),
        source_format: source_format.to_owned(),
        provider_session_id: Some(format!("fixture-session-{lineage}")),
        native_event_id: Some(TypedKey::U64(sequence)),
        branch: None,
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: None,
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    }
}

fn fixture_core_event(event: &EventRecord, body: impl Into<String>) -> CoreEventRecord {
    let mut core_record = CoreRecord::new_selected(
        event.event_id,
        event.session_id,
        event.root_session_id,
        event.source.clone(),
        event.event_sequence,
        event.event_type.clone(),
        event.agent_type.clone(),
        event.is_primary,
        "source-index-test-v1",
        body,
    )
    .unwrap();
    core_record.parent_session_id = event.parent_session_id;
    core_record.provider_session_id = event.provider_session_id.clone();
    core_record.native_event_id = event.native_event_id.clone();
    core_record.occurred_at_unix_ms = event.occurred_at_unix_ms;
    core_record.role = event.role.clone();
    core_record.workspace = event.workspace.clone();
    core_record.branch = event.branch.clone();
    core_record.cwd = event.cwd.clone();
    core_record.validate_contract().unwrap();
    CoreEventRecord {
        event: event.clone(),
        core_record,
    }
}

fn fixture_search_presentation<'event>(
    event: &'event SearchEventMetadata,
    record: CoreEventRecord,
    snippet_truncated: bool,
) -> SearchPresentation<'event> {
    let snippet = record
        .core_record
        .content
        .normalized_body
        .as_ref()
        .expect("search fixture needs normalized body")
        .clone();
    SearchPresentation {
        event,
        snippet,
        snippet_truncated,
    }
}

fn request(refresh: RefreshArg) -> SourceSearchRequest {
    SourceSearchRequest {
        query: TEST_QUERY.to_owned(),
        terms: Vec::new(),
        limit: 10,
        provider: Some(CaptureProvider::Codex),
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        workspace: None,
        since: None,
        primary_only: false,
        include_subagents: false,
        event_type: None,
        file: None,
        session: None,
        events: false,
        include_current_session: true,
        backend: Some(SearchBackendArg::Lexical),
        semantic_weight: 0.35,
        semantic_enabled: true,
        semantic_daemon_enabled: true,
        refresh,
    }
}

fn write_test_generation(data_root: &Path) {
    let sessions = data_root.join("sessions");
    let source = sessions.join(format!("rollout-{TEST_SESSION_ID}.jsonl"));
    fs::create_dir_all(&sessions).unwrap();
    let records = [
        json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": TEST_SESSION_ID,
                "timestamp": "2026-07-28T12:00:00Z",
                "cwd": "/workspace/pinned",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "model_provider": "openai"
            }
        }),
        json!({
            "timestamp": "2026-07-28T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("{TEST_QUERY} sentinel")
                }]
            }
        }),
    ];
    let body = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect::<String>();
    fs::write(source, body).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Codex, sessions),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    refresh_source_backed_generation(index_root(data_root), &registry, WriterOptions::default())
        .unwrap();
}

fn append_fixture_event(data_root: &Path, event: EventRecord, revision: u8) {
    let source = event.source.clone();
    let core_record = fixture_core_event(&event, "ambiguous provider session fixture").core_record;
    let mut writer = GenerationWriter::open(
        index_root(data_root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(core_record).unwrap();
    let observation =
        SourceObservation::new(source, "fixture-revision-v1", vec![revision]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "fixture-parser-v1",
                [revision; 32],
                ScannedSourceCounts {
                    complete_records: 1,
                    retained_records: 1,
                    indexed_documents: 1,
                    certified_bytes: 1,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
}

fn append_fixture_session(data_root: &Path, events: &[CoreEventRecord], revision: u8) {
    let source = events.first().unwrap().source.clone();
    assert!(events.iter().all(|event| event.source == source));
    let mut writer = GenerationWriter::open(
        index_root(data_root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for event in events {
        writer.add_core_record(event.core_record.clone()).unwrap();
    }
    let observation =
        SourceObservation::new(source, "fixture-session-revision-v1", vec![revision]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "fixture-parser-v1",
                [revision; 32],
                ScannedSourceCounts {
                    complete_records: events.len() as u64,
                    retained_records: events.len() as u64,
                    indexed_documents: events.len() as u64,
                    certified_bytes: 1,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
}

fn sorted_json_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .expect("schema snapshot target must be an object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

fn show_session_args(id: Option<&str>, provider_session: Option<&str>) -> ShowSessionArgs {
    ShowSessionArgs {
        id: id.map(str::to_owned),
        provider: None,
        provider_session: provider_session.map(str::to_owned),
        mode: TranscriptMode::Lite,
        max_events: None,
        format: OutputFormat::Json,
        out: None,
    }
}

fn show_event_args(id: &str) -> ShowEventArgs {
    ShowEventArgs {
        id: id.to_owned(),
        before: 0,
        after: 0,
        window: None,
        format: OutputFormat::Json,
    }
}

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

struct BuiltinSemanticTestHost;

static BUILTIN_SEMANTIC_TEST_HOST: BuiltinSemanticTestHost = BuiltinSemanticTestHost;
static INSTALL_BUILTIN_SEMANTIC_TEST_HOST: std::sync::Once = std::sync::Once::new();

fn install_builtin_semantic_test_host() {
    INSTALL_BUILTIN_SEMANTIC_TEST_HOST.call_once(|| {
        ctx_daemon_cli::install_host(&BUILTIN_SEMANTIC_TEST_HOST)
            .expect("install the ctx-history-cli semantic test host");
    });
}

impl ctx_daemon_cli::DaemonCliHost for BuiltinSemanticTestHost {
    fn load_config(&self, _data_root: &Path) -> anyhow::Result<ctx_daemon_cli::AppConfig<'static>> {
        Ok(ctx_daemon_cli::AppConfig::default())
    }

    fn persisted_daemon_enabled(&self, _data_root: &Path) -> anyhow::Result<bool> {
        Ok(true)
    }

    fn set_daemon_enabled(&self, _data_root: &Path, _enabled: bool) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "the ctx-history-cli semantic test host cannot mutate daemon config"
        ))
    }

    fn home_dir(&self) -> Option<PathBuf> {
        None
    }

    fn run_daemon_service(
        &self,
        _data_root: &Path,
        _request: ctx_daemon_cli::DaemonHostRunRequest,
        _config: &ctx_daemon_cli::AppConfig<'_>,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "the ctx-history-cli semantic test host cannot run a daemon"
        ))
    }

    fn deliver_daemon_events(
        &self,
        _data_root: &Path,
        _events: &[ctx_client_observability::analytics::PublicEventV1],
    ) {
    }

    fn fetch_to_writer(
        &self,
        _endpoint: &str,
        _max_bytes: u64,
        _timeout: std::time::Duration,
        _writer: &mut dyn Write,
    ) -> anyhow::Result<u64> {
        Err(anyhow::anyhow!(
            "the ctx-history-cli semantic test host cannot fetch artifacts"
        ))
    }
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
        root_session_id: None,
        session_relationship: None,
        event_copy: None,
        source,
        provider: provider.as_str().to_owned(),
        source_format: source_format.to_owned(),
        provider_session_id: Some(format!("fixture-session-{lineage}")),
        native_event_id: Some(TypedKey::U64(sequence)),
        agent_scope: Some(CoreAgentScope::Primary),
        event_sequence: sequence,
        occurred_at_unix_ms: None,
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
    }
}

fn fixture_copied_event(
    lineage: u8,
    ancestor: &EventRecord,
    claimed_root: &EventRecord,
) -> EventRecord {
    let mut event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", lineage, 1);
    event.parent_session_id = Some(ancestor.session_id);
    event.root_session_id = Some(claimed_root.session_id);
    event.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    event.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: ancestor.session_id,
        ancestor_event_id: ancestor.event_id,
        proof: ProviderNativeCopyProof::NativeEventIdentity,
    });
    event
}

fn fixture_core_event(event: &EventRecord, body: impl Into<String>) -> CoreEventRecord {
    let mut core_record = CoreRecord::new_selected(
        event.event_id,
        event.session_id,
        event.source.clone(),
        event.event_sequence,
        event.event_type.clone(),
        "source-index-test-v1",
        body,
    )
    .unwrap();
    core_record.parent_session_id = event.parent_session_id;
    core_record.root_session_id = event.root_session_id;
    core_record.session_relationship = event.session_relationship;
    core_record.event_copy = event.event_copy.clone();
    core_record.provider_session_id = event.provider_session_id.clone();
    core_record.native_event_id = event.native_event_id.clone();
    core_record.occurred_at_unix_ms = event.occurred_at_unix_ms;
    core_record.role = event.role.clone();
    core_record.agent_scope = event.agent_scope;
    core_record.validate_contract().unwrap();
    let mut projected_event = event.clone();
    projected_event.session_relationship = core_record.session_relationship;
    projected_event.event_copy = core_record.event_copy.clone();
    CoreEventRecord {
        event: projected_event,
        core_record,
    }
}

fn mcp_fixture_show_event(root: &Path, event: &CoreEventRecord) -> Value {
    mcp_show_event(
        root,
        &event.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap()
}

fn fixture_search_presentation(
    event: &SearchEventMetadata,
    record: CoreEventRecord,
    snippet_truncated: bool,
) -> SearchPresentation {
    let snippet = record
        .core_record
        .content
        .normalized_body
        .as_ref()
        .expect("search fixture needs normalized body")
        .clone();
    SearchPresentation {
        event_id: event.event_id,
        snippet,
        snippet_truncated,
    }
}

fn request(_refresh: RefreshArg) -> SourceSearchRequest {
    SourceSearchRequest {
        query: TEST_QUERY.to_owned(),
        terms: Vec::new(),
        limit: 10,
        provider: Some(CaptureProvider::Codex),
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        source_roots: Vec::new(),
        source_groups: Vec::new(),
        workspace: None,
        since: None,
        primary_only: false,
        content_scope: SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: false,
        include_current_session: true,
        backend: Some(SearchBackendArg::Lexical),
        semantic_weight: 0.35,
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
    .unwrap()
    .into_writer()
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
    .unwrap()
    .into_writer()
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
        provider_key: None,
        source_id: None,
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

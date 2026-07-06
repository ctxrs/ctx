#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct CursorNativeImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CursorNativeImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CursorAgentTranscriptJsonlAdapter;

impl ProviderCaptureAdapter for CursorAgentTranscriptJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cursor
    }

    fn source_format(&self) -> &str {
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        )
    }
}

pub fn import_cursor_native_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CursorNativeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        CursorAgentTranscriptJsonlAdapter,
    )
}

pub(crate) const CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl";

#[derive(Debug, Clone, Default)]
pub(crate) struct CustomHistoryJsonlV1NormalizationResult {
    pub(crate) provider: ProviderNormalizationResult,
    pub(crate) edges: Vec<(usize, CustomHistoryJsonlV1EdgeImport)>,
    pub(crate) source_cursors: Vec<CustomHistoryJsonlV1SourceCursorImport>,
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistoryJsonlV1SourceCursorImport {
    pub(crate) machine_id: String,
    pub(crate) checkpoint: ProviderCursorCheckpoint,
}

pub(crate) fn custom_history_cursor_stream(source: &CtxHistoryJsonlSourceRecord) -> String {
    custom_history_jsonl_v1_cursor_stream(
        &source.provider_key,
        &source.source_id,
        &source.source_format,
    )
}

pub fn custom_history_jsonl_v1_cursor_stream(
    provider_key: &str,
    source_id: &str,
    source_format: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "cursor_stream",
        "provider_key": provider_key,
        "source_id": source_id,
        "source_format": source_format,
    }));
    let stream_id = stable_capture_uuid(&key, "custom-cursor-stream");
    format!("provider:custom:{provider_key}:{stream_id}")
}

pub(crate) fn custom_history_normalized_cursor_range(
    source: &CtxHistoryJsonlSourceRecord,
    cursor: &ProviderCursorRange,
) -> ProviderCursorRange {
    ProviderCursorRange {
        before: cursor
            .before
            .as_ref()
            .map(|checkpoint| custom_history_normalized_cursor_checkpoint(source, checkpoint)),
        after: cursor
            .after
            .as_ref()
            .map(|checkpoint| custom_history_normalized_cursor_checkpoint(source, checkpoint)),
    }
}

pub(crate) fn custom_history_normalized_cursor_checkpoint(
    source: &CtxHistoryJsonlSourceRecord,
    checkpoint: &ProviderCursorCheckpoint,
) -> ProviderCursorCheckpoint {
    ProviderCursorCheckpoint {
        stream: custom_history_cursor_stream(source),
        cursor: checkpoint.cursor.clone(),
        observed_at: checkpoint.observed_at,
    }
}

pub(crate) fn import_custom_history_source_cursors(
    store: &mut Store,
    cursors: &[CustomHistoryJsonlV1SourceCursorImport],
) -> Result<()> {
    for cursor in cursors {
        store.upsert_sync_cursor(&SyncCursor {
            id: stable_capture_uuid(
                &format!(
                    "provider-cursor:{}:{}:{}",
                    CaptureProvider::Custom.as_str(),
                    cursor.machine_id,
                    cursor.checkpoint.stream
                ),
                "provider-sync-cursor",
            ),
            team_id: None,
            device_id: cursor.machine_id.clone(),
            stream: cursor.checkpoint.stream.clone(),
            cursor: cursor.checkpoint.cursor.clone(),
            last_synced_at: Some(cursor.checkpoint.observed_at),
            timestamps: timestamps(cursor.checkpoint.observed_at),
        })?;
    }
    Ok(())
}

pub(crate) struct NativeEventDraft {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) body: Value,
    pub(crate) metadata: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsEventDraft {
    pub(crate) thread_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) cursor: String,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) message: DeepAgentsMessage,
    pub(crate) checkpoint_id: String,
    pub(crate) task_id: String,
    pub(crate) write_idx: i64,
    pub(crate) message_offset: usize,
}

pub(crate) fn deepagents_decode_msgpack(value: &[u8]) -> Result<MsgpackValue> {
    let mut cursor = std::io::Cursor::new(value);
    read_msgpack_value(&mut cursor).map_err(|err| {
        CaptureError::InvalidPayload(format!("invalid Deep Agents msgpack payload: {err}"))
    })
}

pub(crate) fn fixture_line_to_capture(
    fixture: &ProviderFixtureLine,
    context: &ProviderAdapterContext,
    source_format: &str,
    fidelity: Fidelity,
) -> ProviderCaptureEnvelope {
    let cursor = fixture
        .event
        .as_ref()
        .and_then(|event| event.cursor.as_ref())
        .map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: provider_cursor_stream(fixture.provider, source_format),
                cursor: cursor.clone(),
                observed_at: fixture
                    .event
                    .as_ref()
                    .map(|event| event.occurred_at)
                    .unwrap_or(context.imported_at),
            }),
        });

    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: fixture.provider,
        source: ProviderSourceEnvelope {
            source_format: source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::Fixture,
            fidelity,
            cursor,
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                fixture.provider.as_str(),
                source_format,
                fixture.session.provider_session_id
            )),
            metadata: json!({
                "adapter": "provider_fixture_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: fixture.session.provider_session_id.clone(),
            parent_provider_session_id: fixture.session.parent_provider_session_id.clone(),
            root_provider_session_id: fixture.session.root_provider_session_id.clone(),
            external_agent_id: fixture.session.external_agent_id.clone(),
            agent_type: fixture.session.agent_type,
            role_hint: fixture.session.role_hint.clone(),
            is_primary: fixture.session.is_primary,
            status: fixture.session.status,
            started_at: fixture.session.started_at,
            ended_at: fixture.session.ended_at,
            cwd: fixture.session.cwd.clone(),
            fidelity,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                fixture.provider.as_str(),
                fixture.session.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: fixture.session.metadata.clone(),
        },
        event: fixture.event.as_ref().map(|event| ProviderEventEnvelope {
            provider_event_index: event.provider_event_index,
            provider_event_hash: event.provider_event_hash.clone(),
            cursor: event.cursor.clone(),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            fidelity,
            redaction_state: RedactionState::LocalPreview,
            idempotency_key: Some(format!(
                "provider-event:{}:{}:{}",
                fixture.provider.as_str(),
                fixture.session.provider_session_id,
                event.provider_event_index
            )),
            artifacts: Vec::new(),
            payload: event.payload.clone(),
            metadata: event.metadata.clone(),
        }),
    }
}

pub(crate) fn provider_cursor_stream(provider: CaptureProvider, source_format: &str) -> String {
    format!("provider:{}:{}", provider.as_str(), source_format)
}

pub(crate) fn persist_provider_cursor(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
) -> Result<()> {
    let checkpoint = capture
        .source
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.after.as_ref())
        .cloned()
        .or_else(|| {
            capture.event.as_ref().and_then(|event| {
                event
                    .cursor
                    .as_ref()
                    .map(|cursor| ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(
                            capture.provider,
                            &capture.source.source_format,
                        ),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    })
            })
        });
    let Some(checkpoint) = checkpoint else {
        return Ok(());
    };

    store.upsert_sync_cursor(&SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                capture.provider.as_str(),
                capture.source.machine_id,
                checkpoint.stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: capture.source.machine_id.clone(),
        stream: checkpoint.stream,
        cursor: checkpoint.cursor,
        last_synced_at: Some(checkpoint.observed_at),
        timestamps: timestamps(checkpoint.observed_at),
    })?;
    Ok(())
}

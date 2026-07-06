#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct CustomHistoryJsonlV1ImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CustomHistoryJsonlV1ImportOptions {
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

pub fn import_custom_history_jsonl_v1(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = normalize_custom_history_jsonl_v1(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    if normalization.provider.summary.failed > 0 && !options.allow_partial_failures {
        return Ok(normalization.provider.summary);
    }

    let mut summary = import_normalized_provider_captures(
        store,
        normalization.provider,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )?;
    import_custom_history_edges(
        store,
        &normalization.edges,
        options.history_record_id,
        options.allow_partial_failures,
        &mut summary,
    )?;
    import_custom_history_source_cursors(store, &normalization.source_cursors)?;
    Ok(summary)
}

pub fn import_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let normalization = normalize_custom_history_jsonl_v1_reader(
        reader,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    if normalization.provider.summary.failed > 0 && !options.allow_partial_failures {
        return Ok(normalization.provider.summary);
    }

    let mut summary = import_normalized_provider_captures(
        store,
        normalization.provider,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )?;
    import_custom_history_edges(
        store,
        &normalization.edges,
        options.history_record_id,
        options.allow_partial_failures,
        &mut summary,
    )?;
    import_custom_history_source_cursors(store, &normalization.source_cursors)?;
    Ok(summary)
}

pub fn validate_custom_history_jsonl_v1(path: impl AsRef<Path>) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let normalization = normalize_custom_history_jsonl_v1(
        path,
        &ProviderAdapterContext {
            source_path: Some(path.to_path_buf()),
            ..ProviderAdapterContext::default()
        },
    )?;
    Ok(normalization.provider.summary)
}

pub fn validate_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
) -> Result<ProviderImportSummary> {
    let normalization =
        normalize_custom_history_jsonl_v1_reader(reader, &ProviderAdapterContext::default())?;
    Ok(normalization.provider.summary)
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistoryJsonlV1EdgeImport {
    pub(crate) provider_key: String,
    pub(crate) source_id: String,
    pub(crate) source_format: String,
    pub(crate) raw_source_path: Option<String>,
    pub(crate) from_provider_session_id: String,
    pub(crate) to_provider_session_id: String,
    pub(crate) edge_id: Option<String>,
    pub(crate) edge_type: SessionEdgeType,
    pub(crate) confidence: Confidence,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) fidelity: Fidelity,
    pub(crate) metadata: Value,
}

pub(crate) fn normalize_custom_history_jsonl_v1(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<CustomHistoryJsonlV1NormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    normalize_custom_history_jsonl_v1_reader(reader, context)
}

pub(crate) fn normalize_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    context: &ProviderAdapterContext,
) -> Result<CustomHistoryJsonlV1NormalizationResult> {
    let mut reader = reader;
    let mut summary = ProviderImportSummary::default();
    let mut records = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<CtxHistoryJsonlRecord>(&line) {
            Ok(record) => records.push((line_number, record)),
            Err(err) => push_provider_import_failure(&mut summary, line_number, err.to_string()),
        }
    }

    if summary.failed > 0 {
        return Ok(custom_history_failed_normalization(summary));
    }

    let mut manifest_line = None;
    let mut sources = BTreeMap::<String, (usize, CtxHistoryJsonlSourceRecord)>::new();
    let mut sessions = BTreeMap::<(String, String), (usize, CtxHistoryJsonlSessionRecord)>::new();
    let mut events = Vec::<(usize, CtxHistoryJsonlEventRecord)>::new();
    let mut event_keys = BTreeSet::<(String, String, u64)>::new();
    let mut file_touches = Vec::<(usize, CtxHistoryJsonlFileTouchRecord)>::new();
    let mut touch_keys = BTreeSet::<(String, String, u64)>::new();
    let mut edges = Vec::<(usize, CtxHistoryJsonlEdgeRecord)>::new();
    let mut edge_keys = BTreeSet::<(String, String, String, String)>::new();

    for (line_number, record) in records {
        match record {
            CtxHistoryJsonlRecord::Manifest(manifest) => {
                if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        format!(
                            "unsupported custom history schema version `{}`",
                            manifest.schema_version
                        ),
                    );
                }
                if manifest_line.replace(line_number).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate manifest record".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Source(source) => {
                validate_custom_source_record(&mut summary, line_number, &source);
                if sources
                    .insert(source.source_id.clone(), (line_number, source))
                    .is_some()
                {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate source_id".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Session(session) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &session.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &session.session_id,
                );
                let key = (session.source_id.clone(), session.session_id.clone());
                if sessions.insert(key, (line_number, session)).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate session record".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Event(event) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &event.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &event.session_id,
                );
                let key = (
                    event.source_id.clone(),
                    event.session_id.clone(),
                    event.event_index,
                );
                if !event_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate event_index for session".to_owned(),
                    );
                }
                events.push((line_number, event));
            }
            CtxHistoryJsonlRecord::FileTouch(file_touch) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &file_touch.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &file_touch.session_id,
                );
                if file_touch.path.trim().is_empty() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "file_touch path must not be empty".to_owned(),
                    );
                }
                let key = (
                    file_touch.source_id.clone(),
                    file_touch.session_id.clone(),
                    file_touch.touch_index,
                );
                if !touch_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate touch_index for session".to_owned(),
                    );
                }
                file_touches.push((line_number, file_touch));
            }
            CtxHistoryJsonlRecord::Edge(edge) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &edge.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "from_session_id",
                    &edge.from_session_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "to_session_id",
                    &edge.to_session_id,
                );
                let edge_key = edge.edge_id.clone().unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        edge.from_session_id,
                        edge.to_session_id,
                        edge.edge_type.as_str()
                    )
                });
                let key = (
                    edge.source_id.clone(),
                    edge.from_session_id.clone(),
                    edge.to_session_id.clone(),
                    edge_key,
                );
                if !edge_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate edge record".to_owned(),
                    );
                }
                edges.push((line_number, edge));
            }
        }
    }

    let reference_index = CustomHistoryReferenceIndex {
        manifest_line,
        sources: &sources,
        sessions: &sessions,
        events: &events,
        event_keys: &event_keys,
        file_touches: &file_touches,
        edges: &edges,
    };
    validate_custom_history_references(&mut summary, reference_index);
    if summary.failed > 0 {
        return Ok(custom_history_failed_normalization(summary));
    }

    let mut result = ProviderNormalizationResult {
        summary,
        ..ProviderNormalizationResult::default()
    };
    let mut source_cursors = Vec::new();
    for (_, source) in sources.values() {
        let machine_id = source
            .machine_id
            .clone()
            .unwrap_or_else(|| context.machine_id.clone());
        if let Some(after) = source
            .cursor
            .as_ref()
            .and_then(|cursor| custom_history_normalized_cursor_range(source, cursor).after)
        {
            source_cursors.push(CustomHistoryJsonlV1SourceCursorImport {
                machine_id,
                checkpoint: after,
            });
        }
    }
    for (line_number, session) in sessions.values() {
        let source = &sources
            .get(&session.source_id)
            .expect("session source already validated")
            .1;
        result.captures.push((
            *line_number,
            custom_history_session_capture(source, session, None, context),
        ));
    }
    for (line_number, event) in events {
        let (_, session) = sessions
            .get(&(event.source_id.clone(), event.session_id.clone()))
            .expect("event session already validated");
        let source = &sources
            .get(&event.source_id)
            .expect("event source already validated")
            .1;
        let envelope = custom_history_event_envelope(source, &event);
        result.captures.push((
            line_number,
            custom_history_session_capture(source, session, Some(envelope), context),
        ));
    }
    for (line_number, file_touch) in file_touches {
        let source = &sources
            .get(&file_touch.source_id)
            .expect("file_touch source already validated")
            .1;
        result.files_touched.push((
            line_number,
            custom_history_file_touch_envelope(source, &file_touch, context),
        ));
    }

    let mut custom_edges = Vec::new();
    for (line_number, edge) in edges {
        let source = &sources
            .get(&edge.source_id)
            .expect("edge source already validated")
            .1;
        custom_edges.push((
            line_number,
            custom_history_edge_import(source, &edge, context),
        ));
    }

    Ok(CustomHistoryJsonlV1NormalizationResult {
        provider: result,
        edges: custom_edges,
        source_cursors,
    })
}

pub(crate) fn custom_history_failed_normalization(
    summary: ProviderImportSummary,
) -> CustomHistoryJsonlV1NormalizationResult {
    CustomHistoryJsonlV1NormalizationResult {
        provider: ProviderNormalizationResult {
            summary,
            ..ProviderNormalizationResult::default()
        },
        edges: Vec::new(),
        source_cursors: Vec::new(),
    }
}

pub(crate) fn validate_custom_source_record(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    source: &CtxHistoryJsonlSourceRecord,
) {
    validate_custom_history_identifier(summary, line_number, "source_id", &source.source_id);
    validate_custom_history_identifier(
        summary,
        line_number,
        "source_format",
        &source.source_format,
    );
    let valid = !source.provider_key.is_empty()
        && source.provider_key.len() <= 128
        && source.provider_key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && source
            .provider_key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !valid {
        push_provider_import_failure(
            summary,
            line_number,
            "provider_key must be 1 to 128 bytes, start with a lowercase ASCII letter or digit, and use only lowercase ASCII letters, digits, '.', '_', or '-'".to_owned(),
        );
    }
}

pub(crate) fn validate_custom_history_identifier(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    field: &str,
    value: &str,
) {
    let error = if value.trim().is_empty() {
        Some(format!("{field} must not be empty"))
    } else if value.len() > 512 {
        Some(format!("{field} must be at most 512 bytes"))
    } else if value.chars().any(char::is_control) {
        Some(format!("{field} must not contain control characters"))
    } else {
        None
    };
    if let Some(error) = error {
        push_provider_import_failure(summary, line_number, error);
    }
}

pub(crate) struct CustomHistoryReferenceIndex<'a> {
    pub(crate) manifest_line: Option<usize>,
    pub(crate) sources: &'a BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    pub(crate) sessions: &'a BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    pub(crate) events: &'a [(usize, CtxHistoryJsonlEventRecord)],
    pub(crate) event_keys: &'a BTreeSet<(String, String, u64)>,
    pub(crate) file_touches: &'a [(usize, CtxHistoryJsonlFileTouchRecord)],
    pub(crate) edges: &'a [(usize, CtxHistoryJsonlEdgeRecord)],
}

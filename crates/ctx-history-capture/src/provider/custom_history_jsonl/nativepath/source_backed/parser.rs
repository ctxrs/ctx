use super::*;
use crate::provider::custom_history_jsonl::CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES;

#[derive(Debug)]
struct TouchCandidate {
    line_number: usize,
    byte_offset: u64,
    source_id: String,
    session_id: String,
    event_index: Option<u64>,
    path: Option<TouchSpoolRef>,
}

#[derive(Debug)]
struct EdgeCandidate {
    line_number: usize,
    source_id: String,
    from_session_id: String,
    to_session_id: String,
    edge_type: SessionEdgeType,
}

#[derive(Debug)]
struct ProjectionCatalog {
    summary: ProviderImportSummary,
    manifest_line: Option<usize>,
    manifest_failure: Option<(ProviderSourceFailureKind, String)>,
    sources: BTreeMap<String, CustomSourceCatalogEntry>,
    sessions: BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    events: BTreeMap<CustomEventKey, CustomEventCatalogEntry>,
    touch_keys: BTreeSet<(String, String, u64)>,
    touches: Vec<TouchCandidate>,
    edge_keys: BTreeSet<(String, String, String, String)>,
    edges: Vec<EdgeCandidate>,
    oversized_lines: BTreeSet<usize>,
    touch_spool_bytes: u64,
    budget: CatalogBudget,
}

impl ProjectionCatalog {
    fn new(limits: CustomHistoryCatalogLimits) -> Self {
        Self {
            summary: ProviderImportSummary::default(),
            manifest_line: None,
            manifest_failure: None,
            sources: BTreeMap::new(),
            sessions: BTreeMap::new(),
            events: BTreeMap::new(),
            touch_keys: BTreeSet::new(),
            touches: Vec::new(),
            edge_keys: BTreeSet::new(),
            edges: Vec::new(),
            oversized_lines: BTreeSet::new(),
            touch_spool_bytes: 0,
            budget: CatalogBudget::new(limits),
        }
    }
}

pub(super) fn parse_projection(
    source: &OpenedProviderSourceFile,
    prior_prefix_bytes: Option<u64>,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    parse_projection_with_limits(
        source,
        prior_prefix_bytes,
        CustomHistoryCatalogLimits::PRODUCTION,
    )
}

pub(super) fn parse_projection_with_limits(
    source: &OpenedProviderSourceFile,
    prior_prefix_bytes: Option<u64>,
    limits: CustomHistoryCatalogLimits,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.projection_parses = work.projection_parses.saturating_add(1);
        work.source_read_passes = work.source_read_passes.saturating_add(1);
    });

    let frozen_length = source.len();
    let mut file = source.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut event_spool = tempfile::tempfile()?;
    let mut touch_spool = tempfile::tempfile()?;
    let mut catalog = ProjectionCatalog::new(limits);
    let mut source_hasher = new_prefix_hasher();
    let mut committed_source_hasher = source_hasher.clone();
    let mut prior_hasher = prior_prefix_bytes.map(|_| new_prefix_hasher());
    let mut prior_observed_bytes = 0_u64;
    let mut line_hasher = Sha256::new();
    let mut line = Vec::new();
    let mut line_oversized = false;
    let mut byte_offset = 0_u64;
    let mut line_start = 0_u64;
    let mut line_number = 0_usize;

    {
        let mut event_writer = BufWriter::new(&mut event_spool);
        let mut touch_writer = BufWriter::new(&mut touch_spool);
        while byte_offset < frozen_length {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            let remaining = frozen_length.saturating_sub(byte_offset);
            let available_len = usize::try_from(remaining.min(available.len() as u64))
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
            let available = &available[..available_len];
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index.saturating_add(1));
            let chunk = &available[..take];
            source_hasher.update(chunk);
            line_hasher.update(chunk);
            if let (Some(prior_prefix_bytes), Some(prior_hasher)) =
                (prior_prefix_bytes, prior_hasher.as_mut())
            {
                let prior_remaining = prior_prefix_bytes.saturating_sub(prior_observed_bytes);
                let prior_take = usize::try_from(prior_remaining.min(chunk.len() as u64))
                    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
                prior_hasher.update(&chunk[..prior_take]);
                prior_observed_bytes = prior_observed_bytes.saturating_add(prior_take as u64);
            }
            if !line_oversized {
                if line.len().saturating_add(chunk.len()) > MAX_PROVIDER_JSONL_LINE_BYTES {
                    line.clear();
                    line_oversized = true;
                } else {
                    line.extend_from_slice(chunk);
                    #[cfg(test)]
                    record_custom_history_work(|work| {
                        work.peak_provider_record_bytes =
                            work.peak_provider_record_bytes.max(line.len());
                    });
                }
            }
            byte_offset = byte_offset
                .checked_add(chunk.len() as u64)
                .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
            let complete = chunk.last() == Some(&b'\n');
            reader.consume(take);
            if !complete {
                continue;
            }

            line_number = line_number
                .checked_add(1)
                .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
            let byte_length = byte_offset
                .checked_sub(line_start)
                .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
            let evidence = CompleteLine {
                line_number,
                byte_offset: line_start,
                byte_length,
                physical_ordinal: u64::try_from(line_number.saturating_sub(1))
                    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
                record_digest: line_hasher.clone().finalize().into(),
            };
            catalog.budget.admit_record()?;
            if line_oversized {
                catalog.summary.skipped = catalog.summary.skipped.saturating_add(1);
                catalog.summary.skipped_events = catalog.summary.skipped_events.saturating_add(1);
                catalog.oversized_lines.insert(line_number);
            } else {
                visit_record(
                    &line,
                    evidence,
                    &mut catalog,
                    &mut event_writer,
                    &mut touch_writer,
                )?;
            }
            committed_source_hasher = source_hasher.clone();
            line.clear();
            line_oversized = false;
            line_hasher = Sha256::new();
            line_start = byte_offset;
        }
        event_writer.flush()?;
        touch_writer.flush()?;
    }

    event_spool.seek(SeekFrom::Start(0))?;
    touch_spool.seek(SeekFrom::Start(0))?;
    let terminal = line_start == frozen_length;
    let certified_prefix_bytes = line_start;
    let content_digest = finish_prefix_digest(&committed_source_hasher, certified_prefix_bytes);
    let observed_prior_prefix_digest = match (prior_prefix_bytes, prior_hasher) {
        (Some(expected), Some(hasher)) if prior_observed_bytes == expected => {
            Some(finish_prefix_digest(&hasher, expected))
        }
        _ => None,
    };
    finish_projection(
        catalog,
        event_spool,
        touch_spool,
        certified_prefix_bytes,
        line_number,
        terminal,
        content_digest,
        observed_prior_prefix_digest,
        prior_prefix_bytes,
    )
}

fn visit_record(
    bytes: &[u8],
    line: CompleteLine,
    catalog: &mut ProjectionCatalog,
    event_writer: &mut impl Write,
    touch_writer: &mut impl Write,
) -> CustomHistorySourceBackedResult<()> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.provider_records_parsed = work.provider_records_parsed.saturating_add(1);
    });
    let record = match serde_json::from_slice::<CtxHistoryJsonlRecord>(bytes) {
        Ok(record) => record,
        Err(error) => {
            push_provider_import_failure(&mut catalog.summary, line.line_number, error.to_string());
            return Ok(());
        }
    };
    match record {
        CtxHistoryJsonlRecord::Manifest(manifest) => {
            if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
                catalog.manifest_failure.get_or_insert_with(|| {
                    (
                        ProviderSourceFailureKind::SchemaIncompatible,
                        format!(
                            "unsupported custom history schema version `{}`",
                            manifest.schema_version
                        ),
                    )
                });
            }
            if catalog.manifest_line.replace(line.line_number).is_some() {
                catalog.manifest_failure = Some((
                    ProviderSourceFailureKind::InvalidSource,
                    format!("duplicate manifest record at line {}", line.line_number),
                ));
            }
        }
        CtxHistoryJsonlRecord::Source(source) => {
            let failures_before = catalog.summary.failed;
            validate_custom_source_record(&mut catalog.summary, line.line_number, &source);
            if catalog.sources.contains_key(&source.source_id) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate source_id".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                let raw_source_path = source.raw_source_path.as_deref().and_then(bounded_metadata);
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    source.source_id.len(),
                    source.provider_key.len(),
                    raw_source_path.as_ref().map_or(0, String::len),
                ]))?;
                catalog.sources.insert(
                    source.source_id.clone(),
                    CustomSourceCatalogEntry {
                        provider_key: source.provider_key,
                        raw_source_path,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::Session(session) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::ParentSessionIdBytes,
                session.parent_session_id.as_deref(),
            )?;
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::RootSessionIdBytes,
                session.root_session_id.as_deref(),
            )?;
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &session.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "session_id",
                &session.session_id,
            );
            let key = (session.source_id.clone(), session.session_id.clone());
            if catalog.sessions.contains_key(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate session record".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                let agent_type = session.agent_type.as_str().to_owned();
                let cwd = session.cwd.as_deref().and_then(bounded_metadata);
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    session.source_id.len().saturating_mul(2),
                    session.session_id.len().saturating_mul(2),
                    session.parent_session_id.as_ref().map_or(0, String::len),
                    session.root_session_id.as_ref().map_or(0, String::len),
                    agent_type.len(),
                    cwd.as_ref().map_or(0, String::len),
                ]))?;
                catalog.sessions.insert(
                    key,
                    CustomSessionCatalogEntry {
                        line_number: line.line_number,
                        source_id: session.source_id,
                        session_id: session.session_id,
                        parent_session_id: session.parent_session_id,
                        root_session_id: session.root_session_id,
                        agent_type,
                        is_primary: session.is_primary,
                        cwd,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::Event(event) => {
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &event.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "session_id",
                &event.session_id,
            );
            let key = (
                event.source_id.clone(),
                event.session_id.clone(),
                event.event_index,
            );
            if catalog.events.contains_key(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate event_index for session".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    event.source_id.len(),
                    event.session_id.len(),
                ]))?;
                let body = lexical_body(&event);
                #[cfg(test)]
                record_custom_history_work(|work| {
                    work.spooled_event_body_bytes =
                        work.spooled_event_body_bytes.saturating_add(body.len());
                    work.resident_event_body_bytes = body.len();
                    work.peak_resident_event_body_bytes =
                        work.peak_resident_event_body_bytes.max(body.len());
                });
                write_spooled_event(
                    event_writer,
                    &SpooledCustomEvent {
                        source_id: event.source_id,
                        session_id: event.session_id,
                        event_index: event.event_index,
                        event_id: event.event_id,
                        event_type: event.event_type.as_str().to_owned(),
                        role: event.role.map(|role| role.as_str().to_owned()),
                        occurred_at_unix_ms: event.occurred_at.timestamp_millis(),
                        body,
                    },
                )?;
                #[cfg(test)]
                record_custom_history_work(|work| {
                    work.resident_event_body_bytes = 0;
                });
                catalog.events.insert(
                    key,
                    CustomEventCatalogEntry {
                        line_number: line.line_number,
                        line,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::FileTouch(touch) => {
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &touch.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "session_id",
                &touch.session_id,
            );
            if touch.path.trim().is_empty() {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "file_touch path must not be empty".to_owned(),
                );
            }
            let key = (
                touch.source_id.clone(),
                touch.session_id.clone(),
                touch.touch_index,
            );
            if catalog.touch_keys.contains(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate touch_index for session".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    touch.source_id.len().saturating_mul(2),
                    touch.session_id.len().saturating_mul(2),
                ]))?;
                let path = if touch.event_index.is_some()
                    && touch.path.len() <= CUSTOM_DOCUMENT_METADATA_MAX_BYTES
                {
                    let byte_offset = catalog.touch_spool_bytes;
                    touch_writer.write_all(touch.path.as_bytes())?;
                    catalog.touch_spool_bytes = catalog
                        .touch_spool_bytes
                        .checked_add(touch.path.len() as u64)
                        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
                    Some(TouchSpoolRef {
                        byte_offset,
                        byte_length: touch.path.len(),
                    })
                } else {
                    None
                };
                catalog.touch_keys.insert(key);
                catalog.touches.push(TouchCandidate {
                    line_number: line.line_number,
                    byte_offset: line.byte_offset,
                    source_id: touch.source_id,
                    session_id: touch.session_id,
                    event_index: touch.event_index,
                    path,
                });
            }
        }
        CtxHistoryJsonlRecord::Edge(edge) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::EdgeIdBytes,
                edge.edge_id.as_deref(),
            )?;
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &edge.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "from_session_id",
                &edge.from_session_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
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
            if catalog.edge_keys.contains(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate edge record".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    edge.source_id.len().saturating_mul(2),
                    edge.from_session_id.len().saturating_mul(2),
                    edge.to_session_id.len().saturating_mul(2),
                    key.3.len(),
                ]))?;
                catalog.edge_keys.insert(key);
                catalog.edges.push(EdgeCandidate {
                    line_number: line.line_number,
                    source_id: edge.source_id,
                    from_session_id: edge.from_session_id,
                    to_session_id: edge.to_session_id,
                    edge_type: edge.edge_type,
                });
            }
        }
    }
    Ok(())
}

fn retained_metadata_bytes(lengths: &[usize]) -> usize {
    lengths.iter().fold(
        CUSTOM_HISTORY_CATALOG_ENTRY_OVERHEAD_BYTES,
        |total, length| total.saturating_add(*length),
    )
}

fn ensure_retained_key_bound(
    limit: CustomHistorySourceBackedBound,
    value: Option<&str>,
) -> CustomHistorySourceBackedResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES {
        return Err(CustomHistorySourceBackedError::Bounds {
            limit,
            maximum: CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES,
            observed: value.len(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_projection(
    mut catalog: ProjectionCatalog,
    event_spool: File,
    touch_spool: File,
    certified_prefix_bytes: u64,
    complete_records: usize,
    terminal: bool,
    content_digest: [u8; 32],
    observed_prior_prefix_digest: Option<[u8; 32]>,
    prior_prefix_bytes: Option<u64>,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    if catalog.manifest_line.is_none() {
        catalog.manifest_failure = Some((
            ProviderSourceFailureKind::InvalidSource,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        ));
    }
    if let Some((kind, detail)) = catalog.manifest_failure {
        return Err(CustomHistorySourceBackedError::StructuralManifest { kind, detail });
    }
    catalog.touch_keys.clear();
    catalog.edge_keys.clear();

    let mut session_roots;
    let mut event_touches = BTreeMap::new();
    let mut appended_touch_changes_prior_document = false;
    {
        let resolution =
            session_catalog(&catalog.sources, &catalog.sessions, &mut catalog.summary)?;
        catalog
            .sessions
            .retain(|key, _| resolution.valid.contains(key));
        session_roots = resolution.roots;

        let mut invalid_events = Vec::new();
        catalog.events.retain(|key, event| {
            let valid = catalog
                .sessions
                .contains_key(&(key.0.clone(), key.1.clone()));
            if !valid {
                invalid_events.push((
                    event.line_number,
                    format!(
                        "event references unknown session `{}` in source `{}`",
                        key.1, key.0
                    ),
                ));
            }
            valid
        });
        for (line_number, error) in invalid_events {
            push_provider_import_failure(&mut catalog.summary, line_number, error);
        }

        let mut valid_touches = Vec::with_capacity(catalog.touches.len());
        for touch in catalog.touches.drain(..) {
            let session_key = (touch.source_id.clone(), touch.session_id.clone());
            let error = if !catalog.sessions.contains_key(&session_key) {
                Some(format!(
                    "file_touch references unknown session `{}` in source `{}`",
                    touch.session_id, touch.source_id
                ))
            } else if let Some(event_index) = touch.event_index {
                let event_key = (
                    touch.source_id.clone(),
                    touch.session_id.clone(),
                    event_index,
                );
                (!catalog.events.contains_key(&event_key))
                    .then(|| format!("file_touch references unknown event_index `{event_index}`"))
            } else {
                None
            };
            if let Some(error) = error {
                push_provider_import_failure(&mut catalog.summary, touch.line_number, error);
            } else {
                valid_touches.push(touch);
            }
        }

        let mut valid_edges = Vec::with_capacity(catalog.edges.len());
        for edge in catalog.edges.drain(..) {
            let from_key = (edge.source_id.clone(), edge.from_session_id.clone());
            let to_key = (edge.source_id.clone(), edge.to_session_id.clone());
            let error = if !catalog.sessions.contains_key(&from_key) {
                Some(format!(
                    "edge references unknown from_session_id `{}`",
                    edge.from_session_id
                ))
            } else if !catalog.sessions.contains_key(&to_key) {
                Some(format!(
                    "edge references unknown to_session_id `{}`",
                    edge.to_session_id
                ))
            } else if edge.edge_type == SessionEdgeType::ParentChild {
                catalog.sessions.get(&to_key).and_then(|child| {
                    child.parent_session_id.as_ref().and_then(|parent| {
                        (parent != &edge.from_session_id).then(|| {
                            format!(
                                "parent_child edge from_session_id `{}` conflicts with session parent_session_id `{parent}`",
                                edge.from_session_id
                            )
                        })
                    })
                })
            } else {
                None
            };
            if let Some(error) = error {
                push_provider_import_failure(&mut catalog.summary, edge.line_number, error);
            } else {
                valid_edges.push(edge);
            }
        }

        let mut required = catalog
            .events
            .keys()
            .map(|key| (key.0.clone(), key.1.clone()))
            .chain(
                valid_touches
                    .iter()
                    .map(|touch| (touch.source_id.clone(), touch.session_id.clone())),
            )
            .chain(valid_edges.iter().flat_map(|edge| {
                [
                    (edge.source_id.clone(), edge.from_session_id.clone()),
                    (edge.source_id.clone(), edge.to_session_id.clone()),
                ]
            }))
            .collect::<BTreeSet<_>>();
        let mut pending = required.iter().cloned().collect::<Vec<_>>();
        while let Some(key) = pending.pop() {
            let Some(session) = catalog.sessions.get(&key) else {
                continue;
            };
            for dependency in [
                session.parent_session_id.as_ref(),
                session.root_session_id.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                let dependency_key = (session.source_id.clone(), dependency.clone());
                if required.insert(dependency_key.clone()) {
                    pending.push(dependency_key);
                }
            }
        }
        catalog.sessions.retain(|key, _| required.contains(key));
        session_roots.retain(|key, _| required.contains(key));

        let mut touch_bytes = BTreeMap::<CustomEventKey, usize>::new();
        for touch in valid_touches {
            let Some(event_index) = touch.event_index else {
                continue;
            };
            let key = (
                touch.source_id.clone(),
                touch.session_id.clone(),
                event_index,
            );
            if let (Some(prior_prefix_bytes), Some(event)) =
                (prior_prefix_bytes, catalog.events.get(&key))
            {
                if touch.byte_offset >= prior_prefix_bytes
                    && event.line.byte_offset < prior_prefix_bytes
                {
                    appended_touch_changes_prior_document = true;
                }
            }
            let Some(path) = touch.path else {
                continue;
            };
            let paths = event_touches.entry(key.clone()).or_insert_with(Vec::new);
            let retained_bytes = touch_bytes.entry(key).or_default();
            if paths.len() == CUSTOM_DOCUMENT_MAX_TOUCHED_FILES
                || retained_bytes.saturating_add(path.byte_length)
                    > CUSTOM_DOCUMENT_METADATA_MAX_BYTES
            {
                continue;
            }
            *retained_bytes = retained_bytes.saturating_add(path.byte_length);
            paths.push(path);
        }
    }

    let mut rejected_lines = catalog
        .summary
        .failures
        .iter()
        .filter_map(|failure| (failure.line != 0).then_some(failure.line))
        .collect::<BTreeSet<_>>();
    rejected_lines.extend(catalog.oversized_lines);
    let retained_lines = catalog
        .events
        .values()
        .map(|event| event.line_number)
        .collect::<BTreeSet<_>>();
    let complete_records = u64::try_from(complete_records)
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records = u64::try_from(catalog.events.len())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records_before_prior_prefix = prior_prefix_bytes
        .map(|prior_prefix_bytes| {
            u64::try_from(
                catalog
                    .events
                    .values()
                    .filter(|event| event.line.byte_offset < prior_prefix_bytes)
                    .count(),
            )
            .map_err(|_| CustomHistorySourceBackedError::CountMismatch)
        })
        .transpose()?;
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.retained_events_before_prior_prefix = retained_records_before_prior_prefix
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0);
    });
    let rejected_records = u64::try_from(
        rejected_lines
            .iter()
            .filter(|line| **line <= complete_records as usize && !retained_lines.contains(*line))
            .count(),
    )
    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let ignored_records = complete_records
        .checked_sub(retained_records)
        .and_then(|value| value.checked_sub(rejected_records))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes: certified_prefix_bytes,
    };
    Ok(ParsedProjection {
        sources: catalog.sources,
        sessions: catalog.sessions,
        session_roots,
        events: catalog.events,
        event_touches,
        event_spool,
        touch_spool,
        observed_prior_prefix_digest,
        retained_records_before_prior_prefix,
        appended_touch_changes_prior_document,
        counts,
        checkpoint: CustomHistoryCheckpoint {
            version: CUSTOM_CHECKPOINT_VERSION,
            certified_prefix_bytes,
            complete_records,
            terminal,
        },
        content_digest,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionVisit {
    Visiting,
    Valid,
    Invalid,
}

struct SessionFrame {
    key: CustomSessionKey,
    next_dependency: usize,
    valid: bool,
}

struct SessionResolution {
    valid: BTreeSet<CustomSessionKey>,
    roots: BTreeMap<CustomSessionKey, String>,
}

fn session_catalog(
    sources: &BTreeMap<String, CustomSourceCatalogEntry>,
    sessions: &BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    summary: &mut ProviderImportSummary,
) -> CustomHistorySourceBackedResult<SessionResolution> {
    let mut visits = HashMap::<CustomSessionKey, SessionVisit>::with_capacity(sessions.len());
    for start in sessions.keys() {
        if visits.contains_key(start) {
            continue;
        }
        visits.insert(start.clone(), SessionVisit::Visiting);
        #[cfg(test)]
        record_custom_history_work(|work| {
            work.session_nodes = work.session_nodes.saturating_add(1);
        });
        let mut stack = vec![SessionFrame {
            key: start.clone(),
            next_dependency: 0,
            valid: sources.contains_key(&start.0),
        }];
        while let Some(frame) = stack.last_mut() {
            let session = sessions
                .get(&frame.key)
                .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
            let dependency = match frame.next_dependency {
                0 => session.parent_session_id.as_ref(),
                1 => session.root_session_id.as_ref(),
                _ => None,
            };
            if frame.next_dependency < 2 {
                frame.next_dependency = frame.next_dependency.saturating_add(1);
                let Some(dependency) = dependency else {
                    continue;
                };
                if dependency == &session.session_id {
                    continue;
                }
                #[cfg(test)]
                record_custom_history_work(|work| {
                    work.session_dependencies = work.session_dependencies.saturating_add(1);
                });
                let dependency_key = (session.source_id.clone(), dependency.clone());
                match visits.get(&dependency_key).copied() {
                    Some(SessionVisit::Valid) => {}
                    Some(SessionVisit::Invalid | SessionVisit::Visiting) => {
                        frame.valid = false;
                    }
                    None if !sessions.contains_key(&dependency_key) => {
                        frame.valid = false;
                    }
                    None => {
                        visits.insert(dependency_key.clone(), SessionVisit::Visiting);
                        #[cfg(test)]
                        record_custom_history_work(|work| {
                            work.session_nodes = work.session_nodes.saturating_add(1);
                        });
                        stack.push(SessionFrame {
                            key: dependency_key.clone(),
                            next_dependency: 0,
                            valid: sources.contains_key(&dependency_key.0),
                        });
                    }
                }
                continue;
            }
            let completed = stack
                .pop()
                .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
            let state = if completed.valid {
                SessionVisit::Valid
            } else {
                SessionVisit::Invalid
            };
            visits.insert(completed.key, state);
            if state == SessionVisit::Invalid {
                if let Some(parent) = stack.last_mut() {
                    parent.valid = false;
                }
            }
        }
    }

    let mut valid = BTreeSet::new();
    for key in sessions.keys() {
        if visits.get(key) == Some(&SessionVisit::Valid) {
            valid.insert(key.clone());
            continue;
        }
        let line = sessions
            .get(key)
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?
            .line_number;
        push_provider_import_failure(
            summary,
            line,
            format!(
                "session `{}` in source `{}` has an invalid or cyclic source/parent/root relationship",
                key.1, key.0
            ),
        );
    }

    let mut roots = BTreeMap::<CustomSessionKey, String>::new();
    for start in &valid {
        if roots.contains_key(start) {
            continue;
        }
        let mut chain = Vec::<CustomSessionKey>::new();
        let mut current = start.clone();
        let root = loop {
            if let Some(root) = roots.get(&current) {
                break root.clone();
            }
            #[cfg(test)]
            record_custom_history_work(|work| {
                work.session_root_nodes = work.session_root_nodes.saturating_add(1);
            });
            let session = sessions
                .get(&current)
                .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
            chain.push(current.clone());
            if let Some(root) = &session.root_session_id {
                break root.clone();
            }
            let Some(parent) = &session.parent_session_id else {
                break session.session_id.clone();
            };
            if parent == &session.session_id {
                break session.session_id.clone();
            }
            current = (session.source_id.clone(), parent.clone());
            if !valid.contains(&current) {
                return Err(CustomHistorySourceBackedError::CountMismatch);
            }
        };
        for key in chain {
            roots.insert(key, root.clone());
        }
    }
    Ok(SessionResolution { valid, roots })
}

fn new_prefix_hasher() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest
}

fn finish_prefix_digest(hasher: &Sha256, prefix_bytes: u64) -> [u8; 32] {
    let mut digest = hasher.clone();
    digest.update(prefix_bytes.to_be_bytes());
    digest.finalize().into()
}

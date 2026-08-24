use super::*;
use crate::provider::custom_history_jsonl::CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES;

#[derive(Debug)]
struct FileReferenceCandidate {
    line_number: usize,
    source_id: String,
    provider_session_id: String,
    event_index: u64,
    value: String,
}

#[derive(Debug)]
struct EdgeCandidate {
    line_number: usize,
    source_id: String,
    from_provider_session_id: String,
    to_provider_session_id: String,
    relationship: Option<ProviderNativeSessionRelationship>,
}

#[derive(Debug)]
struct ProjectionCatalog {
    summary: ProviderImportSummary,
    manifest_line: Option<usize>,
    manifest_failure: Option<(ProviderSourceFailureKind, String)>,
    lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    sources: BTreeMap<String, CustomSourceCatalogEntry>,
    sessions: BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    events: BTreeMap<CustomEventKey, CustomEventCatalogEntry>,
    reference_keys: BTreeSet<(String, String, u64)>,
    file_references: Vec<FileReferenceCandidate>,
    edge_keys: BTreeSet<(String, String, String, String)>,
    edges: Vec<EdgeCandidate>,
    oversized_lines: BTreeSet<usize>,
    budget: CatalogBudget,
}

impl ProjectionCatalog {
    fn new(limits: CustomHistoryCatalogLimits) -> Self {
        Self {
            summary: ProviderImportSummary::default(),
            manifest_line: None,
            manifest_failure: None,
            lineage_contract: None,
            sources: BTreeMap::new(),
            sessions: BTreeMap::new(),
            events: BTreeMap::new(),
            reference_keys: BTreeSet::new(),
            file_references: Vec::new(),
            edge_keys: BTreeSet::new(),
            edges: Vec::new(),
            oversized_lines: BTreeSet::new(),
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
    let mut event_spool = tempfile::tempfile()?;
    let mut catalog = ProjectionCatalog::new(limits);
    let mut stream = JsonlPhysicalStream::open(
        source.file().try_clone()?,
        frozen_length,
        0,
        0,
        JsonlRecordFraming::new(MAX_PROVIDER_JSONL_LINE_BYTES.saturating_sub(1), false),
        JsonlPhysicalDigest::complete_and_bounded_prefix(
            new_complete_prefix_hasher(),
            new_prefix_hasher(),
            prior_prefix_bytes.unwrap_or(0),
        ),
        || CaptureError::SourceChangedDuringCapture,
    )?;

    {
        let mut event_writer = BufWriter::new(&mut event_spool);
        while let Some(record) = stream.next_record()? {
            #[cfg(test)]
            record_custom_history_work(|work| {
                work.peak_provider_record_bytes =
                    work.peak_provider_record_bytes.max(record.stored_len);
            });
            if !record.complete {
                break;
            }

            let line_number = usize::try_from(record.physical_ordinal.saturating_add(1))
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
            let evidence = CompleteLine {
                line_number,
                byte_offset: record.byte_start,
            };
            catalog.budget.admit_record()?;
            if record.oversized {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line_number,
                    "custom history JSONL line exceeds the bounded record limit".to_owned(),
                );
                catalog.oversized_lines.insert(line_number);
            } else {
                visit_record(
                    stream.record_bytes(record),
                    evidence,
                    &mut catalog,
                    &mut event_writer,
                )?;
            }
        }
        event_writer.flush()?;
    }

    event_spool.seek(SeekFrom::Start(0))?;
    let terminal = stream.terminal();
    let certified_prefix_bytes = stream.complete_prefix_end();
    let complete_records = usize::try_from(stream.next_physical_ordinal())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let source_hasher = stream.digest().complete_hasher();
    let content_digest = finish_complete_prefix_digest(source_hasher, certified_prefix_bytes);
    let (prior_hasher, prior_remaining) = stream
        .digest()
        .bounded_prefix()
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let observed_prior_prefix_digest = match prior_prefix_bytes {
        Some(expected) if prior_remaining == 0 => {
            Some(finish_prefix_digest(prior_hasher, expected))
        }
        _ => None,
    };
    finish_projection(
        catalog,
        event_spool,
        certified_prefix_bytes,
        complete_records,
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
) -> CustomHistorySourceBackedResult<()> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.provider_records_parsed = work.provider_records_parsed.saturating_add(1);
    });
    let record = match parse_record(bytes) {
        Ok(record) => record,
        Err(error) => {
            push_provider_import_failure(&mut catalog.summary, line.line_number, error);
            return Ok(());
        }
    };
    match record {
        CtxHistoryJsonlRecord::Manifest(manifest) => {
            if manifest.schema_version != CUSTOM_HISTORY_PUBLIC_SCHEMA_VERSION {
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
            catalog.lineage_contract = manifest.lineage_contract;
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
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    source.source_id.len(),
                    source.provider_key.len(),
                ]))?;
                catalog.sources.insert(
                    source.source_id.clone(),
                    CustomSourceCatalogEntry {
                        provider_key: source.provider_key,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::Session(session) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::ProviderSessionIdBytes,
                Some(&session.provider_session_id),
            )?;
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::ParentProviderSessionIdBytes,
                session.parent_provider_session_id.as_deref(),
            )?;
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::RootProviderSessionIdBytes,
                session.root_provider_session_id.as_deref(),
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
                "provider_session_id",
                &session.provider_session_id,
            );
            for (field, value) in [
                (
                    "parent_provider_session_id",
                    session.parent_provider_session_id.as_deref(),
                ),
                (
                    "root_provider_session_id",
                    session.root_provider_session_id.as_deref(),
                ),
            ] {
                if let Some(value) = value {
                    validate_custom_history_identifier(
                        &mut catalog.summary,
                        line.line_number,
                        field,
                        value,
                    );
                }
            }
            let key = (
                session.source_id.clone(),
                session.provider_session_id.clone(),
            );
            if catalog.sessions.contains_key(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate provider_session_id for source".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                let agent_scope = session.agent_scope;
                let cwd = session.cwd;
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    session.source_id.len().saturating_mul(2),
                    session.provider_session_id.len().saturating_mul(2),
                    session
                        .parent_provider_session_id
                        .as_ref()
                        .map_or(0, String::len),
                    session
                        .root_provider_session_id
                        .as_ref()
                        .map_or(0, String::len),
                    agent_scope.map_or(0, |scope| scope.as_str().len()),
                    cwd.as_ref().map_or(0, String::len),
                ]))?;
                catalog.sessions.insert(
                    key,
                    CustomSessionCatalogEntry {
                        line_number: line.line_number,
                        provider_session_id: session.provider_session_id,
                        parent_provider_session_id: session.parent_provider_session_id,
                        root_provider_session_id: session.root_provider_session_id,
                        session_relationship: session.session_relationship,
                        agent_scope,
                        cwd,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::Event(event) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::EventIdBytes,
                event.event_id.as_deref(),
            )?;
            if let Some(copied_from) = &event.copied_from {
                ensure_retained_key_bound(
                    CustomHistorySourceBackedBound::ProviderSessionIdBytes,
                    Some(&copied_from.ancestor_provider_session_id),
                )?;
                ensure_retained_key_bound(
                    CustomHistorySourceBackedBound::EventIdBytes,
                    Some(&copied_from.ancestor_event_id),
                )?;
            }
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
                "provider_session_id",
                &event.provider_session_id,
            );
            if let Some(event_id) = event.event_id.as_deref() {
                validate_custom_history_identifier(
                    &mut catalog.summary,
                    line.line_number,
                    "event_id",
                    event_id,
                );
            }
            if let Some(copied_from) = event.copied_from.as_ref() {
                validate_custom_history_identifier(
                    &mut catalog.summary,
                    line.line_number,
                    "ancestor_provider_session_id",
                    &copied_from.ancestor_provider_session_id,
                );
                validate_custom_history_identifier(
                    &mut catalog.summary,
                    line.line_number,
                    "ancestor_event_id",
                    &copied_from.ancestor_event_id,
                );
            }
            let key = (
                event.source_id.clone(),
                event.provider_session_id.clone(),
                event.event_index,
            );
            if catalog.events.contains_key(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate event_index for provider session".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                let event_id = event.event_id.clone();
                let copied_from = event.copied_from.clone();
                let body = lexical_body(&event);
                let activity = match event.payload.get("activity") {
                    Some(activity) => {
                        match serde_json::from_value::<CoreActivity>(activity.clone()) {
                            Ok(activity) => {
                                if let Err(error) = validate_unmerged_activity(&activity, &body) {
                                    push_provider_import_failure(
                                        &mut catalog.summary,
                                        line.line_number,
                                        error.to_owned(),
                                    );
                                    return Ok(());
                                }
                                Some(activity)
                            }
                            Err(_) => {
                                push_provider_import_failure(
                                    &mut catalog.summary,
                                    line.line_number,
                                    "neutral activity JSON does not match the Core contract"
                                        .to_owned(),
                                );
                                return Ok(());
                            }
                        }
                    }
                    None => None,
                };
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    event.source_id.len(),
                    event.provider_session_id.len(),
                    event.event_id.as_ref().map_or(0, String::len),
                    event.copied_from.as_ref().map_or(0, |selector| {
                        selector
                            .ancestor_provider_session_id
                            .len()
                            .saturating_add(selector.ancestor_event_id.len())
                    }),
                ]))?;
                let payload = event.payload;
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
                        provider_session_id: event.provider_session_id,
                        event_index: event.event_index,
                        event_id: event_id.clone(),
                        event_type: event.event_type.as_str().to_owned(),
                        role: event.role.map(|role| role.as_str().to_owned()),
                        occurred_at_unix_ms: event.occurred_at.timestamp_millis(),
                        body,
                        payload,
                        activity: activity.clone(),
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
                        event_id,
                        copied_from,
                        activity_fact_count: activity
                            .as_ref()
                            .map_or(0, |activity| activity.facts.len()),
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::FileReference(reference) => {
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &reference.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "provider_session_id",
                &reference.provider_session_id,
            );
            if reference.value.is_empty() {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "file_reference value must not be empty".to_owned(),
                );
            }
            let key = (
                reference.source_id.clone(),
                reference.provider_session_id.clone(),
                reference.reference_index,
            );
            if catalog.reference_keys.contains(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate reference_index for session".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    reference.source_id.len().saturating_mul(2),
                    reference.provider_session_id.len().saturating_mul(2),
                    reference.value.len(),
                ]))?;
                catalog.reference_keys.insert(key);
                catalog.file_references.push(FileReferenceCandidate {
                    line_number: line.line_number,
                    source_id: reference.source_id,
                    provider_session_id: reference.provider_session_id,
                    event_index: reference.event_index,
                    value: reference.value,
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
                "from_provider_session_id",
                &edge.from_provider_session_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "to_provider_session_id",
                &edge.to_provider_session_id,
            );
            if let Some(edge_id) = edge.edge_id.as_deref() {
                validate_custom_history_identifier(
                    &mut catalog.summary,
                    line.line_number,
                    "edge_id",
                    edge_id,
                );
            }
            let edge_key = edge.edge_id.clone().unwrap_or_else(|| {
                format!(
                    "{}:{}:{}",
                    edge.from_provider_session_id,
                    edge.to_provider_session_id,
                    edge.relationship
                        .map_or("none", |relationship| relationship.as_str())
                )
            });
            let key = (
                edge.source_id.clone(),
                edge.from_provider_session_id.clone(),
                edge.to_provider_session_id.clone(),
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
                    edge.from_provider_session_id.len().saturating_mul(2),
                    edge.to_provider_session_id.len().saturating_mul(2),
                    key.3.len(),
                ]))?;
                catalog.edge_keys.insert(key);
                catalog.edges.push(EdgeCandidate {
                    line_number: line.line_number,
                    source_id: edge.source_id,
                    from_provider_session_id: edge.from_provider_session_id,
                    to_provider_session_id: edge.to_provider_session_id,
                    relationship: edge.relationship,
                });
            }
        }
    }
    Ok(())
}

fn parse_record(bytes: &[u8]) -> Result<CtxHistoryJsonlRecord, String> {
    serde_json::from_slice(bytes).map_err(|error| error.to_string())
}

mod validation;

use validation::{
    ensure_retained_key_bound, finish_projection, retained_metadata_bytes,
    validate_unmerged_activity,
};

fn session_catalog(
    sources: &BTreeMap<String, CustomSourceCatalogEntry>,
    sessions: &BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    summary: &mut ProviderImportSummary,
) -> BTreeSet<CustomSessionKey> {
    let mut valid = BTreeSet::new();
    for (key, session) in sessions {
        #[cfg(test)]
        record_custom_history_work(|work| {
            work.session_nodes = work.session_nodes.saturating_add(1);
        });
        let source_exists = sources.contains_key(&key.0);
        let has_self_parent =
            session.parent_provider_session_id.as_deref() == Some(&session.provider_session_id);
        if source_exists && !has_self_parent {
            valid.insert(key.clone());
            continue;
        }
        let detail = if has_self_parent {
            "declares itself as its direct parent"
        } else {
            "references an unknown source"
        };
        push_provider_import_failure(
            summary,
            session.line_number,
            format!("session `{}` in source `{}` {detail}", key.1, key.0,),
        );
    }
    valid
}

fn new_prefix_hasher() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest
}

fn new_complete_prefix_hasher() -> JsonlResumableSha256 {
    let mut digest = JsonlResumableSha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest
}

fn finish_complete_prefix_digest(hasher: &JsonlResumableSha256, prefix_bytes: u64) -> [u8; 32] {
    let mut digest = hasher.clone();
    digest.update(&prefix_bytes.to_be_bytes());
    digest.digest()
}

fn finish_prefix_digest(hasher: &Sha256, prefix_bytes: u64) -> [u8; 32] {
    let mut digest = hasher.clone();
    digest.update(prefix_bytes.to_be_bytes());
    digest.finalize().into()
}

#[cfg(test)]
mod record_tests {
    use super::*;

    #[test]
    fn v2_session_requires_provider_session_id() {
        let error = parse_record(
            br#"{"record_type":"session","source_id":"source-a","started_at":"2026-07-28T12:00:00Z"}"#,
        )
        .unwrap_err();
        assert!(
            error.contains("missing field `provider_session_id`"),
            "{error}"
        );
    }

    #[test]
    fn v1_file_touch_has_no_compatibility_parser() {
        let error = parse_record(
            br#"{"record_type":"file_touch","source_id":"source-a","session_id":"child","touch_index":0,"occurred_at":"2026-07-28T12:00:02Z"}"#,
        )
        .unwrap_err();
        assert!(error.contains("unknown variant `file_touch`"), "{error}");
    }
}

//! Exhaustive Hermes inventory and incremental-admission validation.

use super::*;

struct HermesSessionObservationBuilder {
    session_row: Option<(i64, [u8; 32])>,
    message_count: u64,
    message_keys: Sha256,
}

impl HermesSessionObservationBuilder {
    fn new() -> Self {
        let mut message_keys = Sha256::new();
        message_keys.update(HERMES_MESSAGE_OBSERVATION_DOMAIN);
        Self {
            session_row: None,
            message_count: 0,
            message_keys,
        }
    }

    fn record_session(
        &mut self,
        rowid: i64,
        row: &HermesSessionRow,
    ) -> HermesSourceBackedResult<()> {
        if self
            .session_row
            .replace((rowid, session_record_digest(row)))
            .is_some()
        {
            return Err(HermesSourceBackedError::Capture(
                CaptureError::InvalidPayload(format!("Hermes session {} is duplicated", row.id)),
            ));
        }
        Ok(())
    }

    fn record_message(
        &mut self,
        rowid: i64,
        record_digest: [u8; 32],
    ) -> HermesSourceBackedResult<()> {
        self.message_count = checked_add(self.message_count, 1)?;
        self.message_keys.update(rowid.to_be_bytes());
        self.message_keys.update(record_digest);
        Ok(())
    }

    fn finish(
        self,
        source: &SourceKey,
        schema_evidence: &[u8],
    ) -> ([u8; 32], DocumentLeafFingerprint) {
        let mut revision = Sha256::new();
        revision.update(HERMES_SESSION_OBSERVATION_DOMAIN);
        hash_bytes(&mut revision, schema_evidence);
        revision.update(source.exact_descriptor_digest());
        match self.session_row {
            Some((rowid, digest)) => {
                revision.update([1]);
                revision.update(rowid.to_be_bytes());
                revision.update(digest);
            }
            None => revision.update([0]),
        }
        revision.update(self.message_count.to_be_bytes());
        revision.update(self.message_keys.finalize());
        let revision: [u8; 32] = revision.finalize().into();
        let mut fingerprint = Sha256::new();
        fingerprint.update(HERMES_LEAF_FINGERPRINT_DOMAIN);
        fingerprint.update(source.exact_descriptor_digest());
        fingerprint.update(revision);
        (
            revision,
            DocumentLeafFingerprint::new(fingerprint.finalize().into()),
        )
    }
}

#[cfg(test)]
pub(super) fn observe_hermes_session_inventory<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<HermesSessionInventory<L>>
where
    L::PinnedAppendBase: Clone,
{
    report_progress(hermes_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        0,
        0,
    ))?;
    let (schema, schema_evidence) = detect_hermes_schema(conn)?;
    observe_hermes_session_inventory_with_schema(
        candidate,
        conn,
        schema,
        schema_evidence,
        report_progress,
    )
}

pub(super) fn detect_hermes_schema(
    conn: &rusqlite::Connection,
) -> HermesSourceBackedResult<(HermesSchema, Vec<u8>)> {
    let sqlite_user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Schema))?;
    let schema_fingerprint = sqlite_schema_fingerprint(conn)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Schema))?;
    let schema = HermesSchema::detect(conn)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Schema))?;
    Ok((
        schema,
        hermes_schema_evidence(sqlite_user_version, &schema_fingerprint),
    ))
}

pub(super) fn hermes_incremental_requires_exhaustive(
    conn: &rusqlite::Connection,
    prior: &HermesRefreshReceipt,
    profile_source_descriptor: [u8; 32],
    database_identity: [u8; 32],
) -> HermesSourceBackedResult<bool> {
    let (_, schema_evidence) = detect_hermes_schema(conn)?;
    let schema_digest: [u8; 32] = Sha256::digest(&schema_evidence).into();
    let session_rowid = hermes_max_rowid(conn, "sessions")?;
    let message_rowid = hermes_max_rowid(conn, "messages")?;
    Ok(prior.kind != HERMES_ROUTE_CONTROL_KIND
        || prior.version != HERMES_ROUTE_CONTROL_VERSION
        || prior.parser_revision != HERMES_SOURCE_PARSER_REVISION
        || prior.outcome != "successful"
        || prior.profile_source_descriptor != profile_source_descriptor
        || prior.database_identity != database_identity
        || prior.schema_evidence != schema_digest
        || session_rowid < prior.session_rowid
        || message_rowid < prior.message_rowid)
}

pub(super) fn observe_hermes_session_inventory_with_schema<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    schema: HermesSchema,
    schema_evidence: Vec<u8>,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<HermesSessionInventory<L>>
where
    L::PinnedAppendBase: Clone,
{
    let mut builders = BTreeMap::<String, HermesSessionObservationBuilder>::new();
    let mut observed_rows = 0_u64;
    let mut session_reader = HermesRowReader::new(conn, &schema)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut after_session_rowid = None;
    loop {
        let page = session_reader
            .next_session_inventory_page(after_session_rowid)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        if page.is_empty() {
            break;
        }
        for native in page {
            after_session_rowid = Some(native.locator.rowid);
            observed_rows = checked_add(observed_rows, 1)?;
            match native.record {
                HermesNativeRecord::Session(row) => {
                    hermes_session_source_key(&candidate.source, &row.id)?;
                    builders
                        .entry(row.id.clone())
                        .or_insert_with(HermesSessionObservationBuilder::new)
                        .record_session(native.locator.rowid, &row)?;
                }
                HermesNativeRecord::Rejected(reason) => {
                    return Err(HermesSourceBackedError::Capture(
                        CaptureError::InvalidPayload(format!(
                    "Hermes cannot derive a logical source for rejected session row: {reason}")),
                    ))
                }
                HermesNativeRecord::Message { .. } => {
                    return Err(HermesSourceBackedError::Capture(
                        CaptureError::SystemInvariant(
                            "Hermes session inventory crossed into message rows",
                        ),
                    ))
                }
            }
        }
        report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }
    drop(session_reader);
    let mut message_spool = HermesExactMessageSpool::new()
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut message_ranges = BTreeMap::<String, HermesMessageSpoolRange>::new();
    let mut after_message_rowid = None;
    let mut message_reader = HermesRowReader::new(conn, &schema)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut next_message_ordinal = 0;
    loop {
        let page = message_reader
            .next_message_inventory_page(after_message_rowid, next_message_ordinal)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        if page.is_empty() {
            break;
        }
        let last = page.last().ok_or(CaptureError::SystemInvariant(
            "Hermes message inventory returned an empty nonterminal page",
        ))?;
        after_message_rowid = Some(last.locator.rowid);
        next_message_ordinal = last.next_frontier.next_ordinal;
        for native in page {
            observed_rows = checked_add(observed_rows, 1)?;
            let provider_session_id = match &native.record {
                HermesNativeRecord::Message { row, .. } => row.session_id.clone(),
                HermesNativeRecord::Rejected(_) => {
                    hermes_message_session_id(conn, native.locator.rowid)?
                }
                HermesNativeRecord::Session(_) => {
                    return Err(CaptureError::SystemInvariant(
                        "Hermes message inventory returned a session row",
                    )
                    .into())
                }
            };
            hermes_session_source_key(&candidate.source, &provider_session_id)?;
            message_spool
                .push(
                    message_ranges
                        .entry(provider_session_id.clone())
                        .or_insert_with(HermesMessageSpoolRange::empty),
                    native.locator.rowid,
                )
                .map_err(HermesSourceBackedError::from)?;
            builders
                .entry(provider_session_id)
                .or_insert_with(HermesSessionObservationBuilder::new)
                .record_message(native.locator.rowid, native_record_digest(&native)?)?;
        }
        report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }
    let mut leaves = Vec::with_capacity(builders.len());
    for (provider_session_id, builder) in builders {
        let source = hermes_session_source_key(&candidate.source, &provider_session_id)?;
        let (observation_revision, fingerprint) = builder.finish(&source, &schema_evidence);
        let exact_message_range = message_ranges.get(&provider_session_id).copied();
        leaves.push(ObservedDocumentLeaf::new(
            fingerprint,
            HermesSessionLeaf {
                provider_session_id,
                source,
                observation_revision: observation_revision.to_vec(),
                incremental: None,
                exact_message_range,
            },
        ));
    }
    let tree_fingerprint = hermes_tree_fingerprint(&candidate.source, &schema_evidence, &leaves);
    record_inventory_rows(observed_rows);
    report_progress(hermes_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        observed_rows,
        0,
    ))?;
    Ok(HermesSessionInventory {
        schema,
        schema_evidence,
        leaves,
        tree_fingerprint,
        max_session_rowid: after_session_rowid.unwrap_or(0),
        max_message_rowid: after_message_rowid.unwrap_or(0),
        reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
        publication_receipt: None,
        message_spool: Some(message_spool),
    })
}

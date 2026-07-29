use super::*;

pub(crate) fn materialize(
    data_root: &Path,
    telemetry: &mut ProMaterializationTelemetryV1,
) -> Result<MaterializeReport> {
    let result = retry_materialization_once(|| materialize_once(data_root, telemetry))
        .and_then(|report| super::super::pending_materialization::clear_after(data_root, report));
    if let Err(error) = &result {
        telemetry.fail(stable_error_code(error));
    }
    result
}

pub(super) fn retry_materialization_once<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    // Give a live private rebuild owner one bounded helper-session interval to
    // publish or drop without turning concurrent activation into a retry loop.
    match operation() {
        Err(error) if stable_error_code(&error) == Some("not_materialized") => operation(),
        result => result,
    }
}

pub(super) fn materialize_once(
    data_root: &Path,
    telemetry: &mut ProMaterializationTelemetryV1,
) -> Result<MaterializeReport> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        bail!(
            "source_unavailable: ctx Store is not initialized at {}; run ctx setup or ctx import first",
            db_path.display()
        );
    }
    crate::commands::import::prepare_core_for_pro_materialization(data_root)
        .context("source_unavailable: settle canonical history before Pro materialization")?;
    let store = Store::open(&db_path).with_context(|| {
        format!(
            "source_unavailable: open canonical Store {}",
            db_path.display()
        )
    })?;
    store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .context("source_unavailable: activate canonical projection journal")?;
    let required = nativepath_pro_capabilities();
    let mut client = match ProClient::connect(data_root, &required) {
        Ok(client) => client,
        Err(error) => {
            telemetry.helper_connection = pro_helper_connection_outcome(stable_error_code(&error));
            return Err(error);
        }
    };
    telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
    let helper_status = helper_status(&mut client)?;
    let helper_state = helper_status.state;
    let helper_checkpoint = helper_status.checkpoint;
    let helper_store_checkpoint = helper_checkpoint.as_ref().map(store_checkpoint);
    let active = store
        .reconcile_projection_journal(helper_store_checkpoint.as_ref())
        .context("source_unavailable: reconcile canonical projection journal")?;
    let checkpoint_compatible = helper_checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint.contract_fingerprint == PROTOCOL_FINGERPRINT
            && checkpoint.position.generation == active.position.generation
            && checkpoint.position.sequence <= active.position.sequence
    });
    telemetry.mode = Some(ProMaterializationModeV1::from_graph_state(
        helper_state,
        checkpoint_compatible,
        helper_checkpoint
            .as_ref()
            .map_or(0, |checkpoint| checkpoint.position.sequence),
    ));
    let prior = helper_checkpoint
        .filter(|checkpoint| {
            checkpoint.contract_fingerprint == PROTOCOL_FINGERPRINT
                && checkpoint.position.generation == active.position.generation
                && checkpoint.position.sequence <= active.position.sequence
        })
        .unwrap_or_else(|| JournalCheckpoint {
            position: JournalPosition {
                generation: active.position.generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial_journal_digest(active.position.generation),
        });
    let target = protocol_checkpoint(active);
    let sync = sync_projection_journal_pages_through(
        &store,
        helper_state,
        checkpoint_compatible,
        prior,
        target,
        &mut |message, timeout| client.exchange(message, timeout),
    )?;
    if sync.full_baseline_from_ready {
        telemetry.mode = Some(ProMaterializationModeV1::Full);
    }
    let prior = sync.committed_through;
    let output =
        ProOutputImport::begin_with_client(client, Some((data_root.to_path_buf(), prior.clone())))?;
    drop(store);
    crate::commands::import::catch_up_pro_outputs(data_root, output)
        .context("not_materialized: catch up provider output materialization")?;
    telemetry.complete(
        sync.batches,
        sync.observations,
        sync.accepted_observations,
        sync.replayed_batches,
        sync.initial_lag,
    );
    Ok(MaterializeReport {
        schema_version: 1,
        payload_type: "pro_materialization",
        frontier: prior.position.sequence,
        batches: sync.batches,
        observations: sync.observations,
        replayed_batches: sync.replayed_batches,
    })
}

#[derive(Debug)]
pub(super) struct JournalPageSyncReport {
    pub(super) committed_through: JournalCheckpoint,
    pub(super) batches: u64,
    pub(super) observations: u64,
    pub(super) accepted_observations: u64,
    pub(super) replayed_batches: u64,
    pub(super) initial_lag: u64,
    pub(super) full_baseline_from_ready: bool,
}

pub(super) fn sync_projection_journal_pages_through(
    store: &Store,
    helper_state: GraphState,
    checkpoint_compatible: bool,
    mut prior: JournalCheckpoint,
    target: JournalCheckpoint,
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<JournalPageSyncReport> {
    let prior_store = store_checkpoint(&prior);
    let target_store = store_checkpoint(&target);
    validate_retained_journal_checkpoint(store, &prior_store)?;
    validate_retained_journal_checkpoint(store, &target_store)?;
    if prior.position.generation != target.position.generation
        || prior.position.sequence > target.position.sequence
    {
        bail!("corrupt: helper journal checkpoint is not a prefix of the Store target");
    }

    let initial_lag = target
        .position
        .sequence
        .saturating_sub(prior.position.sequence);
    let mut batches = 0_u64;
    let mut observations = 0_u64;
    let mut accepted_observations = 0_u64;
    let mut replayed_batches = 0_u64;
    let mut full_baseline_from_ready = false;
    let mut authorized_repository_roots: Option<Vec<String>> = None;
    loop {
        let snapshot = coalesced_journal_snapshot_through(
            store,
            store_position(prior.position),
            &target_store,
        )?;
        if !journal_sync_required(
            helper_state,
            checkpoint_compatible,
            &prior,
            &target,
            batches,
        ) {
            break;
        }
        let roots = authorized_repository_roots
            .get_or_insert_with(|| snapshot.authorized_repository_roots.clone())
            .clone();
        let records = snapshot
            .records
            .into_iter()
            .map(protocol_journal_record)
            .collect::<Vec<_>>();
        let context = JournalContextWindow {
            base_checkpoint: protocol_checkpoint(snapshot.context.base_checkpoint),
            records: snapshot
                .context
                .records
                .into_iter()
                .map(protocol_journal_record)
                .collect(),
        };
        if batches == 0
            && prior.position.sequence == 0
            && !records.is_empty()
            && helper_state == GraphState::Ready
        {
            full_baseline_from_ready = true;
        }
        let mut request = fit_journal_sync_request(JournalSyncRequest {
            mode: if prior.position.sequence == 0 {
                JournalSyncMode::FullBaseline
            } else {
                JournalSyncMode::Incremental
            },
            canonical_schema_version: snapshot.canonical_schema_version,
            canonical_schema_identity: snapshot.canonical_schema_identity,
            projection_contract_version: snapshot.projection_contract_version,
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            prior_checkpoint: prior.clone(),
            context,
            frozen_through: target.clone(),
            authorized_repository_roots: roots,
            records,
        })?;
        let (result, expected_records) = loop {
            let expected_records = u32::try_from(request.records.len())
                .map_err(|_| anyhow!("invalid_request: journal page is too large"))?;
            request.validate().map_err(|error| {
                anyhow!(
                    "corrupt: invalid canonical projection journal page: {}",
                    error.message
                )
            })?;
            let expected_checkpoint = request.committed_checkpoint();
            let frozen_checkpoint = request.frozen_through.clone();
            let response = exchange(HostMessage::SyncJournal(request.clone()), BATCH_TIMEOUT)?;
            match response {
                HelperMessage::JournalSynced(result) => {
                    validate_journal_ack(
                        &result,
                        &expected_checkpoint,
                        &frozen_checkpoint,
                        expected_records,
                    )?;
                    break (result, expected_records);
                }
                HelperMessage::Error(error)
                    if error.class == ctx_pro_host_protocol::ErrorClass::Bounds
                        && request.records.len() > 1 =>
                {
                    request.records.truncate(request.records.len().div_ceil(2));
                }
                HelperMessage::Error(error) => return Err(protocol_error(error)),
                _ => bail!("invalid_response: helper returned a non-journal response"),
            }
        };
        let acknowledged = store_checkpoint(&result.committed_through);
        if acknowledged.position.sequence > target_store.position.sequence {
            bail!("invalid_response: helper acknowledged beyond the requested Store target");
        }
        store
            .acknowledge_projection_journal(&acknowledged)
            .context("source_unavailable: publish canonical journal acknowledgement")?;
        batches = batches.saturating_add(1);
        observations = observations.saturating_add(u64::from(expected_records));
        accepted_observations =
            accepted_observations.saturating_add(u64::from(result.accepted_records));
        replayed_batches = replayed_batches.saturating_add(u64::from(result.replayed));
        prior = result.committed_through;
        if result.frozen_complete && !snapshot.has_more {
            break;
        }
    }
    if prior != target {
        bail!("invalid_response: helper did not reach the exact requested Store target");
    }
    Ok(JournalPageSyncReport {
        committed_through: prior,
        batches,
        observations,
        accepted_observations,
        replayed_batches,
        initial_lag,
        full_baseline_from_ready,
    })
}

pub(super) fn validate_retained_journal_checkpoint(
    store: &Store,
    checkpoint: &StoreJournalCheckpoint,
) -> Result<()> {
    protocol_checkpoint(checkpoint.clone())
        .validate()
        .map_err(|error| {
            anyhow!(
                "corrupt: invalid Store journal checkpoint: {}",
                error.message
            )
        })?;
    if store
        .verify_projection_journal_checkpoint(checkpoint)
        .context("source_unavailable: validate retained canonical projection journal")?
    {
        return Ok(());
    }
    bail!("corrupt: Store journal checkpoint digest is not present in the retained suffix")
}

pub(super) fn nativepath_pro_capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
    ])
}

pub(super) fn prepare_nativepath_projection_journal(
    store: &Store,
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<JournalCheckpoint> {
    store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .context("source_unavailable: activate canonical projection journal")?;
    let status = helper_status_with(exchange)?;
    let helper_checkpoint = status.checkpoint;
    let helper_store_checkpoint = helper_checkpoint.as_ref().map(store_checkpoint);
    let active = store
        .reconcile_projection_journal(helper_store_checkpoint.as_ref())
        .context("source_unavailable: reconcile canonical projection journal")?;
    let checkpoint_compatible = helper_checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint.contract_fingerprint == PROTOCOL_FINGERPRINT
            && checkpoint.position.generation == active.position.generation
            && checkpoint.position.sequence <= active.position.sequence
    });
    let prior = helper_checkpoint
        .filter(|checkpoint| {
            checkpoint.contract_fingerprint == PROTOCOL_FINGERPRINT
                && checkpoint.position.generation == active.position.generation
                && checkpoint.position.sequence <= active.position.sequence
        })
        .unwrap_or_else(|| JournalCheckpoint {
            position: JournalPosition {
                generation: active.position.generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial_journal_digest(active.position.generation),
        });
    let target = protocol_checkpoint(active);
    sync_projection_journal_pages_through(
        store,
        status.state,
        checkpoint_compatible,
        prior,
        target,
        exchange,
    )
    .map(|report| report.committed_through)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeProAdvanceDisposition {
    Advanced,
    AlreadyAdvanced,
}

pub(super) fn sync_nativepath_group_through(
    store: &Store,
    target: &StoreJournalCheckpoint,
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<NativeProAdvanceDisposition> {
    validate_retained_journal_checkpoint(store, target)?;
    let status = helper_status_with(exchange)?;
    let target = protocol_checkpoint(target.clone());
    let checkpoint_compatible = status.checkpoint.is_some();
    let prior = match status.checkpoint {
        Some(checkpoint) => {
            let helper_store_checkpoint = store_checkpoint(&checkpoint);
            if checkpoint != target {
                validate_retained_journal_checkpoint(store, &helper_store_checkpoint)?;
            }
            if checkpoint.contract_fingerprint != target.contract_fingerprint
                || checkpoint.position.generation != target.position.generation
            {
                bail!("corrupt: helper journal checkpoint is outside the Store target chain");
            }
            if checkpoint.position.sequence >= target.position.sequence {
                if status.state != GraphState::Ready {
                    if checkpoint.position.sequence > target.position.sequence {
                        bail!(
                            "not_materialized: non-ready helper advanced beyond the Store receipt target"
                        );
                    }
                } else {
                    store
                        .acknowledge_projection_journal(&store_checkpoint(&target))
                        .context(
                            "source_unavailable: publish exact canonical journal acknowledgement",
                        )?;
                    return Ok(NativeProAdvanceDisposition::AlreadyAdvanced);
                }
            }
            checkpoint
        }
        None => {
            let genesis = JournalCheckpoint {
                position: JournalPosition {
                    generation: target.position.generation,
                    sequence: 0,
                },
                contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
                cumulative_digest: initial_journal_digest(target.position.generation),
            };
            validate_retained_journal_checkpoint(store, &store_checkpoint(&genesis))?;
            genesis
        }
    };
    sync_projection_journal_pages_through(
        store,
        status.state,
        checkpoint_compatible,
        prior,
        target,
        exchange,
    )?;
    Ok(NativeProAdvanceDisposition::Advanced)
}

pub(super) fn verify_canonical_frontier(
    data_root: &Path,
    expected: &JournalCheckpoint,
) -> Result<()> {
    let db_path = database_path(data_root.to_path_buf());
    let store = Store::open_read_only(&db_path)
        .context("not_materialized: reopen canonical Store before Pro publication")?;
    let snapshot = store
        .projection_journal_snapshot(Some(store_position(expected.position)))
        .context("not_materialized: verify canonical frontier before Pro publication")?;
    if protocol_checkpoint(snapshot.frozen_through) != *expected {
        bail!("not_materialized: canonical history advanced during Pro materialization");
    }
    Ok(())
}

pub(super) fn journal_sync_required(
    helper_state: ctx_pro_host_protocol::GraphState,
    checkpoint_compatible: bool,
    prior: &JournalCheckpoint,
    frozen_through: &JournalCheckpoint,
    batches: u64,
) -> bool {
    if prior != frozen_through {
        return true;
    }
    if batches > 0 {
        return false;
    }
    // A ready helper already at the frozen checkpoint has durably accepted this
    // range. Re-sending it as a zero-width page would reuse the prior page's
    // checkpoint coordinates with different contents.
    !(helper_state == ctx_pro_host_protocol::GraphState::Ready && checkpoint_compatible)
}

#[cfg(test)]
pub(super) fn coalesced_journal_snapshot(
    store: &Store,
    after: StoreJournalPosition,
) -> Result<ProjectionJournalSnapshot> {
    let snapshot = store
        .projection_journal_snapshot(Some(after))
        .context("source_unavailable: read frozen canonical projection journal page")?;
    let target = snapshot.frozen_through.clone();
    coalesce_journal_snapshot_through(after, &target, snapshot)
}

pub(super) fn coalesced_journal_snapshot_through(
    store: &Store,
    after: StoreJournalPosition,
    target: &StoreJournalCheckpoint,
) -> Result<ProjectionJournalSnapshot> {
    let snapshot = store
        .projection_journal_snapshot(Some(after))
        .context("source_unavailable: read bounded canonical projection journal page")?;
    coalesce_journal_snapshot_through(after, target, snapshot)
}

pub(super) fn coalesce_journal_snapshot_through(
    after: StoreJournalPosition,
    target: &StoreJournalCheckpoint,
    mut snapshot: ProjectionJournalSnapshot,
) -> Result<ProjectionJournalSnapshot> {
    if after.generation != target.position.generation
        || after.sequence > target.position.sequence
        || snapshot.frozen_through.contract_fingerprint != target.contract_fingerprint
        || snapshot.frozen_through.position.generation != target.position.generation
        || snapshot.frozen_through.position.sequence < target.position.sequence
    {
        bail!("corrupt: bounded canonical journal target is outside the retained Store chain");
    }
    snapshot
        .records
        .retain(|record| record.sequence <= target.position.sequence);
    snapshot.next_position = snapshot
        .records
        .last()
        .map_or(after, |record| StoreJournalPosition {
            generation: record.generation,
            sequence: record.sequence,
        });
    // One Store snapshot already coalesces the physical 64-record chunks into
    // an exact <=512-record/<=8 MiB page. Do not combine multiple byte-limited
    // pages before constructing the Protocol envelope: legal large records
    // could otherwise accumulate far beyond the 16 MiB wire budget before the
    // first size measurement.
    snapshot.frozen_through = target.clone();
    snapshot.has_more = snapshot.next_position.sequence < snapshot.frozen_through.position.sequence;
    Ok(snapshot)
}

pub(super) fn fit_journal_sync_request(
    mut request: JournalSyncRequest,
) -> Result<JournalSyncRequest> {
    let records = std::mem::take(&mut request.records);
    let mut envelope_bytes = journal_sync_envelope_bytes(&request).map_err(protocol_error)?;
    if envelope_bytes > MAX_JOURNAL_SYNC_ENVELOPE_BYTES {
        bail!("invalid_request: canonical journal context cannot fit the Protocol V1 envelope");
    }
    for record in records {
        let record_bytes = serde_json::to_vec(&record)
            .context("invalid_request: encode canonical journal record")?
            .len();
        let separator_bytes = usize::from(!request.records.is_empty());
        let candidate_bytes = envelope_bytes
            .saturating_add(separator_bytes)
            .saturating_add(record_bytes);
        if candidate_bytes > MAX_JOURNAL_SYNC_ENVELOPE_BYTES {
            break;
        }
        envelope_bytes = candidate_bytes;
        request.records.push(record);
    }
    if request.records.is_empty() && request.prior_checkpoint != request.frozen_through {
        bail!("invalid_request: one canonical journal record cannot fit the Protocol V1 envelope");
    }
    Ok(request)
}

pub(super) fn required_blame_capabilities(target: &BlameTarget) -> BTreeSet<Capability> {
    let mut capabilities = BTreeSet::from([Capability::Status, Capability::Query]);
    if target.requires_git_read() {
        capabilities.insert(Capability::GitRead);
    }
    capabilities
}

pub(super) fn validate_journal_ack(
    result: &JournalSyncResult,
    expected_checkpoint: &JournalCheckpoint,
    frozen_checkpoint: &JournalCheckpoint,
    expected_count: u32,
) -> Result<()> {
    if &result.committed_through != expected_checkpoint
        || result.frozen_complete != (expected_checkpoint == frozen_checkpoint)
        || (!result.replayed && result.accepted_records != expected_count)
        || (result.replayed && result.accepted_records != 0)
    {
        bail!("invalid_response: helper journal acknowledgement does not match the requested page");
    }
    Ok(())
}

pub(super) fn helper_status(client: &mut ProClient) -> Result<ctx_pro_host_protocol::StatusResult> {
    helper_status_with(&mut |message, timeout| client.exchange(message, timeout))
}

pub(super) fn helper_status_with(
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<StatusResult> {
    match exchange(HostMessage::Status(StatusRequest {}), HANDSHAKE_TIMEOUT)? {
        HelperMessage::Status(status) => {
            status
                .validate()
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            Ok(status)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-status response"),
    }
}

pub(super) const fn store_position(position: JournalPosition) -> StoreJournalPosition {
    StoreJournalPosition {
        generation: position.generation,
        sequence: position.sequence,
    }
}

pub(super) fn protocol_checkpoint(checkpoint: StoreJournalCheckpoint) -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: checkpoint.position.generation,
            sequence: checkpoint.position.sequence,
        },
        contract_fingerprint: checkpoint.contract_fingerprint,
        cumulative_digest: checkpoint.cumulative_digest,
    }
}

pub(super) fn store_checkpoint(checkpoint: &JournalCheckpoint) -> StoreJournalCheckpoint {
    StoreJournalCheckpoint {
        position: StoreJournalPosition {
            generation: checkpoint.position.generation,
            sequence: checkpoint.position.sequence,
        },
        contract_fingerprint: checkpoint.contract_fingerprint.clone(),
        cumulative_digest: checkpoint.cumulative_digest.clone(),
    }
}

pub(super) fn protocol_journal_record(record: ProjectionJournalRecord) -> JournalRecord {
    JournalRecord {
        generation: record.generation,
        sequence: record.sequence,
        projection_contract_version: record.projection_contract_version,
        entity_kind: protocol_entity_kind(record.entity_kind),
        stable_entity_id: record.stable_entity_id,
        entity_revision: record.entity_revision,
        operation: match record.operation {
            StoreJournalOperation::Upsert => JournalOperation::Upsert,
            StoreJournalOperation::Delete => JournalOperation::Delete,
        },
        canonical_payload: record.canonical_payload,
        payload_sha256: record.payload_sha256,
        evidence: record
            .evidence
            .into_iter()
            .map(protocol_journal_evidence)
            .collect(),
        provenance: protocol_journal_provenance(record.provenance),
        cumulative_digest: record.cumulative_digest,
    }
}

pub(super) const fn protocol_entity_kind(kind: StoreJournalEntityKind) -> JournalEntityKind {
    match kind {
        StoreJournalEntityKind::Event => JournalEntityKind::Event,
        StoreJournalEntityKind::FileTouch => JournalEntityKind::FileTouch,
        StoreJournalEntityKind::VcsChange => JournalEntityKind::VcsChange,
    }
}

pub(super) fn protocol_journal_evidence(
    evidence: StoreJournalEvidenceIdentity,
) -> JournalEvidenceIdentity {
    JournalEvidenceIdentity {
        event_id: evidence.event_id,
        source_id: evidence.source_id,
        source_path: evidence.source_path,
        source_record_ordinal: evidence.source_record_ordinal,
        source_record_subrecord_index: evidence.source_record_subrecord_index,
        byte_start: evidence.byte_start,
        byte_end_exclusive: evidence.byte_end_exclusive,
    }
}

pub(super) fn protocol_journal_provenance(
    provenance: StoreJournalProvenanceIdentity,
) -> JournalProvenanceIdentity {
    JournalProvenanceIdentity {
        entity_kind: protocol_entity_kind(provenance.entity_kind),
        stable_entity_id: provenance.stable_entity_id,
        capture_source_id: provenance.capture_source_id,
        provider: provenance.provider,
        provider_external_id: provenance.provider_external_id,
    }
}

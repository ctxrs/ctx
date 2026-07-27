use std::{
    collections::BTreeSet,
    io::Read,
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::database_path;
use ctx_history_store::{
    JournalCheckpoint as StoreJournalCheckpoint, JournalEntityKind as StoreJournalEntityKind,
    JournalEvidenceIdentity as StoreJournalEvidenceIdentity,
    JournalOperation as StoreJournalOperation, JournalPosition as StoreJournalPosition,
    JournalProvenanceIdentity as StoreJournalProvenanceIdentity, ProjectionJournalRecord,
    ProjectionJournalSnapshot, Store,
};
use ctx_pro_host_protocol::{
    decode_base64url, initial_journal_digest, journal_sync_envelope_bytes, read_frame, write_frame,
    BlameResult, BlameTarget, Capability, ConfirmGraphKeyDeletionRequest, EntitlementAccessState,
    GraphKeyDeletionPrepared, GraphState, HelloRequest, HelperEnvelope, HelperMessage,
    HostEnvelope, HostMessage, JournalCheckpoint, JournalEntityKind, JournalEvidenceIdentity,
    JournalOperation, JournalPosition, JournalProvenanceIdentity, JournalRecord, JournalSyncMode,
    JournalSyncRequest, JournalSyncResult, PrepareGraphKeyDeletionRequest, StatusRequest,
    StatusResult, GRAPH_KEY_DELETION_CHALLENGE_BYTES, MAX_JOURNAL_RECORDS_PER_BATCH,
    MAX_JOURNAL_SYNC_ENVELOPE_BYTES, PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
};
use serde::Serialize;
use uuid::Uuid;

use crate::analytics::{
    pro_helper_connection_outcome, ProHelperConnectionOutcomeV1, ProMaterializationModeV1,
    ProMaterializationTelemetryV1,
};

#[path = "client_support.rs"]
mod support;
use super::authorization::{
    AuthorizationProvider, EntitlementSchedule, StoredAuthorizationProvider,
};
use super::credential_vault::CredentialVaultNamespace;
use super::verified_executable::VerifiedHelperExecutable;
pub(crate) use support::default_helper_path;
use support::{git_executable, helper_executable};

#[path = "client_errors.rs"]
mod errors;
use errors::protocol_error;
pub(crate) use errors::stable_error_code;

#[path = "client_environment.rs"]
mod environment;
use environment::configure_helper_environment;

#[path = "client_output.rs"]
mod output;
pub(crate) use output::ProOutputImport;

#[path = "client_status.rs"]
mod client_status;
#[cfg(all(test, unix))]
use client_status::smoke_helper_at_path_with_authorization;
#[cfg(test)]
use client_status::status_outcome;
#[cfg(test)]
pub(crate) use client_status::status_with_helper_resolver;
pub(crate) use client_status::{
    smoke_helper_at_path, status, HelperSmoke, ProSetupRepairability, ProStatus,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const BLAME_TIMEOUT: Duration = Duration::from_secs(30);
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);
const STDERR_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaterializeReport {
    pub(crate) schema_version: u32,
    pub(crate) payload_type: &'static str,
    pub(crate) frontier: u64,
    pub(crate) batches: u64,
    pub(crate) observations: u64,
    pub(crate) replayed_batches: u64,
}

pub(crate) fn blame(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<BlameResult> {
    let first = blame_once(data_root, target.clone(), limit, cursor.clone());
    let should_catch_up = first.as_ref().is_err_and(|error| {
        matches!(
            stable_error_code(error),
            Some("not_materialized" | "needs_rebuild" | "partial" | "needs_resume")
        )
    });
    if !should_catch_up {
        return first;
    }
    let mut materialization = ProMaterializationTelemetryV1::started();
    materialize(data_root, &mut materialization)?;
    blame_once(data_root, target, limit, cursor)
}

fn blame_once(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<BlameResult> {
    let request = support::current_blame_request(data_root, target, limit, cursor)?;
    request
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let capabilities = required_blame_capabilities(&request.target);
    let request_context = request.clone();
    let mut client = ProClient::connect(data_root, &capabilities)?;
    match client.exchange(HostMessage::Blame(request), BLAME_TIMEOUT)? {
        HelperMessage::Blame(result) => {
            validate_blame_response(&request_context, &result)?;
            Ok(result)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-blame response"),
    }
}

fn validate_blame_response(
    request: &ctx_pro_host_protocol::BlameRequest,
    result: &BlameResult,
) -> Result<()> {
    result
        .validate_for_request(request)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))
}

pub(super) fn delete_graph_key(
    data_root: &Path,
    namespace: CredentialVaultNamespace,
    installation_key_thumbprint: &str,
) -> Result<()> {
    if decode_base64url(installation_key_thumbprint)
        .as_deref()
        .map(<[u8]>::len)
        != Some(32)
    {
        bail!("invalid_request: installation key thumbprint is invalid");
    }
    let required = BTreeSet::from([Capability::GraphKeyDeletion]);
    let mut client = ProClient::connect(data_root, &required)?;
    delete_graph_key_with_client(&mut client, installation_key_thumbprint, |challenge| {
        StoredAuthorizationProvider::load_for_graph_key_deletion(
            data_root,
            namespace,
            installation_key_thumbprint,
        )?
        .authorization_for_challenge(challenge)
    })
}

fn delete_graph_key_with_client(
    client: &mut ProClient,
    installation_key_thumbprint: &str,
    authorize: impl FnOnce(
        &[u8; GRAPH_KEY_DELETION_CHALLENGE_BYTES],
    ) -> Result<ctx_pro_host_protocol::AuthorizationRequest>,
) -> Result<()> {
    let prepared = prepare_graph_key_deletion(client, installation_key_thumbprint)?;
    if !prepared.key_present {
        return Ok(());
    }
    let challenge = graph_key_deletion_challenge(&prepared)?;
    let authorization = authorize(&challenge)?;
    match client.exchange(
        HostMessage::ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest { authorization }),
        HANDSHAKE_TIMEOUT,
    )? {
        HelperMessage::GraphKeyDeleted(_) => {}
        HelperMessage::Error(error) => return Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-deletion response"),
    }
    let verified = prepare_graph_key_deletion(client, installation_key_thumbprint)?;
    if verified.key_present {
        bail!("key_store_unavailable: graph-key deletion could not be verified");
    }
    Ok(())
}

fn prepare_graph_key_deletion(
    client: &mut ProClient,
    installation_key_thumbprint: &str,
) -> Result<GraphKeyDeletionPrepared> {
    let prepared = match client.exchange(
        HostMessage::PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest {
            installation_key_thumbprint: installation_key_thumbprint.to_owned(),
        }),
        HANDSHAKE_TIMEOUT,
    )? {
        HelperMessage::GraphKeyDeletionPrepared(prepared) => prepared,
        HelperMessage::Error(error) => return Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-deletion-preparation response"),
    };
    let _ = graph_key_deletion_challenge(&prepared)?;
    Ok(prepared)
}

fn graph_key_deletion_challenge(
    prepared: &GraphKeyDeletionPrepared,
) -> Result<[u8; GRAPH_KEY_DELETION_CHALLENGE_BYTES]> {
    decode_base64url(&prepared.challenge_base64url)
        .and_then(|challenge| challenge.try_into().ok())
        .ok_or_else(|| anyhow!("invalid_response: helper returned an invalid deletion challenge"))
}

pub(crate) fn materialize(
    data_root: &Path,
    telemetry: &mut ProMaterializationTelemetryV1,
) -> Result<MaterializeReport> {
    let result = retry_materialization_once(|| materialize_once(data_root, telemetry))
        .and_then(|report| super::pending_materialization::clear_after(data_root, report));
    if let Err(error) = &result {
        telemetry.fail(stable_error_code(error));
    }
    result
}

fn retry_materialization_once<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    // Give a live private rebuild owner one bounded helper-session interval to
    // publish or drop without turning concurrent activation into a retry loop.
    match operation() {
        Err(error) if stable_error_code(&error) == Some("not_materialized") => operation(),
        result => result,
    }
}

fn materialize_once(
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
struct JournalPageSyncReport {
    committed_through: JournalCheckpoint,
    batches: u64,
    observations: u64,
    accepted_observations: u64,
    replayed_batches: u64,
    initial_lag: u64,
    full_baseline_from_ready: bool,
}

fn sync_projection_journal_pages_through(
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

fn validate_retained_journal_checkpoint(
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
    let mut snapshot = store
        .projection_journal_snapshot(None)
        .context("source_unavailable: read retained canonical projection journal")?;
    if checkpoint.contract_fingerprint != snapshot.frozen_through.contract_fingerprint
        || checkpoint.position.generation != snapshot.frozen_through.position.generation
        || checkpoint.position.sequence > snapshot.frozen_through.position.sequence
    {
        bail!("corrupt: Store journal checkpoint is outside the active retained generation");
    }
    if checkpoint.position.sequence == snapshot.frozen_through.position.sequence {
        if checkpoint.cumulative_digest != snapshot.frozen_through.cumulative_digest {
            bail!("corrupt: Store journal target cumulative digest does not match retained data");
        }
        return Ok(());
    }

    let retained_after = snapshot
        .records
        .first()
        .map_or(snapshot.next_position.sequence, |record| {
            record.sequence.saturating_sub(1)
        });
    if checkpoint.position.sequence < retained_after {
        bail!("corrupt: Store journal checkpoint is older than the retained suffix");
    }
    if checkpoint.position.sequence == retained_after {
        store
            .acknowledge_projection_journal(checkpoint)
            .context("corrupt: validate retained Store journal checkpoint digest")?;
        return Ok(());
    }

    loop {
        if let Some(record) = snapshot
            .records
            .iter()
            .find(|record| record.sequence == checkpoint.position.sequence)
        {
            if record.cumulative_digest != checkpoint.cumulative_digest {
                bail!(
                    "corrupt: Store journal target cumulative digest does not match retained data"
                );
            }
            return Ok(());
        }
        if snapshot.next_position.sequence >= checkpoint.position.sequence {
            break;
        }
        let prior_position = snapshot.next_position;
        snapshot = store
            .projection_journal_snapshot(Some(prior_position))
            .context("source_unavailable: walk retained canonical projection journal")?;
        if snapshot.next_position.sequence <= prior_position.sequence
            || snapshot.frozen_through.contract_fingerprint != checkpoint.contract_fingerprint
            || snapshot.frozen_through.position.generation != checkpoint.position.generation
            || snapshot.frozen_through.position.sequence < checkpoint.position.sequence
        {
            bail!("corrupt: canonical projection journal changed during checkpoint validation");
        }
    }
    bail!("corrupt: Store journal checkpoint digest is not present in the retained suffix")
}

fn nativepath_pro_capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
    ])
}

fn prepare_nativepath_projection_journal(
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
enum NativeProAdvanceDisposition {
    Advanced,
    AlreadyAdvanced,
}

fn sync_nativepath_group_through(
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
            validate_retained_journal_checkpoint(store, &store_checkpoint(&checkpoint))?;
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

fn verify_canonical_frontier(data_root: &Path, expected: &JournalCheckpoint) -> Result<()> {
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

fn journal_sync_required(
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
fn coalesced_journal_snapshot(
    store: &Store,
    after: StoreJournalPosition,
) -> Result<ProjectionJournalSnapshot> {
    let snapshot = store
        .projection_journal_snapshot(Some(after))
        .context("source_unavailable: read frozen canonical projection journal page")?;
    let target = snapshot.frozen_through.clone();
    coalesce_journal_snapshot_through(store, after, &target, snapshot)
}

fn coalesced_journal_snapshot_through(
    store: &Store,
    after: StoreJournalPosition,
    target: &StoreJournalCheckpoint,
) -> Result<ProjectionJournalSnapshot> {
    let snapshot = store
        .projection_journal_snapshot(Some(after))
        .context("source_unavailable: read bounded canonical projection journal page")?;
    coalesce_journal_snapshot_through(store, after, target, snapshot)
}

fn coalesce_journal_snapshot_through(
    store: &Store,
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
    while snapshot.records.len() < MAX_JOURNAL_RECORDS_PER_BATCH
        && snapshot.next_position.sequence < target.position.sequence
    {
        let next = store
            .projection_journal_snapshot(Some(snapshot.next_position))
            .context("source_unavailable: read frozen canonical projection journal page")?;
        if next.canonical_schema_version != snapshot.canonical_schema_version
            || next.canonical_schema_identity != snapshot.canonical_schema_identity
            || next.projection_contract_version != snapshot.projection_contract_version
            || next.frozen_through.contract_fingerprint != target.contract_fingerprint
            || next.frozen_through.position.generation != target.position.generation
            || next.frozen_through.position.sequence < target.position.sequence
        {
            bail!("corrupt: canonical projection journal changed during a frozen page walk");
        }
        let remaining = MAX_JOURNAL_RECORDS_PER_BATCH.saturating_sub(snapshot.records.len());
        let records = next
            .records
            .into_iter()
            .take_while(|record| record.sequence <= target.position.sequence)
            .take(remaining)
            .collect::<Vec<_>>();
        if records.is_empty() {
            bail!("corrupt: canonical projection journal stopped before its frozen checkpoint");
        }
        snapshot.next_position = StoreJournalPosition {
            generation: records
                .last()
                .map_or(target.position.generation, |record| record.generation),
            sequence: records
                .last()
                .map_or(snapshot.next_position.sequence, |record| record.sequence),
        };
        snapshot.records.extend(records);
    }
    snapshot.frozen_through = target.clone();
    snapshot.has_more = snapshot.next_position.sequence < snapshot.frozen_through.position.sequence;
    Ok(snapshot)
}

fn fit_journal_sync_request(mut request: JournalSyncRequest) -> Result<JournalSyncRequest> {
    while journal_sync_envelope_bytes(&request).map_err(protocol_error)?
        > MAX_JOURNAL_SYNC_ENVELOPE_BYTES
    {
        if request.records.pop().is_none() {
            bail!("invalid_request: canonical journal page cannot fit the Protocol V1 envelope");
        }
    }
    if request.records.is_empty() && request.prior_checkpoint != request.frozen_through {
        bail!("invalid_request: one canonical journal record cannot fit the Protocol V1 envelope");
    }
    Ok(request)
}

fn required_blame_capabilities(target: &BlameTarget) -> BTreeSet<Capability> {
    let mut capabilities = BTreeSet::from([Capability::Query]);
    if target.requires_git_read() {
        capabilities.insert(Capability::GitRead);
    }
    capabilities
}

fn validate_journal_ack(
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

fn helper_status(client: &mut ProClient) -> Result<ctx_pro_host_protocol::StatusResult> {
    helper_status_with(&mut |message, timeout| client.exchange(message, timeout))
}

fn helper_status_with(
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<StatusResult> {
    match exchange(HostMessage::Status(StatusRequest {}), HANDSHAKE_TIMEOUT)? {
        HelperMessage::Status(status) => Ok(status),
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-status response"),
    }
}

const fn store_position(position: JournalPosition) -> StoreJournalPosition {
    StoreJournalPosition {
        generation: position.generation,
        sequence: position.sequence,
    }
}

fn protocol_checkpoint(checkpoint: StoreJournalCheckpoint) -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: checkpoint.position.generation,
            sequence: checkpoint.position.sequence,
        },
        contract_fingerprint: checkpoint.contract_fingerprint,
        cumulative_digest: checkpoint.cumulative_digest,
    }
}

fn store_checkpoint(checkpoint: &JournalCheckpoint) -> StoreJournalCheckpoint {
    StoreJournalCheckpoint {
        position: StoreJournalPosition {
            generation: checkpoint.position.generation,
            sequence: checkpoint.position.sequence,
        },
        contract_fingerprint: checkpoint.contract_fingerprint.clone(),
        cumulative_digest: checkpoint.cumulative_digest.clone(),
    }
}

fn protocol_journal_record(record: ProjectionJournalRecord) -> JournalRecord {
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

const fn protocol_entity_kind(kind: StoreJournalEntityKind) -> JournalEntityKind {
    match kind {
        StoreJournalEntityKind::Event => JournalEntityKind::Event,
        StoreJournalEntityKind::FileTouch => JournalEntityKind::FileTouch,
        StoreJournalEntityKind::VcsChange => JournalEntityKind::VcsChange,
    }
}

fn protocol_journal_evidence(evidence: StoreJournalEvidenceIdentity) -> JournalEvidenceIdentity {
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

fn protocol_journal_provenance(
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

pub(crate) struct ProClient {
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    child: Arc<Mutex<Child>>,
    stderr: StderrDrain,
    sequence: u64,
    capabilities: BTreeSet<Capability>,
    helper_version: String,
    authorization_state: Option<EntitlementAccessState>,
    entitlement_schedule: Option<EntitlementSchedule>,
}

impl ProClient {
    fn connect(data_root: &Path, required: &BTreeSet<Capability>) -> Result<Self> {
        Self::connect_with_authorization_mode(data_root, required, None, false)
    }

    fn connect_for_status(data_root: &Path, required: &BTreeSet<Capability>) -> Result<Self> {
        Self::connect_with_authorization_mode(data_root, required, None, true)
    }

    fn connect_with_authorization_mode(
        data_root: &Path,
        required: &BTreeSet<Capability>,
        authorization: Option<&dyn AuthorizationProvider>,
        bind_status_identity: bool,
    ) -> Result<Self> {
        let executable = helper_executable(data_root)?;
        let path = executable.path().to_path_buf();
        Self::connect_to_path_with_authorization_mode(
            data_root,
            &path,
            Some(executable),
            required,
            authorization,
            bind_status_identity,
        )
    }

    fn connect_to_path_with_authorization_mode(
        data_root: &Path,
        path: &Path,
        execution_guard: Option<VerifiedHelperExecutable>,
        required: &BTreeSet<Capability>,
        authorization: Option<&dyn AuthorizationProvider>,
        bind_status_identity: bool,
    ) -> Result<Self> {
        // Resolve Git while the public process still has its ordinary PATH. The helper receives
        // only this exact executable locator after its environment is cleared; it never receives
        // broad executable search state or a shell command.
        let needs_git = required.contains(&Capability::GitRead)
            || required.contains(&Capability::JournalSync)
            || required.contains(&Capability::OutputMaterialization);
        let git_executable = needs_git.then(git_executable).transpose()?;
        let mut command = Command::new(path);
        command
            .arg("serve-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let installation_id = crate::identity::existing_installation_id(data_root)
            .context("key_store_unavailable: load local Pro installation identity")?
            .ok_or_else(|| {
                anyhow!("key_store_unavailable: local Pro installation identity is missing")
            })?;
        configure_helper_environment(
            &mut command,
            data_root,
            &installation_id,
            git_executable.as_deref(),
        )?;
        #[cfg(target_os = "linux")]
        {
            let expected_parent = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != expected_parent {
                        libc::kill(libc::getpid(), libc::SIGKILL);
                        libc::_exit(127);
                    }
                    Ok(())
                });
            }
        }
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        if let Some(executable) = execution_guard.as_ref() {
            executable.verify_execution_identity()?;
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("helper_crashed: start Pro helper {}", path.display()))?;
        drop(execution_guard);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("helper_crashed: Pro helper stdin was unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("helper_crashed: Pro helper stdout was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("helper_crashed: Pro helper stderr was unavailable"))?;
        let mut client = Self {
            stdin: Some(stdin),
            stdout,
            child: Arc::new(Mutex::new(child)),
            stderr: StderrDrain::start(stderr),
            sequence: 0,
            capabilities: BTreeSet::new(),
            helper_version: String::new(),
            authorization_state: None,
            entitlement_schedule: None,
        };
        let offered = BTreeSet::from([
            Capability::EntitlementAuthorization,
            Capability::GraphKeyDeletion,
            Capability::Status,
            Capability::JournalSync,
            Capability::OutputMaterialization,
            Capability::Query,
            Capability::GitRead,
        ]);
        let response = client.exchange(
            HostMessage::Hello(HelloRequest::current(
                env!("CARGO_PKG_VERSION"),
                offered.clone(),
            )),
            HANDSHAKE_TIMEOUT,
        )?;
        let hello = match response {
            HelperMessage::Hello(hello) => hello,
            HelperMessage::Error(error) => return Err(protocol_error(error)),
            _ => bail!("protocol_mismatch: helper did not answer hello negotiation"),
        };
        if hello.protocol_version != PROTOCOL_VERSION
            || hello.protocol_fingerprint != PROTOCOL_FINGERPRINT
        {
            bail!("protocol_mismatch: helper does not implement the exact Protocol V1 inventory");
        }
        if !hello.capabilities.is_subset(&offered) {
            bail!("protocol_mismatch: helper advertised capabilities the host did not offer");
        }
        if let Some(missing) = required
            .iter()
            .find(|capability| !hello.capabilities.contains(capability))
        {
            bail!("protocol_mismatch: helper does not support required capability {missing:?}");
        }
        let authorization_selected = hello
            .capabilities
            .contains(&Capability::EntitlementAuthorization);
        if authorization_selected && authorization_required(required, bind_status_identity) {
            let challenge =
                ctx_pro_host_protocol::decode_base64url(&hello.authorization_challenge_base64url)
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(|| {
                        anyhow!(
                            "protocol_mismatch: helper returned an invalid authorization challenge"
                        )
                    })?;
            let stored;
            let entitlement_schedule;
            let provider: &dyn AuthorizationProvider = if let Some(provider) = authorization {
                entitlement_schedule = None;
                provider
            } else if bind_status_identity {
                stored = StoredAuthorizationProvider::load_for_status(data_root)?;
                entitlement_schedule = Some(stored.entitlement_schedule());
                &stored
            } else {
                stored = StoredAuthorizationProvider::load(data_root)?;
                entitlement_schedule = Some(stored.entitlement_schedule());
                &stored
            };
            let request = provider.authorization_for_challenge(&challenge)?;
            match client.exchange(HostMessage::Authorize(request), HANDSHAKE_TIMEOUT)? {
                HelperMessage::Authorized(result) => {
                    client.authorization_state = Some(result.state);
                    client.entitlement_schedule = entitlement_schedule;
                }
                HelperMessage::Error(error) => return Err(protocol_error(error)),
                _ => bail!("invalid_response: helper returned a non-authorization response"),
            }
        }
        client.capabilities = hello.capabilities;
        client.helper_version = hello.helper_version;
        Ok(client)
    }

    fn public_access_status(&self) -> PublicAccessStatus {
        PublicAccessStatus {
            state: self.authorization_state.map(access_state_name),
            refresh_after_unix: self
                .entitlement_schedule
                .map(|schedule| schedule.refresh_after_unix),
            access_deadline_unix: self
                .entitlement_schedule
                .map(|schedule| schedule.access_deadline_unix),
            grace_deadline_unix: self
                .entitlement_schedule
                .map(|schedule| schedule.grace_deadline_unix),
        }
    }

    fn exchange(&mut self, message: HostMessage, timeout: Duration) -> Result<HelperMessage> {
        let request_id = Uuid::new_v4();
        let sequence = self.sequence;
        let request = HostEnvelope {
            sequence,
            request_id,
            message,
        };
        if matches!(&request.message, HostMessage::SyncJournal(_))
            && serde_json::to_vec(&request)
                .context("invalid_request: encode journal request")?
                .len()
                > MAX_JOURNAL_SYNC_ENVELOPE_BYTES
        {
            bail!("invalid_request: journal request exceeds the Protocol V1 envelope bound");
        }
        let timed_out = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = mpsc::channel();
        let watchdog_child = Arc::clone(&self.child);
        let watchdog_timed_out = Arc::clone(&timed_out);
        let watchdog = thread::spawn(move || {
            if stop_rx.recv_timeout(timeout).is_err() {
                watchdog_timed_out.store(true, Ordering::Release);
                if let Ok(mut child) = watchdog_child.lock() {
                    kill_helper_process(&mut child);
                }
            }
        });
        let response = (|| -> Result<_> {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| anyhow!("helper_crashed: helper stdin is closed"))?;
            write_frame(stdin, &request).context("helper_crashed: write framed request")?;
            Ok(read_frame::<_, HelperEnvelope>(&mut self.stdout))
        })();
        let _ = stop_tx.send(());
        let _ = watchdog.join();
        if timed_out.load(Ordering::Acquire) {
            self.stdin.take();
            if let Ok(mut child) = self.child.lock() {
                kill_helper_process(&mut child);
                let _ = child.wait();
            }
            bail!("helper_timeout: Pro helper exceeded its exchange deadline");
        }
        let response = response?;
        let response = match response {
            Ok(response) => response,
            Err(ctx_pro_host_protocol::FrameError::UnsupportedVersion {
                received,
                supported,
            }) => bail!(
                "protocol_mismatch: helper frame version {received} does not equal {supported}"
            ),
            Err(error) => {
                let exited = self
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut child| child.try_wait().ok().flatten());
                if let Some(status) = exited {
                    bail!("helper_crashed: Pro helper exited with {status}");
                }
                return Err(error).context("invalid_response: read framed helper response");
            }
        };
        if response.sequence != sequence || response.request_id != request_id {
            bail!(
                "invalid_response: helper response identity or sequence did not match the request"
            );
        }
        self.sequence = self.sequence.saturating_add(1);
        Ok(response.message)
    }
}

struct PublicAccessStatus {
    state: Option<String>,
    refresh_after_unix: Option<i64>,
    access_deadline_unix: Option<i64>,
    grace_deadline_unix: Option<i64>,
}

fn access_state_name(state: EntitlementAccessState) -> String {
    match state {
        EntitlementAccessState::Trial => "trial",
        EntitlementAccessState::Active => "active",
        EntitlementAccessState::CancelingPaid => "canceling_paid",
        EntitlementAccessState::OfflineGrace => "offline_grace",
        EntitlementAccessState::Locked => "locked",
    }
    .to_owned()
}

fn authorization_required(required: &BTreeSet<Capability>, bind_status_identity: bool) -> bool {
    bind_status_identity
        || required.iter().any(|capability| {
            !matches!(
                *capability,
                Capability::Status | Capability::GraphKeyDeletion
            )
        })
}

impl Drop for ProClient {
    fn drop(&mut self) {
        self.stdin.take();
        if let Ok(mut child) = self.child.lock() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    kill_helper_process(&mut child);
                    let _ = child.wait();
                }
            }
        }
        self.stderr.finish();
    }
}

fn kill_helper_process(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = i32::try_from(child.id()).ok().and_then(i32::checked_neg);
        if let Some(process_group) = process_group {
            // The child is placed in a fresh process group before spawn. Killing
            // the group prevents descendants from retaining inherited IPC pipes.
            unsafe {
                libc::kill(process_group, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

struct StderrDrain {
    bytes: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StderrDrain {
    fn start(mut stderr: ChildStderr) -> Self {
        let bytes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&bytes);
        let thread = thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let current = observed.load(Ordering::Relaxed);
                        observed.store(
                            current.saturating_add(read).min(STDERR_MAX_BYTES),
                            Ordering::Relaxed,
                        );
                    }
                }
            }
        });
        Self {
            bytes,
            thread: Some(thread),
        }
    }

    fn finish(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.bytes.load(Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

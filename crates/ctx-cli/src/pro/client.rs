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
    initial_journal_digest, journal_sync_envelope_bytes, read_frame, write_frame, Capability,
    EntitlementAccessState, EvidenceCitation, HelloRequest, HelperEnvelope, HelperMessage,
    HostEnvelope, HostMessage, JournalCheckpoint, JournalEntityKind, JournalEvidenceIdentity,
    JournalOperation, JournalPosition, JournalProvenanceIdentity, JournalRecord, JournalSyncMode,
    JournalSyncRequest, JournalSyncResult, QueryKind, QueryResult, ResourceSelector, StatusRequest,
    MAX_JOURNAL_RECORDS_PER_BATCH, MAX_JOURNAL_SYNC_ENVELOPE_BYTES, PROTOCOL_FINGERPRINT,
    PROTOCOL_VERSION,
};
#[cfg(test)]
use ctx_pro_host_protocol::{GraphState, MAX_RESULT_CONTENT_BYTES_PER_ITEM};
use serde::Serialize;
use uuid::Uuid;

use crate::analytics::{
    pro_helper_connection_outcome, ProAccessStateV1, ProAutoMaterializationV1,
    ProHelperConnectionOutcomeV1, ProMaterializationModeV1, ProMaterializationTelemetryV1,
    ProQueryTelemetryV1,
};

#[path = "client_support.rs"]
mod support;
use super::authorization::{
    AuthorizationProvider, EntitlementSchedule, StoredAuthorizationProvider,
};
use super::verified_executable::VerifiedHelperExecutable;
pub(crate) use support::default_helper_path;
use support::{git_executable, helper_executable};

#[path = "client_environment.rs"]
mod environment;
use environment::configure_helper_environment;

#[path = "client_result_content.rs"]
mod result_content;
use result_content::hydrate_result_contents;
#[cfg(test)]
use result_content::ResultHydrationCounts;

#[path = "client_status.rs"]
mod client_status;
#[cfg(all(test, unix))]
use client_status::smoke_helper_at_path_with_authorization;
#[cfg(test)]
use client_status::status_outcome;
pub(crate) use client_status::{smoke_helper_at_path, status, HelperSmoke, ProStatus};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
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
    pub(crate) result_contents_hydrated: u64,
    pub(crate) result_contents_omitted: u64,
}

pub(crate) fn query(
    data_root: &Path,
    kind: QueryKind,
    target: ResourceSelector,
    limit: u32,
    cursor: Option<String>,
    telemetry: &mut ProQueryTelemetryV1,
) -> Result<QueryResult> {
    let first = query_once(
        data_root,
        kind,
        target.clone(),
        limit,
        cursor.clone(),
        telemetry,
    );
    let should_catch_up = first.as_ref().is_err_and(|error| {
        matches!(
            stable_error_code(error),
            Some(
                "not_materialized"
                    | "needs_rebuild"
                    | "partial"
                    | "needs_resume"
                    | "protocol_mismatch"
            )
        )
    });
    if !should_catch_up {
        return first;
    }
    let mut materialization = ProMaterializationTelemetryV1::started();
    if let Err(error) = materialize(data_root, &mut materialization) {
        telemetry.auto_materialization = ProAutoMaterializationV1::Failed;
        if materialization.helper_connection != ProHelperConnectionOutcomeV1::NotAttempted {
            telemetry.helper_connection = materialization.helper_connection;
        }
        telemetry.materialization = Some(materialization);
        return Err(error);
    }
    telemetry.auto_materialization = ProAutoMaterializationV1::Completed;
    if materialization.helper_connection != ProHelperConnectionOutcomeV1::NotAttempted {
        telemetry.helper_connection = materialization.helper_connection;
    }
    telemetry.materialization = Some(materialization);
    query_once(data_root, kind, target, limit, cursor, telemetry)
}

fn query_once(
    data_root: &Path,
    kind: QueryKind,
    target: ResourceSelector,
    limit: u32,
    cursor: Option<String>,
    telemetry: &mut ProQueryTelemetryV1,
) -> Result<QueryResult> {
    let request = support::current_query_request(data_root, kind, target, limit, cursor)?;
    request
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let capabilities = required_query_capabilities(request.kind);
    let requested_limit = request.limit as usize;
    let mut client = match ProClient::connect(data_root, &capabilities) {
        Ok(client) => client,
        Err(error) => {
            telemetry.helper_connection = pro_helper_connection_outcome(stable_error_code(&error));
            return Err(error);
        }
    };
    telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
    telemetry.access_state = client
        .authorization_state
        .map(ProAccessStateV1::from_protocol);
    match client.exchange(HostMessage::Query(request), QUERY_TIMEOUT)? {
        HelperMessage::Query(result) => {
            if result.records.len() > requested_limit {
                bail!("invalid_response: helper returned more records than requested");
            }
            if result.next_cursor.is_some() && !result.truncated {
                bail!("invalid_response: helper returned a continuation for a complete result");
            }
            if result.next_cursor.as_deref().is_some_and(|cursor| {
                cursor.is_empty()
                    || cursor.len() > ctx_pro_host_protocol::MAX_QUERY_CURSOR_BYTES
                    || !cursor.is_ascii()
            }) {
                bail!("invalid_response: helper returned an invalid continuation cursor");
            }
            for record in &result.records {
                if record.citations.len() > ctx_pro_host_protocol::MAX_CITATIONS_PER_FACT {
                    bail!("invalid_response: helper returned too many record citations");
                }
                if !record.citations.is_empty()
                    && !record.citations.iter().any(EvidenceCitation::is_usable)
                {
                    bail!("invalid_response: helper returned no usable record citation");
                }
                for fact in &record.facts {
                    fact.validate()
                        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
                }
            }
            Ok(result)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-query response"),
    }
}

pub(crate) fn materialize(
    data_root: &Path,
    telemetry: &mut ProMaterializationTelemetryV1,
) -> Result<MaterializeReport> {
    let result = retry_materialization_once(|| materialize_once(data_root, telemetry));
    if let Err(error) = &result {
        telemetry.fail(stable_error_code(error));
    }
    result
}

fn retry_materialization_once<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
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
    let store = Store::open(&db_path).with_context(|| {
        format!(
            "source_unavailable: open canonical Store {}",
            db_path.display()
        )
    })?;
    store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .context("source_unavailable: activate canonical projection journal")?;
    let required = BTreeSet::from([Capability::Status, Capability::JournalSync]);
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
    let mut prior = helper_checkpoint
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
    let mut batches = 0_u64;
    let mut observations = 0_u64;
    let mut accepted_observations = 0_u64;
    let mut replayed_batches = 0_u64;
    let mut result_contents_hydrated = 0_u64;
    let mut result_contents_omitted = 0_u64;
    let mut initial_lag = None;
    let mut authorized_repository_roots: Option<Vec<String>> = None;
    loop {
        let snapshot = coalesced_journal_snapshot(&store, store_position(prior.position))?;
        let frozen_through = protocol_checkpoint(snapshot.frozen_through);
        initial_lag.get_or_insert_with(|| {
            if prior.position.generation == frozen_through.position.generation {
                frozen_through
                    .position
                    .sequence
                    .saturating_sub(prior.position.sequence)
            } else {
                frozen_through.position.sequence
            }
        });
        if prior == frozen_through && batches > 0 {
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
            && helper_state == ctx_pro_host_protocol::GraphState::Ready
        {
            telemetry.mode = Some(ProMaterializationModeV1::Full);
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
            frozen_through,
            authorized_repository_roots: roots,
            records,
            result_contents: Vec::new(),
        })?;
        let (result, expected_records) = loop {
            let hydration = hydrate_result_contents(&store, &mut request);
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
            let response =
                client.exchange(HostMessage::SyncJournal(request.clone()), BATCH_TIMEOUT)?;
            match response {
                HelperMessage::JournalSynced(result) => {
                    validate_journal_ack(
                        &result,
                        &expected_checkpoint,
                        &frozen_checkpoint,
                        expected_records,
                    )?;
                    result_contents_hydrated =
                        result_contents_hydrated.saturating_add(hydration.hydrated);
                    result_contents_omitted =
                        result_contents_omitted.saturating_add(hydration.omitted);
                    break (result, expected_records);
                }
                HelperMessage::Error(error)
                    if error.class == ctx_pro_host_protocol::ErrorClass::Bounds
                        && request.records.len() > 1 =>
                {
                    request.records.truncate(request.records.len().div_ceil(2));
                    request.result_contents.clear();
                }
                HelperMessage::Error(error) => return Err(protocol_error(error)),
                _ => bail!("invalid_response: helper returned a non-journal response"),
            }
        };
        let acknowledged = store_checkpoint(&result.committed_through);
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
    telemetry.complete(
        batches,
        observations,
        accepted_observations,
        replayed_batches,
        initial_lag.unwrap_or(0),
    );
    Ok(MaterializeReport {
        schema_version: 1,
        payload_type: "pro_materialization",
        frontier: prior.position.sequence,
        batches,
        observations,
        replayed_batches,
        result_contents_hydrated,
        result_contents_omitted,
    })
}

fn coalesced_journal_snapshot(
    store: &Store,
    after: StoreJournalPosition,
) -> Result<ProjectionJournalSnapshot> {
    let mut snapshot = store
        .projection_journal_snapshot(Some(after))
        .context("source_unavailable: read frozen canonical projection journal page")?;
    let frozen = snapshot.frozen_through.clone();
    while snapshot.records.len() < MAX_JOURNAL_RECORDS_PER_BATCH
        && snapshot.next_position.sequence < frozen.position.sequence
    {
        let next = store
            .projection_journal_snapshot(Some(snapshot.next_position))
            .context("source_unavailable: read frozen canonical projection journal page")?;
        if next.canonical_schema_version != snapshot.canonical_schema_version
            || next.canonical_schema_identity != snapshot.canonical_schema_identity
            || next.projection_contract_version != snapshot.projection_contract_version
            || next.frozen_through.position.generation != frozen.position.generation
            || next.frozen_through.position.sequence < frozen.position.sequence
        {
            bail!("corrupt: canonical projection journal changed during a frozen page walk");
        }
        let remaining = MAX_JOURNAL_RECORDS_PER_BATCH.saturating_sub(snapshot.records.len());
        let records = next
            .records
            .into_iter()
            .take_while(|record| record.sequence <= frozen.position.sequence)
            .take(remaining)
            .collect::<Vec<_>>();
        if records.is_empty() {
            bail!("corrupt: canonical projection journal stopped before its frozen checkpoint");
        }
        snapshot.next_position = StoreJournalPosition {
            generation: records
                .last()
                .map_or(frozen.position.generation, |record| record.generation),
            sequence: records
                .last()
                .map_or(snapshot.next_position.sequence, |record| record.sequence),
        };
        snapshot.records.extend(records);
    }
    snapshot.frozen_through = frozen;
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

fn required_query_capabilities(kind: QueryKind) -> BTreeSet<Capability> {
    let mut capabilities = BTreeSet::from([kind.required_capability()]);
    if kind == QueryKind::Blame {
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
    match client.exchange(HostMessage::Status(StatusRequest {}), HANDSHAKE_TIMEOUT)? {
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
        let git_executable = git_executable()?;
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
        configure_helper_environment(&mut command, data_root, &installation_id, &git_executable)?;
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
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("helper_crashed: helper stdin is closed"))?;
        write_frame(stdin, &request).context("helper_crashed: write framed request")?;
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
        let response = read_frame::<_, HelperEnvelope>(&mut self.stdout);
        let _ = stop_tx.send(());
        let _ = watchdog.join();
        if timed_out.load(Ordering::Acquire) {
            bail!("helper_timeout: Pro helper exceeded its response deadline");
        }
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

fn protocol_error(error: ctx_pro_host_protocol::ProtocolError) -> anyhow::Error {
    let code = match error.class {
        ctx_pro_host_protocol::ErrorClass::EntitlementExpired => "entitlement_expired",
        ctx_pro_host_protocol::ErrorClass::KeyStoreUnavailable => "key_store_unavailable",
        ctx_pro_host_protocol::ErrorClass::KeyStoreLocked => "key_store_locked",
        ctx_pro_host_protocol::ErrorClass::NotMaterialized => "not_materialized",
        ctx_pro_host_protocol::ErrorClass::ProtocolMismatch => "protocol_mismatch",
        ctx_pro_host_protocol::ErrorClass::MissingSource => "source_unavailable",
        ctx_pro_host_protocol::ErrorClass::MissingRepository => "repository_unavailable",
        ctx_pro_host_protocol::ErrorClass::StaleFact => "stale_fact",
        ctx_pro_host_protocol::ErrorClass::Ambiguous => "ambiguous",
        ctx_pro_host_protocol::ErrorClass::Corrupt => "corrupt_graph",
        ctx_pro_host_protocol::ErrorClass::InvalidRequest => "invalid_request",
        ctx_pro_host_protocol::ErrorClass::Bounds => "invalid_request",
        ctx_pro_host_protocol::ErrorClass::Sequence => "invalid_response",
        ctx_pro_host_protocol::ErrorClass::Internal => "helper_crashed",
    };
    // Helper error details are untrusted and can contain local paths or key-store diagnostics.
    // The typed class is the complete stable public error contract.
    anyhow!(code)
}

pub(crate) fn stable_error_code(error: &anyhow::Error) -> Option<&'static str> {
    let text = error.to_string();
    let code = text.split(':').next().unwrap_or_default();
    match code {
        "pro_not_installed" => Some("pro_not_installed"),
        "commercial_unavailable" => Some("commercial_unavailable"),
        "helper_upgrade_required" => Some("helper_upgrade_required"),
        "entitlement_expired" => Some("entitlement_expired"),
        "key_store_unavailable" => Some("key_store_unavailable"),
        "key_store_locked" => Some("key_store_locked"),
        "not_materialized" => Some("not_materialized"),
        "needs_rebuild" => Some("needs_rebuild"),
        "partial" => Some("partial"),
        "needs_resume" => Some("needs_resume"),
        "protocol_mismatch" => Some("protocol_mismatch"),
        "source_unavailable" => Some("source_unavailable"),
        "repository_unavailable" => Some("repository_unavailable"),
        "stale_fact" => Some("stale_fact"),
        "ambiguous" => Some("ambiguous"),
        "corrupt_graph" => Some("corrupt_graph"),
        "invalid_request" => Some("invalid_request"),
        "invalid_response" => Some("invalid_response"),
        "cancelled" => Some("cancelled"),
        "helper_crashed" => Some("helper_crashed"),
        "helper_timeout" => Some("helper_timeout"),
        _ => None,
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

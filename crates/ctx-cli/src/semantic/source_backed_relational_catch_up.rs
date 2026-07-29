use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use ctx_history_core::{
    AgentType, Confidence, EventRole, EventType, Fidelity, SessionStatus, StableEntityId,
};
use ctx_history_index::{EventRecord, VerifiedIndex, MAX_SOURCE_EVENT_PAGE_ITEMS};
use ctx_history_relational::{
    CommittedCoreGeneration, RawSqlOptions, RawSqlValue, RelationalEventMetadata,
    RelationalFileTouchMetadata, RelationalProjectionError, RelationalProjectionMetadata,
    RelationalProjectionReceipt, RelationalProjectionRecord, RelationalProjectionStatus,
    RelationalSessionMetadata, RelationalSourceMetadata, SourceBackedRelationalProjection,
    RAW_SQL_MAX_ROWS_CAP,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{compact_json, source_sql::sql_compatibility_path};

use super::{
    paths_status::{daemon_jobs_path, read_daemon_job_status, write_daemon_job_status},
    source_backed_refresh_coordinator::source_backed_index_root,
};

const SOURCE_BACKED_RELATIONAL_STATUS_FILE: &str = "relational-catch-up.json";
const SOURCE_BACKED_RELATIONAL_STATUS_SCHEMA_VERSION: u16 = 1;
const CERTIFICATE_DIGEST_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceBackedRelationalCatchUpState {
    Pending,
    Error,
    Completed,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
struct SourceBackedRelationalCatchUpStatus {
    schema_version: u16,
    owner: String,
    kind: String,
    status: SourceBackedRelationalCatchUpState,
    pending: bool,
    retryable: bool,
    core_generation_id: String,
    active_core_generation_id: Option<String>,
    receipt_core_generation_id: Option<String>,
    projection_status: Option<String>,
    build_generation: Option<u64>,
    attempts: u64,
    last_attempt_at_ms: i64,
    error_code: Option<String>,
    last_error: Option<String>,
}

impl SourceBackedRelationalCatchUpStatus {
    fn pending(
        core_generation_id: &str,
        attempts: u64,
        frontier: Option<&RelationalProjectionMetadata>,
    ) -> Self {
        Self {
            schema_version: SOURCE_BACKED_RELATIONAL_STATUS_SCHEMA_VERSION,
            owner: "daemon".to_owned(),
            kind: "source_backed_relational_catch_up".to_owned(),
            status: SourceBackedRelationalCatchUpState::Pending,
            pending: true,
            retryable: true,
            core_generation_id: core_generation_id.to_owned(),
            active_core_generation_id: frontier
                .and_then(|metadata| metadata.active_core_generation_id.clone()),
            receipt_core_generation_id: None,
            projection_status: frontier.map(|metadata| status_name(metadata.status).to_owned()),
            build_generation: frontier.map(|metadata| metadata.build_generation),
            attempts,
            last_attempt_at_ms: ctx_history_core::utc_now().timestamp_millis(),
            error_code: None,
            last_error: None,
        }
    }

    fn error(
        mut self,
        error: SourceBackedRelationalCatchUpError,
        frontier: Option<&RelationalProjectionMetadata>,
    ) -> Self {
        self.status = SourceBackedRelationalCatchUpState::Error;
        self.pending = true;
        self.retryable = true;
        self.active_core_generation_id = frontier
            .and_then(|metadata| metadata.active_core_generation_id.clone())
            .or(self.active_core_generation_id);
        self.projection_status = frontier.map(|metadata| status_name(metadata.status).to_owned());
        self.build_generation = frontier.map(|metadata| metadata.build_generation);
        self.error_code = Some(error.code().to_owned());
        self.last_error = Some(error.to_string());
        self
    }

    fn completed(mut self, receipt: &RelationalProjectionReceipt) -> Self {
        self.status = SourceBackedRelationalCatchUpState::Completed;
        self.pending = false;
        self.retryable = false;
        self.active_core_generation_id = Some(receipt.core_generation_id.clone());
        self.receipt_core_generation_id = Some(receipt.core_generation_id.clone());
        self.projection_status = Some("ready".to_owned());
        self.build_generation = Some(receipt.build_generation);
        self.error_code = None;
        self.last_error = None;
        self
    }

    fn is_completed_for(&self, core_generation_id: &str) -> bool {
        self.status == SourceBackedRelationalCatchUpState::Completed
            && self.core_generation_id == core_generation_id
            && self.active_core_generation_id.as_deref() == Some(core_generation_id)
            && self.receipt_core_generation_id.as_deref() == Some(core_generation_id)
            && self.projection_status.as_deref() == Some("ready")
    }

    fn to_json(&self) -> Result<Value> {
        Ok(compact_json(serde_json::to_value(self)?))
    }
}

#[derive(Debug, Error)]
enum SourceBackedRelationalCatchUpError {
    #[error(
        "source_relational_generation_mismatch: expected Core generation {expected}, \
         but exact index carries {actual}"
    )]
    GenerationMismatch { expected: String, actual: String },
    #[error("source_relational_index_unavailable: {0}")]
    IndexUnavailable(String),
    #[error("source_relational_metadata_invalid: {0}")]
    InvalidMetadata(String),
    #[error("source_relational_projection_unavailable: {0}")]
    Projection(String),
}

impl SourceBackedRelationalCatchUpError {
    fn code(&self) -> &'static str {
        match self {
            Self::GenerationMismatch { .. } => "source_relational_generation_mismatch",
            Self::IndexUnavailable(_) => "source_relational_index_unavailable",
            Self::InvalidMetadata(_) => "source_relational_metadata_invalid",
            Self::Projection(_) => "source_relational_projection_unavailable",
        }
    }

    fn projection(error: impl std::fmt::Display) -> Self {
        Self::Projection(error.to_string())
    }
}

pub(super) struct SourceBackedRelationalCatchUpRun {
    pub(super) status: Value,
    pub(super) did_work: bool,
}

struct ProjectionOutcome {
    receipt: RelationalProjectionReceipt,
    did_work: bool,
}

pub(super) fn run_after_core_publication(
    data_root: &Path,
    core_generation_id: &str,
) -> Result<SourceBackedRelationalCatchUpRun> {
    run_with(data_root, core_generation_id, project_exact_core_generation)
}

fn run_with<Project>(
    data_root: &Path,
    core_generation_id: &str,
    project: Project,
) -> Result<SourceBackedRelationalCatchUpRun>
where
    Project: FnOnce(
        &Path,
        &str,
    )
        -> std::result::Result<ProjectionOutcome, SourceBackedRelationalCatchUpError>,
{
    let prior = read_status(data_root);
    if prior
        .as_ref()
        .is_some_and(|status| status.is_completed_for(core_generation_id))
        && ready_projection_metadata(data_root, core_generation_id).is_some()
    {
        return Ok(SourceBackedRelationalCatchUpRun {
            status: prior.expect("checked above").to_json()?,
            did_work: false,
        });
    }

    let frontier = projection_metadata(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let pending = SourceBackedRelationalCatchUpStatus::pending(
        core_generation_id,
        attempts,
        frontier.as_ref(),
    );
    persist_status(data_root, &pending)?;

    match project(data_root, core_generation_id) {
        Ok(outcome) => {
            let completed = pending.completed(&outcome.receipt);
            persist_status(data_root, &completed)?;
            Ok(SourceBackedRelationalCatchUpRun {
                status: completed.to_json()?,
                did_work: outcome.did_work,
            })
        }
        Err(error) => {
            let frontier = projection_metadata(data_root);
            let failed = pending.error(error, frontier.as_ref());
            persist_status(data_root, &failed)?;
            Ok(SourceBackedRelationalCatchUpRun {
                status: failed.to_json()?,
                did_work: false,
            })
        }
    }
}

fn project_exact_core_generation(
    data_root: &Path,
    core_generation_id: &str,
) -> std::result::Result<ProjectionOutcome, SourceBackedRelationalCatchUpError> {
    let index_root = source_backed_index_root(data_root);
    let index = VerifiedIndex::open(&index_root).map_err(|error| {
        SourceBackedRelationalCatchUpError::IndexUnavailable(format!(
            "open verified source-backed index {}: {error}",
            index_root.display()
        ))
    })?;
    if index.generation_id() != core_generation_id {
        return Err(SourceBackedRelationalCatchUpError::GenerationMismatch {
            expected: core_generation_id.to_owned(),
            actual: index.generation_id().to_owned(),
        });
    }

    let generation = committed_generation(&index)?;
    let projection_path = sql_compatibility_path(data_root);
    let (mut projection, reset) = open_disposable_projection(&projection_path)?;
    let metadata = projection
        .metadata()
        .map_err(SourceBackedRelationalCatchUpError::projection)?;
    if metadata.status == RelationalProjectionStatus::Ready
        && metadata.active_core_generation_id.as_deref() == Some(core_generation_id)
    {
        return Ok(ProjectionOutcome {
            receipt: receipt_from_metadata(core_generation_id, &metadata),
            did_work: false,
        });
    }

    let rebuild = reset || metadata.status == RelationalProjectionStatus::Empty;
    let selected_sources = if rebuild {
        index
            .manifest()
            .sources
            .iter()
            .map(|source| source.observation().source().identity().as_uuid())
            .collect()
    } else {
        changed_source_ids(&projection, &index)?
    };
    let records = relational_records(&index, &selected_sources)?;
    let receipt = if rebuild {
        projection.rebuild(&generation, records)
    } else {
        projection.catch_up(&generation, records)
    }
    .map_err(SourceBackedRelationalCatchUpError::projection)?;
    if receipt.core_generation_id != core_generation_id {
        return Err(SourceBackedRelationalCatchUpError::GenerationMismatch {
            expected: core_generation_id.to_owned(),
            actual: receipt.core_generation_id,
        });
    }
    Ok(ProjectionOutcome {
        receipt,
        did_work: true,
    })
}

fn committed_generation(
    index: &VerifiedIndex,
) -> std::result::Result<CommittedCoreGeneration, SourceBackedRelationalCatchUpError> {
    let manifest = index.manifest();
    let manifest_json = serde_json::to_vec(manifest).map_err(|error| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
            "serialize exact generation manifest: {error}"
        ))
    })?;
    Ok(CommittedCoreGeneration {
        generation_id: index.generation_id().to_owned(),
        manifest_json,
        indexed_documents: manifest.indexed_documents,
        certified_sources: manifest.sources.len(),
        certified_source_bytes: manifest.certified_source_bytes,
    })
}

fn open_disposable_projection(
    path: &Path,
) -> std::result::Result<(SourceBackedRelationalProjection, bool), SourceBackedRelationalCatchUpError>
{
    match SourceBackedRelationalProjection::open(path) {
        Ok(projection) => Ok((projection, false)),
        Err(
            RelationalProjectionError::MissingSchema
            | RelationalProjectionError::UnsupportedSchema { .. }
            | RelationalProjectionError::IncompatibleState(_),
        ) => {
            reset_disposable_projection(path)?;
            SourceBackedRelationalProjection::open(path)
                .map(|projection| (projection, true))
                .map_err(SourceBackedRelationalCatchUpError::projection)
        }
        Err(error) => Err(SourceBackedRelationalCatchUpError::projection(error)),
    }
}

fn reset_disposable_projection(
    path: &Path,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(SourceBackedRelationalCatchUpError::Projection(format!(
                    "reset disposable projection {}: {error}",
                    candidate.display()
                )))
            }
        }
    }
    Ok(())
}

fn changed_source_ids(
    projection: &SourceBackedRelationalProjection,
    index: &VerifiedIndex,
) -> std::result::Result<BTreeSet<Uuid>, SourceBackedRelationalCatchUpError> {
    let prior = stored_certificate_digests(projection)?;
    let mut changed = BTreeSet::new();
    for certificate in &index.manifest().sources {
        let certificate_json = serde_json::to_vec(certificate).map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "serialize source certificate: {error}"
            ))
        })?;
        let digest: [u8; CERTIFICATE_DIGEST_BYTES] = Sha256::digest(certificate_json).into();
        let source_id = certificate.observation().source().identity().as_uuid();
        if prior.get(&source_id) != Some(&digest) {
            changed.insert(source_id);
        }
    }
    Ok(changed)
}

fn stored_certificate_digests(
    projection: &SourceBackedRelationalProjection,
) -> std::result::Result<
    BTreeMap<Uuid, [u8; CERTIFICATE_DIGEST_BYTES]>,
    SourceBackedRelationalCatchUpError,
> {
    let mut output = BTreeMap::new();
    let mut after: Option<Uuid> = None;
    loop {
        let sql = match after {
            Some(source_id) => format!(
                "SELECT source_id, certificate_digest FROM source_backed_sources \
                 WHERE source_id > '{source_id}' ORDER BY source_id LIMIT {RAW_SQL_MAX_ROWS_CAP}"
            ),
            None => format!(
                "SELECT source_id, certificate_digest FROM source_backed_sources \
                 ORDER BY source_id LIMIT {RAW_SQL_MAX_ROWS_CAP}"
            ),
        };
        let result = projection
            .raw_sql_query(
                &sql,
                RawSqlOptions {
                    max_rows: RAW_SQL_MAX_ROWS_CAP,
                    max_value_bytes: 64,
                    ..RawSqlOptions::default()
                },
            )
            .map_err(SourceBackedRelationalCatchUpError::projection)?;
        let returned = result.rows.len();
        for row in result.rows {
            let [source_value, digest_value]: [RawSqlValue; 2] = row.try_into().map_err(|_| {
                SourceBackedRelationalCatchUpError::InvalidMetadata(
                    "stored certificate query returned the wrong column count".to_owned(),
                )
            })?;
            let RawSqlValue::Text {
                value: source_id,
                truncated: false,
                ..
            } = source_value
            else {
                return Err(SourceBackedRelationalCatchUpError::InvalidMetadata(
                    "stored source identity is not complete text".to_owned(),
                ));
            };
            let source_id = Uuid::parse_str(&source_id).map_err(|error| {
                SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                    "stored source identity is invalid: {error}"
                ))
            })?;
            let RawSqlValue::Blob {
                bytes: CERTIFICATE_DIGEST_BYTES,
                preview_hex,
                truncated: false,
            } = digest_value
            else {
                return Err(SourceBackedRelationalCatchUpError::InvalidMetadata(
                    "stored certificate digest is malformed".to_owned(),
                ));
            };
            let digest = decode_digest(&preview_hex)?;
            output.insert(source_id, digest);
            after = Some(source_id);
        }
        if returned < RAW_SQL_MAX_ROWS_CAP {
            break;
        }
    }
    Ok(output)
}

fn decode_digest(
    value: &str,
) -> std::result::Result<[u8; CERTIFICATE_DIGEST_BYTES], SourceBackedRelationalCatchUpError> {
    if value.len() != CERTIFICATE_DIGEST_BYTES * 2 {
        return Err(SourceBackedRelationalCatchUpError::InvalidMetadata(
            "stored certificate digest has the wrong length".to_owned(),
        ));
    }
    let mut output = [0_u8; CERTIFICATE_DIGEST_BYTES];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let digits = std::str::from_utf8(chunk).map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "stored certificate digest is not UTF-8 hex: {error}"
            ))
        })?;
        output[index] = u8::from_str_radix(digits, 16).map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "stored certificate digest is not hex: {error}"
            ))
        })?;
    }
    Ok(output)
}

fn relational_records(
    index: &VerifiedIndex,
    selected_sources: &BTreeSet<Uuid>,
) -> std::result::Result<Vec<RelationalProjectionRecord>, SourceBackedRelationalCatchUpError> {
    let mut output = Vec::new();
    for certificate in &index.manifest().sources {
        let source = certificate.observation().source();
        if !selected_sources.contains(&source.identity().as_uuid()) {
            continue;
        }
        let events = source_events(index, source)?;
        let first = events.first();
        output.push(RelationalProjectionRecord::BeginSource(
            RelationalSourceMetadata {
                source: source.clone(),
                source_root: first
                    .and_then(|event| event.source_path.as_deref())
                    .and_then(|path| Path::new(path).parent())
                    .map(|path| path.to_string_lossy().into_owned())
                    .filter(|path| !path.is_empty()),
                source_path: first.and_then(|event| event.source_path.clone()),
                cwd: first.and_then(|event| event.cwd.clone()),
            },
        ));

        let mut sessions = BTreeMap::<Uuid, SessionAggregate>::new();
        for event in &events {
            sessions
                .entry(event.session_id.as_uuid())
                .and_modify(|session| session.observe(event))
                .or_insert_with(|| SessionAggregate::new(event));
        }
        for session in sessions.into_values() {
            output.push(RelationalProjectionRecord::Session(
                session.into_metadata()?,
            ));
        }
        for event in events {
            let event_type = event.event_type.parse::<EventType>().map_err(|error| {
                SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                    "invalid event type {:?}: {error}",
                    event.event_type
                ))
            })?;
            let role = event
                .role
                .as_deref()
                .map(str::parse::<EventRole>)
                .transpose()
                .map_err(|error| {
                    SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                        "invalid event role for {}: {error}",
                        event.event_id
                    ))
                })?;
            output.push(RelationalProjectionRecord::Event(RelationalEventMetadata {
                event_id: event.event_id,
                session_id: event.session_id,
                event_sequence: event.event_sequence,
                event_type,
                role,
                occurred_at_unix_ms: event.occurred_at_unix_ms,
                fidelity: Fidelity::Imported,
                locator: event.locator.clone(),
            }));
            for (ordinal, path) in event.touched_files.iter().enumerate() {
                output.push(RelationalProjectionRecord::FileTouch(
                    RelationalFileTouchMetadata {
                        file_touch_id: file_touch_id(event.event_id, ordinal, path)?,
                        event_id: Some(event.event_id),
                        session_id: Some(event.session_id),
                        path: path.clone(),
                        old_path: None,
                        change_kind: None,
                        line_count_delta: None,
                        confidence: Confidence::Explicit,
                        created_at_unix_ms: event.occurred_at_unix_ms,
                        updated_at_unix_ms: event.occurred_at_unix_ms,
                    },
                ));
            }
        }
        output.push(RelationalProjectionRecord::EndSource {
            source_id: source.identity().as_uuid(),
        });
    }
    Ok(output)
}

fn source_events(
    index: &VerifiedIndex,
    source: &ctx_history_core::SourceKey,
) -> std::result::Result<Vec<EventRecord>, SourceBackedRelationalCatchUpError> {
    let mut output = Vec::new();
    let mut cursor = None;
    loop {
        let page = index
            .source_event_page(source, cursor.as_ref(), MAX_SOURCE_EVENT_PAGE_ITEMS)
            .map_err(|error| {
                SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                    "enumerate exact source {}: {error}",
                    source.identity()
                ))
            })?;
        if page.generation_id != index.generation_id() || !page.source.exact_descriptor_eq(source) {
            return Err(SourceBackedRelationalCatchUpError::GenerationMismatch {
                expected: index.generation_id().to_owned(),
                actual: page.generation_id,
            });
        }
        output.extend(page.items);
        if page.terminal {
            break;
        }
        cursor = page.next_cursor;
    }
    Ok(output)
}

struct SessionAggregate {
    first: EventRecord,
    started_at_unix_ms: Option<i64>,
    ended_at_unix_ms: Option<i64>,
}

impl SessionAggregate {
    fn new(event: &EventRecord) -> Self {
        Self {
            first: event.clone(),
            started_at_unix_ms: event.occurred_at_unix_ms,
            ended_at_unix_ms: event.occurred_at_unix_ms,
        }
    }

    fn observe(&mut self, event: &EventRecord) {
        if event.event_sequence < self.first.event_sequence {
            self.first = event.clone();
        }
        self.started_at_unix_ms = option_min(self.started_at_unix_ms, event.occurred_at_unix_ms);
        self.ended_at_unix_ms = option_max(self.ended_at_unix_ms, event.occurred_at_unix_ms);
    }

    fn into_metadata(
        self,
    ) -> std::result::Result<RelationalSessionMetadata, SourceBackedRelationalCatchUpError> {
        let agent_type = self
            .first
            .agent_type
            .parse::<AgentType>()
            .map_err(|error| {
                SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                    "invalid agent type {:?}: {error}",
                    self.first.agent_type
                ))
            })?;
        Ok(RelationalSessionMetadata {
            session_id: self.first.session_id,
            parent_session_id: self.first.parent_session_id,
            root_session_id: self.first.root_session_id,
            provider_session_id: self.first.provider_session_id,
            external_agent_id: None,
            agent_type,
            role_hint: None,
            is_primary: self.first.is_primary,
            branch: self.first.branch,
            workspace: self.first.workspace,
            cwd: self.first.cwd,
            source_path: self.first.source_path,
            status: SessionStatus::Imported,
            fidelity: Fidelity::Imported,
            started_at_unix_ms: self.started_at_unix_ms,
            ended_at_unix_ms: self.ended_at_unix_ms,
        })
    }
}

fn option_min(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn option_max(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn file_touch_id(
    event_id: StableEntityId,
    ordinal: usize,
    path: &str,
) -> std::result::Result<Uuid, SourceBackedRelationalCatchUpError> {
    let identity = event_id.encode_canonical().map_err(|error| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
            "encode file-touch event identity: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-source-relational-file-touch-v1\0");
    hasher.update(identity);
    hasher.update((ordinal as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes))
}

fn receipt_from_metadata(
    core_generation_id: &str,
    metadata: &RelationalProjectionMetadata,
) -> RelationalProjectionReceipt {
    RelationalProjectionReceipt {
        core_generation_id: core_generation_id.to_owned(),
        build_generation: metadata.build_generation,
        source_count: metadata.source_count,
        session_count: metadata.session_count,
        event_count: metadata.event_count,
        file_touch_count: metadata.file_touch_count,
    }
}

fn status_name(status: RelationalProjectionStatus) -> &'static str {
    match status {
        RelationalProjectionStatus::Empty => "empty",
        RelationalProjectionStatus::Ready => "ready",
        RelationalProjectionStatus::Behind => "behind",
    }
}

fn projection_metadata(data_root: &Path) -> Option<RelationalProjectionMetadata> {
    SourceBackedRelationalProjection::open_read_only(sql_compatibility_path(data_root))
        .ok()
        .and_then(|projection| projection.metadata().ok())
}

fn ready_projection_metadata(
    data_root: &Path,
    core_generation_id: &str,
) -> Option<RelationalProjectionMetadata> {
    projection_metadata(data_root).filter(|metadata| {
        metadata.status == RelationalProjectionStatus::Ready
            && metadata.active_core_generation_id.as_deref() == Some(core_generation_id)
            && metadata.target_core_generation_id.is_none()
    })
}

fn next_attempt(
    prior: Option<&SourceBackedRelationalCatchUpStatus>,
    core_generation_id: &str,
) -> u64 {
    prior
        .filter(|status| status.core_generation_id == core_generation_id)
        .map(|status| status.attempts.saturating_add(1))
        .unwrap_or(1)
}

fn status_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(SOURCE_BACKED_RELATIONAL_STATUS_FILE)
}

fn read_status(data_root: &Path) -> Option<SourceBackedRelationalCatchUpStatus> {
    read_daemon_job_status(&status_path(data_root))
        .and_then(|value| serde_json::from_value(value).ok())
}

fn persist_status(data_root: &Path, status: &SourceBackedRelationalCatchUpStatus) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), &status.to_json()?)
}

#[cfg(test)]
#[path = "source_backed_relational_catch_up_tests.rs"]
mod tests;

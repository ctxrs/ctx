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
use ctx_history_search::sql_compatibility_path;
use ctx_history_store::{
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

use crate::compact_json;

use super::{
    paths_status::{daemon_jobs_path, read_daemon_job_status, write_daemon_job_status},
    source_backed_refresh_coordinator::source_backed_index_root,
};

const SOURCE_BACKED_RELATIONAL_STATUS_FILE: &str = "source-backed-relational-catch-up.json";
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
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend,
        CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput,
        LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
        ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
        SourceInventoryObservation, SourceObservation, SourceRecordLocator, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
    use ctx_history_search::SqlCompatibility;
    use ctx_history_store::{RawSqlValue, RelationalProjectionStatus};

    use super::*;

    const PROVIDER_TEXT: &str = "provider-body-sentinel-must-not-enter-relational";
    const PREVIEW_TEXT: &str = "provider-preview-sentinel-must-not-enter-relational";

    fn source() -> ctx_history_core::SourceKey {
        ctx_history_core::SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::provider_native(
                "session-file",
                TypedKey::utf8("relational-production-writer.jsonl").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn appendable_certificate(
        source: &ctx_history_core::SourceKey,
        revision: u8,
        documents: u64,
        bytes: u64,
    ) -> CertifiedSource {
        let observation =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
        CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation,
            "codex-parser-v1",
            [revision; 32],
            ScannedSourceCounts {
                complete_records: documents,
                retained_records: documents,
                indexed_documents: documents,
                certified_bytes: bytes,
                ..ScannedSourceCounts::default()
            },
            Some(
                SourceFrontier::new(
                    "jsonl-byte-offset",
                    TypedKey::U64(bytes),
                    bytes,
                    [revision; 32],
                )
                .unwrap(),
            ),
        )
        .unwrap()
    }

    fn document(
        source: &ctx_history_core::SourceKey,
        sequence: u64,
        role: &str,
        touched_files: &[&str],
    ) -> LexicalDocument {
        let native_session = TypedKey::utf8("provider-session").unwrap();
        let session_key = NativeSessionKey::native_id("session", native_session.clone()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let native_item = NativeItemKey::native_id(
            "message",
            TypedKey::utf8(format!("event-{sequence}")).unwrap(),
        )
        .unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item,
            subrecord_selector: None,
        })
        .unwrap();
        LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source: source.clone(),
            locator: SourceRecordLocator::new(
                source.clone(),
                NativeRecordCoordinate::Jsonl {
                    byte_offset: sequence * 100,
                    byte_length: 100,
                    physical_ordinal: sequence,
                    native_session_key: Some(native_session),
                    native_event_key: Some(TypedKey::U64(sequence)),
                },
                LocatorRevisionPolicy::StableRecordEvidence,
                None,
                [sequence as u8; 32],
            )
            .unwrap(),
            provider_session_id: Some("provider-session".to_owned()),
            branch: Some("main".to_owned()),
            source_path: Some("/provider/codex/session.jsonl".to_owned()),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
            event_type: "message".to_owned(),
            role: Some(role.to_owned()),
            body: format!("{PROVIDER_TEXT} {PREVIEW_TEXT} sequence-{sequence}"),
            workspace: Some("ctx".to_owned()),
            cwd: Some("/work/ctx".to_owned()),
            touched_files: touched_files
                .iter()
                .map(|path| (*path).to_owned())
                .collect(),
        }
    }

    fn replace_generation(
        data_root: &Path,
        source: &ctx_history_core::SourceKey,
        revision: u8,
        documents: Vec<LexicalDocument>,
    ) -> String {
        let document_count = documents.len() as u64;
        let mut writer = GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap();
        writer.begin_source(source.clone()).unwrap();
        for document in documents {
            writer.add_document(document).unwrap();
        }
        writer
            .certify_source(appendable_certificate(
                source,
                revision,
                document_count,
                document_count * 100,
            ))
            .unwrap();
        writer.commit(|_| true).unwrap().generation_id
    }

    fn initial_generation(data_root: &Path, source: &ctx_history_core::SourceKey) -> String {
        replace_generation(
            data_root,
            source,
            1,
            vec![document(source, 1, "user", &["src/lib.rs"])],
        )
    }

    fn append_generation(data_root: &Path, source: &ctx_history_core::SourceKey) -> String {
        let mut writer = GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap();
        let base = writer.begin_source_append(source.clone()).unwrap().clone();
        writer
            .add_document(document(
                source,
                2,
                "assistant",
                &["src/lib.rs", "src/main.rs"],
            ))
            .unwrap();
        let current = appendable_certificate(source, 2, 2, 200);
        writer
            .certify_source_append(
                CertifiedSourceAppend::certify(&base, current, 100, [1; 32]).unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap().generation_id
    }

    fn delete_generation(data_root: &Path, source: &ctx_history_core::SourceKey) -> String {
        let observation = SourceInventoryObservation::new(
            source.provider(),
            "provider-root",
            TypedKey::utf8("root-lineage").unwrap(),
            "tree-inventory-v1",
            vec![4],
        )
        .unwrap();
        let inventory = CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "discovery-v1",
            vec![],
        )
        .unwrap();
        let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
        let mut writer = GenerationWriter::open(
            source_backed_index_root(data_root),
            WriterOptions::default(),
        )
        .unwrap();
        writer.delete_source(deletion, inventory).unwrap();
        writer.commit(|_| true).unwrap().generation_id
    }

    fn query(data_root: &Path, sql: &str) -> Vec<Vec<RawSqlValue>> {
        SqlCompatibility::open_for_data_root(data_root)
            .unwrap()
            .query(sql, RawSqlOptions::default())
            .unwrap()
            .rows
    }

    fn projection_bytes(data_root: &Path) -> Vec<u8> {
        let path = sql_compatibility_path(data_root);
        let mut output = fs::read(&path).unwrap();
        for suffix in ["-wal", "-shm"] {
            if let Ok(bytes) = fs::read(format!("{}{suffix}", path.display())) {
                output.extend(bytes);
            }
        }
        output
    }

    fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|candidate| candidate == needle.as_bytes())
    }

    #[test]
    fn cold_append_rewrite_delete_and_noop_preserve_only_relational_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let source = source();
        let first_generation = initial_generation(temp.path(), &source);

        let cold = run_after_core_publication(temp.path(), &first_generation).unwrap();
        assert!(cold.did_work);
        assert_eq!(cold.status["status"], "completed");
        assert_eq!(cold.status["core_generation_id"], first_generation);
        assert_eq!(cold.status["receipt_core_generation_id"], first_generation);

        let metadata = SqlCompatibility::open_for_data_root(temp.path())
            .unwrap()
            .metadata()
            .unwrap();
        let index = VerifiedIndex::open(source_backed_index_root(temp.path())).unwrap();
        assert_eq!(metadata.status, RelationalProjectionStatus::Ready);
        assert_eq!(
            metadata.active_core_generation_id.as_deref(),
            Some(first_generation.as_str())
        );
        assert_eq!(metadata.active_manifest_version, Some(3));
        assert_eq!(metadata.active_lexical_schema_version, Some(5));
        assert_eq!(
            metadata.active_policy_schema_hash.as_deref(),
            Some(index.manifest().policy_schema_hash.as_str())
        );
        assert_eq!((metadata.source_count, metadata.session_count), (1, 1));
        assert_eq!((metadata.event_count, metadata.file_touch_count), (1, 1));

        let rows = query(
            temp.path(),
            "SELECT provider, provider_session_id, agent_type, branch, workspace, cwd,
                    source_path, event_type, role, event_seq, payload_json,
                    native_locator_json
             FROM ctx_events",
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0][0],
            RawSqlValue::Text { value, .. } if value == "codex"
        ));
        assert!(matches!(
            &rows[0][8],
            RawSqlValue::Text { value, .. } if value == "user"
        ));
        assert!(matches!(rows[0][9], RawSqlValue::Integer(1)));
        assert!(matches!(
            &rows[0][10],
            RawSqlValue::Text { value, .. } if value == r#"{"content_authority":"provider_source"}"#
        ));
        assert!(matches!(
            &rows[0][11],
            RawSqlValue::Blob { bytes, truncated: false, .. } if *bytes > 0
        ));
        let bytes = projection_bytes(temp.path());
        assert!(!contains_bytes(&bytes, PROVIDER_TEXT));
        assert!(!contains_bytes(&bytes, PREVIEW_TEXT));

        let build_generation = metadata.build_generation;
        let noop = run_after_core_publication(temp.path(), &first_generation).unwrap();
        assert!(!noop.did_work);
        assert_eq!(
            SqlCompatibility::open_for_data_root(temp.path())
                .unwrap()
                .metadata()
                .unwrap()
                .build_generation,
            build_generation
        );

        let appended_generation = append_generation(temp.path(), &source);
        let appended = run_after_core_publication(temp.path(), &appended_generation).unwrap();
        assert!(appended.did_work);
        assert_eq!(
            query(temp.path(), "SELECT COUNT(*) FROM ctx_events")[0][0],
            RawSqlValue::Integer(2)
        );
        assert_eq!(
            query(temp.path(), "SELECT COUNT(*) FROM ctx_files_touched")[0][0],
            RawSqlValue::Integer(3)
        );

        let rewritten_generation = replace_generation(
            temp.path(),
            &source,
            3,
            vec![document(&source, 3, "tool", &["README.md"])],
        );
        let rewritten = run_after_core_publication(temp.path(), &rewritten_generation).unwrap();
        assert!(rewritten.did_work);
        assert_eq!(
            query(temp.path(), "SELECT event_seq FROM ctx_events"),
            vec![vec![RawSqlValue::Integer(3)]]
        );
        assert_eq!(
            query(temp.path(), "SELECT path FROM ctx_files_touched"),
            vec![vec![RawSqlValue::Text {
                value: "README.md".to_owned(),
                bytes: "README.md".len(),
                truncated: false,
            }]]
        );
        let bytes = projection_bytes(temp.path());
        assert!(!contains_bytes(&bytes, PROVIDER_TEXT));
        assert!(!contains_bytes(&bytes, PREVIEW_TEXT));

        let deleted_generation = delete_generation(temp.path(), &source);
        let deleted = run_after_core_publication(temp.path(), &deleted_generation).unwrap();
        assert!(deleted.did_work);
        let metadata = SqlCompatibility::open_for_data_root(temp.path())
            .unwrap()
            .metadata()
            .unwrap();
        assert_eq!(
            metadata.active_core_generation_id.as_deref(),
            Some(deleted_generation.as_str())
        );
        assert_eq!(
            (
                metadata.source_count,
                metadata.session_count,
                metadata.event_count,
                metadata.file_touch_count,
            ),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn failed_catch_up_keeps_prior_generation_and_a_later_tick_retries() {
        let temp = tempfile::tempdir().unwrap();
        let source = source();
        let first_generation = initial_generation(temp.path(), &source);
        run_after_core_publication(temp.path(), &first_generation).unwrap();
        let appended_generation = append_generation(temp.path(), &source);

        let interrupted = SourceBackedRelationalCatchUpStatus::pending(
            &appended_generation,
            1,
            projection_metadata(temp.path()).as_ref(),
        );
        persist_status(temp.path(), &interrupted).unwrap();

        let failed = run_with(
            temp.path(),
            &appended_generation,
            |data_root, generation_id| {
                let index =
                    VerifiedIndex::open(source_backed_index_root(data_root)).map_err(|error| {
                        SourceBackedRelationalCatchUpError::IndexUnavailable(error.to_string())
                    })?;
                let generation = committed_generation(&index)?;
                let mut projection =
                    SourceBackedRelationalProjection::open(sql_compatibility_path(data_root))
                        .map_err(SourceBackedRelationalCatchUpError::projection)?;
                let error = projection
                    .catch_up(&generation, Vec::<RelationalProjectionRecord>::new())
                    .expect_err("changed source must be present");
                assert_eq!(generation_id, generation.generation_id);
                Err(SourceBackedRelationalCatchUpError::projection(error))
            },
        )
        .unwrap();
        assert!(!failed.did_work);
        assert_eq!(failed.status["status"], "error");
        assert_eq!(failed.status["pending"], true);
        assert_eq!(failed.status["retryable"], true);
        assert_eq!(failed.status["attempts"], 2);
        assert_eq!(failed.status["active_core_generation_id"], first_generation);
        assert_eq!(failed.status["core_generation_id"], appended_generation);
        assert_eq!(failed.status["projection_status"], "behind");

        let prior = SqlCompatibility::open_for_data_root(temp.path())
            .unwrap()
            .metadata()
            .unwrap();
        assert_eq!(
            prior.active_core_generation_id.as_deref(),
            Some(first_generation.as_str())
        );
        assert_eq!(
            prior.target_core_generation_id,
            Some(appended_generation.clone())
        );
        assert_eq!(prior.status, RelationalProjectionStatus::Behind);
        let prior_projection =
            SourceBackedRelationalProjection::open_read_only(sql_compatibility_path(temp.path()))
                .unwrap();
        assert_eq!(
            prior_projection
                .raw_sql_query("SELECT COUNT(*) FROM ctx_events", RawSqlOptions::default())
                .unwrap()
                .rows[0][0],
            RawSqlValue::Integer(1)
        );

        let retried = run_after_core_publication(temp.path(), &appended_generation).unwrap();
        assert!(retried.did_work);
        assert_eq!(retried.status["status"], "completed");
        assert_eq!(retried.status["attempts"], 3);
        assert_eq!(
            retried.status["active_core_generation_id"],
            appended_generation
        );
        let ready = SqlCompatibility::open_for_data_root(temp.path())
            .unwrap()
            .metadata()
            .unwrap();
        assert_eq!(ready.status, RelationalProjectionStatus::Ready);
        assert_eq!(ready.target_core_generation_id, None);
        assert_eq!(
            query(temp.path(), "SELECT COUNT(*) FROM ctx_events")[0][0],
            RawSqlValue::Integer(2)
        );
    }

    #[test]
    fn generation_mismatch_is_persistent_and_never_creates_a_fallback_database() {
        let temp = tempfile::tempdir().unwrap();
        let source = source();
        let generation = initial_generation(temp.path(), &source);
        let wrong_generation = "f".repeat(64);
        assert_ne!(wrong_generation, generation);

        let run = run_after_core_publication(temp.path(), &wrong_generation).unwrap();

        assert!(!run.did_work);
        assert_eq!(run.status["status"], "error");
        assert_eq!(
            run.status["error_code"],
            "source_relational_generation_mismatch"
        );
        assert_eq!(run.status["core_generation_id"], wrong_generation);
        assert!(!sql_compatibility_path(temp.path()).exists());
    }

    #[test]
    fn materializer_has_no_provider_hydration_or_legacy_store_authority() {
        let source = include_str!("source_backed_relational_catch_up.rs");
        for forbidden in [
            ["database_", "path"].concat(),
            ["work", ".sqlite"].concat(),
            ["SourceBacked", "ResolverRegistry"].concat(),
            ["provider_", "bytes"].concat(),
            ["bounded_", "preview"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "relational catch-up contains forbidden architecture term {forbidden}"
            );
        }
        assert!(source.contains("VerifiedIndex::open"));
        assert!(source.contains(".source_event_page("));
        assert!(source.contains("SourceBackedRelationalProjection::open"));
    }
}

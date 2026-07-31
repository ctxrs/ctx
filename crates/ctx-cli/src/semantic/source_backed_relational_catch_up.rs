use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use ctx_history_core::SourceKey;
use ctx_history_index::{
    EventRecord, SourceEventCursor, SourceEventPage, VerifiedIndex, MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use ctx_history_relational::{
    CommittedCoreGeneration, RawSqlOptions, RawSqlValue, RelationalProjectionError,
    RelationalProjectionMetadata, RelationalProjectionReceipt, RelationalProjectionRecord,
    RelationalProjectionStatus, RelationalSessionMetadata, RelationalSourceMetadata,
    SourceBackedRelationalProjection, RAW_SQL_MAX_ROWS_CAP,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::source_sql::sql_compatibility_path;

use super::{
    paths_status::{
        daemon_jobs_path, daemon_report, read_daemon_job_status, write_daemon_job_status,
    },
    source_backed_refresh_coordinator::{
        daemon_cycle_verified_index, nonzero_duration_micros, open_verified_index,
        source_backed_index_root,
    },
};

const SOURCE_BACKED_RELATIONAL_STATUS_FILE: &str = "relational-catch-up.json";
const CERTIFICATE_DIGEST_BYTES: usize = 32;
const SOURCE_BACKED_RELATIONAL_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOURCE_BACKED_RELATIONAL_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

mod record_metadata;
mod status;

use record_metadata::{records_for_event, SessionAggregate, SourceMetadataSeed};
use status::SourceBackedRelationalCatchUpStatus;

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

pub(super) fn generation_needs_catch_up(data_root: &Path, core_generation_id: &str) -> bool {
    !read_status(data_root).is_some_and(|status| status.is_completed_for(core_generation_id))
        || ready_projection_metadata(data_root, core_generation_id).is_none()
}

pub(super) fn status_generation(data_root: &Path) -> Option<String> {
    read_status(data_root).map(|status| status.core_generation_id)
}

/// Waits for the daemon-owned relational projection of one exact Core generation.
///
/// Import uses this read-only observation seam after lexical publication. Pro and
/// semantic projections remain independently scheduled and never extend the
/// foreground import boundary.
pub(crate) fn wait_for_completed_generation(
    data_root: &Path,
    core_generation_id: &str,
    fail_if_daemon_unavailable: bool,
) -> Result<()> {
    wait_for_completed_generation_with(
        data_root,
        core_generation_id,
        fail_if_daemon_unavailable,
        SOURCE_BACKED_RELATIONAL_WAIT_TIMEOUT,
        || thread::sleep(SOURCE_BACKED_RELATIONAL_POLL_INTERVAL),
    )
}

fn wait_for_completed_generation_with(
    data_root: &Path,
    core_generation_id: &str,
    fail_if_daemon_unavailable: bool,
    timeout: Duration,
    mut wait: impl FnMut(),
) -> Result<()> {
    let started = Instant::now();
    loop {
        if let Some(status) = read_status(data_root) {
            if status.is_completed_for(core_generation_id)
                && ready_projection_metadata(data_root, core_generation_id).is_some()
            {
                return Ok(());
            }
            if status.core_generation_id == core_generation_id
                && status.status == status::SourceBackedRelationalCatchUpState::Error
            {
                let code = status
                    .error_code
                    .as_deref()
                    .unwrap_or("source_relational_projection_unavailable");
                let detail = status
                    .last_error
                    .as_deref()
                    .unwrap_or("daemon source-backed relational catch-up failed");
                anyhow::bail!("{code}: {detail}");
            }
        }
        if fail_if_daemon_unavailable {
            let daemon = daemon_report(data_root);
            let owns_relational_catch_up = daemon.get("running").and_then(Value::as_bool)
                == Some(true)
                && daemon.get("mode").and_then(Value::as_str) == Some("full");
            if !owns_relational_catch_up {
                anyhow::bail!(
                    "the ctx daemon is unavailable for required relational catch-up; no foreground writer was started"
                );
            }
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "source_relational_projection_unavailable: timed out waiting for daemon relational generation {core_generation_id}"
            );
        }
        wait();
    }
}

pub(super) fn read_status_json(data_root: &Path) -> Option<Value> {
    read_daemon_job_status(&status_path(data_root))
}

pub(super) fn persist_status_json(data_root: &Path, status: &Value) -> Result<()> {
    write_daemon_job_status(&status_path(data_root), status)
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
    let attempt_started = Instant::now();
    let pending = SourceBackedRelationalCatchUpStatus::pending(
        core_generation_id,
        attempts,
        frontier.as_ref(),
    );
    persist_status(data_root, &pending)?;

    match project(data_root, core_generation_id) {
        Ok(outcome) => {
            let completed = pending
                .completed(&outcome.receipt)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &completed)?;
            Ok(SourceBackedRelationalCatchUpRun {
                status: completed.to_json()?,
                did_work: outcome.did_work,
            })
        }
        Err(error) => {
            let frontier = projection_metadata(data_root);
            let failed = pending
                .error(error, frontier.as_ref())
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
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
    let index = match daemon_cycle_verified_index(data_root, core_generation_id) {
        Some(index) => index,
        None => Arc::new(open_verified_index(&index_root).map_err(|error| {
            SourceBackedRelationalCatchUpError::IndexUnavailable(format!(
                "open verified source-backed index {}: {error}",
                index_root.display()
            ))
        })?),
    };
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
    let changed_sources = (!rebuild)
        .then(|| changed_source_ids(&projection, &index))
        .transpose()?;
    let selection = changed_sources
        .as_ref()
        .map_or(RelationalSourceSelection::All, |sources| {
            RelationalSourceSelection::Changed(sources)
        });
    let records = RelationalRecordStream::new(&index, selection, MAX_SOURCE_EVENT_PAGE_ITEMS);
    let receipt = if rebuild {
        projection.rebuild_stream(&generation, records)
    } else {
        projection.catch_up_stream(&generation, records)
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

#[derive(Clone, Copy)]
enum RelationalSourceSelection<'a> {
    All,
    Changed(&'a BTreeSet<Uuid>),
}

impl RelationalSourceSelection<'_> {
    fn includes(self, source_id: Uuid) -> bool {
        match self {
            Self::All => true,
            Self::Changed(sources) => sources.contains(&source_id),
        }
    }
}

struct RelationalRecordStream<'a> {
    index: &'a VerifiedIndex,
    selection: RelationalSourceSelection<'a>,
    certificate_index: usize,
    current: Option<SourceRecordStream>,
    page_size: usize,
    failed: bool,
    #[cfg(test)]
    pages_loaded: usize,
    #[cfg(test)]
    page_items_loaded: usize,
    #[cfg(test)]
    max_page_items: usize,
    #[cfg(test)]
    max_session_aggregates: usize,
}

impl<'a> RelationalRecordStream<'a> {
    fn new(
        index: &'a VerifiedIndex,
        selection: RelationalSourceSelection<'a>,
        page_size: usize,
    ) -> Self {
        Self {
            index,
            selection,
            certificate_index: 0,
            current: None,
            page_size,
            failed: false,
            #[cfg(test)]
            pages_loaded: 0,
            #[cfg(test)]
            page_items_loaded: 0,
            #[cfg(test)]
            max_page_items: 0,
            #[cfg(test)]
            max_session_aggregates: 0,
        }
    }

    fn prepare_next_source(
        &mut self,
    ) -> std::result::Result<bool, SourceBackedRelationalCatchUpError> {
        while let Some(certificate) = self.index.manifest().sources.get(self.certificate_index) {
            self.certificate_index += 1;
            let source = certificate.observation().source();
            if !self.selection.includes(source.identity().as_uuid()) {
                continue;
            }

            let page = load_source_page(self.index, source, None, self.page_size)?;
            self.observe_page(&page);
            self.current = Some(SourceRecordStream::new(source.clone(), page)?);
            return Ok(true);
        }
        Ok(false)
    }

    #[cfg(test)]
    fn observe_page(&mut self, page: &SourceEventPage) {
        self.pages_loaded += 1;
        self.page_items_loaded += page.items.len();
        self.max_page_items = self.max_page_items.max(page.items.len());
    }

    #[cfg(not(test))]
    fn observe_page(&mut self, _page: &SourceEventPage) {}

    #[cfg(test)]
    fn observe_page_items(&mut self, page_items: usize) {
        self.pages_loaded += 1;
        self.page_items_loaded += page_items;
        self.max_page_items = self.max_page_items.max(page_items);
    }

    #[cfg(not(test))]
    fn observe_page_items(&mut self, _page_items: usize) {}

    #[cfg(test)]
    fn observe_session_aggregates(&mut self, sessions: usize) {
        self.max_session_aggregates = self.max_session_aggregates.max(sessions);
    }

    #[cfg(not(test))]
    fn observe_session_aggregates(&mut self, _sessions: usize) {}
}

impl Iterator for RelationalRecordStream<'_> {
    type Item = std::result::Result<RelationalProjectionRecord, RelationalProjectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if self.current.is_none() {
                match self.prepare_next_source() {
                    Ok(true) => {}
                    Ok(false) => return None,
                    Err(error) => {
                        self.failed = true;
                        return Some(Err(stream_error(error)));
                    }
                }
            }

            let result = self
                .current
                .as_mut()
                .expect("prepared above")
                .next_record(self.index, self.page_size);
            let session_aggregates = self
                .current
                .as_ref()
                .map_or(0, SourceRecordStream::session_aggregates);
            self.observe_session_aggregates(session_aggregates);
            match result {
                Ok(Some((record, page_items))) => {
                    if let Some(page_items) = page_items {
                        self.observe_page_items(page_items);
                    }
                    return Some(Ok(record));
                }
                Ok(None) => self.current = None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(stream_error(error)));
                }
            }
        }
    }
}

fn stream_error(error: SourceBackedRelationalCatchUpError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

struct SourceRecordStream {
    source: SourceKey,
    stage: SourceRecordStage,
    metadata: Option<RelationalSourceMetadata>,
    sessions: BTreeMap<Uuid, SessionAggregate>,
    session_records: Option<std::collections::btree_map::IntoValues<Uuid, SessionAggregate>>,
    events: SourceEventStream,
}

impl SourceRecordStream {
    fn new(
        source: SourceKey,
        page: SourceEventPage,
    ) -> std::result::Result<Self, SourceBackedRelationalCatchUpError> {
        let first = page.items.first().map(SourceMetadataSeed::new);
        let metadata = RelationalSourceMetadata {
            source: source.clone(),
            source_root: first
                .as_ref()
                .and_then(|seed| seed.source_path.as_deref())
                .and_then(|path| Path::new(path).parent())
                .map(|path| path.to_string_lossy().into_owned())
                .filter(|path| !path.is_empty()),
            source_path: first.as_ref().and_then(|seed| seed.source_path.clone()),
            cwd: first.and_then(|seed| seed.cwd),
        };
        Ok(Self {
            source,
            stage: SourceRecordStage::Begin,
            metadata: Some(metadata),
            sessions: BTreeMap::new(),
            session_records: None,
            events: SourceEventStream::from_page(page)?,
        })
    }

    fn session_aggregates(&self) -> usize {
        self.sessions.len()
    }

    fn next_record(
        &mut self,
        index: &VerifiedIndex,
        page_size: usize,
    ) -> std::result::Result<
        Option<(RelationalProjectionRecord, Option<usize>)>,
        SourceBackedRelationalCatchUpError,
    > {
        loop {
            match self.stage {
                SourceRecordStage::Begin => {
                    self.stage = SourceRecordStage::Events;
                    return Ok(Some((
                        RelationalProjectionRecord::BeginSource(
                            self.metadata.take().expect("begin metadata is present"),
                        ),
                        None,
                    )));
                }
                SourceRecordStage::Events => {
                    if let Some(record) = self.events.pending.pop_front() {
                        return Ok(Some((record, None)));
                    }
                    if let Some(event) = self.events.items.next() {
                        let provisional_session = self.observe_event(&event)?;
                        self.events.pending = records_for_event(event, provisional_session)?;
                        continue;
                    }
                    if self.events.terminal {
                        self.session_records =
                            Some(std::mem::take(&mut self.sessions).into_values());
                        self.stage = SourceRecordStage::Sessions;
                        continue;
                    }
                    let page = load_source_page(
                        index,
                        &self.source,
                        self.events.cursor.as_ref(),
                        page_size,
                    )?;
                    let page_items = page.items.len();
                    self.events.replace_page(page)?;
                    if let Some(event) = self.events.items.next() {
                        let provisional_session = self.observe_event(&event)?;
                        self.events.pending = records_for_event(event, provisional_session)?;
                        let record = self.events.pending.pop_front().expect("event record");
                        return Ok(Some((record, Some(page_items))));
                    }
                    self.session_records = Some(std::mem::take(&mut self.sessions).into_values());
                    self.stage = SourceRecordStage::Sessions;
                    return self
                        .next_record(index, page_size)
                        .map(|record| record.map(|(record, _)| (record, Some(page_items))));
                }
                SourceRecordStage::Sessions => {
                    if let Some(session) = self.session_records.as_mut().and_then(Iterator::next) {
                        return Ok(Some((
                            RelationalProjectionRecord::Session(session.into_metadata()?),
                            None,
                        )));
                    }
                    self.stage = SourceRecordStage::End;
                }
                SourceRecordStage::End => {
                    self.stage = SourceRecordStage::Done;
                    return Ok(Some((
                        RelationalProjectionRecord::EndSource {
                            source_id: self.source.identity().as_uuid(),
                        },
                        None,
                    )));
                }
                SourceRecordStage::Done => return Ok(None),
            }
        }
    }

    fn observe_event(
        &mut self,
        event: &EventRecord,
    ) -> std::result::Result<Option<RelationalSessionMetadata>, SourceBackedRelationalCatchUpError>
    {
        match self.sessions.entry(event.session_id.as_uuid()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                let session = SessionAggregate::new(event);
                let metadata = session.to_metadata()?;
                entry.insert(session);
                Ok(Some(metadata))
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().observe(event);
                Ok(None)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SourceRecordStage {
    Begin,
    Events,
    Sessions,
    End,
    Done,
}

struct SourceEventStream {
    cursor: Option<SourceEventCursor>,
    items: std::vec::IntoIter<EventRecord>,
    terminal: bool,
    pending: VecDeque<RelationalProjectionRecord>,
}

impl SourceEventStream {
    fn from_page(
        page: SourceEventPage,
    ) -> std::result::Result<Self, SourceBackedRelationalCatchUpError> {
        let mut stream = Self {
            cursor: None,
            items: Vec::new().into_iter(),
            terminal: false,
            pending: VecDeque::new(),
        };
        stream.replace_page(page)?;
        Ok(stream)
    }

    fn replace_page(
        &mut self,
        page: SourceEventPage,
    ) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
        self.terminal = page.terminal;
        self.cursor = if page.terminal {
            None
        } else {
            Some(next_page_cursor(&page)?)
        };
        self.items = page.items.into_iter();
        if self.items.len() == 0 && !self.terminal {
            return Err(SourceBackedRelationalCatchUpError::InvalidMetadata(
                "non-terminal source event page is empty".to_owned(),
            ));
        }
        Ok(())
    }
}

fn load_source_page(
    index: &VerifiedIndex,
    source: &SourceKey,
    cursor: Option<&SourceEventCursor>,
    page_size: usize,
) -> std::result::Result<SourceEventPage, SourceBackedRelationalCatchUpError> {
    let page = index
        .source_event_page(source, cursor, page_size)
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
    Ok(page)
}

fn next_page_cursor(
    page: &SourceEventPage,
) -> std::result::Result<SourceEventCursor, SourceBackedRelationalCatchUpError> {
    page.next_cursor.clone().ok_or_else(|| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(
            "non-terminal source event page has no cursor".to_owned(),
        )
    })
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

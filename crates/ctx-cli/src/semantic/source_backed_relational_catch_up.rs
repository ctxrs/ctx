use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_core::{CertifiedSource, SourceKey};
use ctx_history_index::{
    CoreSourceEventPage, SourceEventCursor, VerifiedIndex, MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use ctx_history_relational::{
    CommittedCoreGeneration, RelationalProjectionError, RelationalProjectionMetadata,
    RelationalProjectionPlan, RelationalProjectionReceipt, RelationalProjectionRecord,
    RelationalProjectionStatus, RelationalSourceHealth, RelationalSourceMetadata,
    SourceBackedRelationalProjection, RELATIONAL_MATERIALIZER_REVISION,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::source_sql::sql_compatibility_path;

use super::{
    paths_status::{
        daemon_jobs_path, daemon_report, open_or_create_pid_lock_file, read_daemon_job_status,
        write_daemon_job_status,
    },
    source_backed_refresh_coordinator::{
        daemon_cycle_verified_index, nonzero_duration_micros, open_verified_index,
        source_backed_index_root,
    },
};

const SOURCE_BACKED_RELATIONAL_STATUS_FILE: &str = "relational-catch-up.json";
const SOURCE_BACKED_RELATIONAL_LOCK_FILE: &str = "relational-catch-up.lock";
const SOURCE_BACKED_RELATIONAL_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOURCE_BACKED_RELATIONAL_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

mod status;

use status::SourceBackedRelationalCatchUpStatus;

#[derive(Debug, Error)]
enum SourceBackedRelationalCatchUpError {
    #[error(
        "source_relational_generation_mismatch: expected Core generation {expected}, but exact index carries {actual}"
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

struct RelationalCatchUpLock {
    file: fs::File,
}

impl RelationalCatchUpLock {
    fn acquire(data_root: &Path) -> Result<Self> {
        let jobs = daemon_jobs_path(data_root);
        fs::create_dir_all(&jobs)
            .with_context(|| format!("create relational catch-up jobs root {}", jobs.display()))?;
        ctx_history_core::platform_security::restrict_private_directory(&jobs)
            .with_context(|| format!("secure relational catch-up jobs root {}", jobs.display()))?;
        let path = jobs.join(SOURCE_BACKED_RELATIONAL_LOCK_FILE);
        let (file, _) = open_or_create_pid_lock_file(&path)
            .with_context(|| format!("open relational catch-up lock {}", path.display()))?;
        fs2::FileExt::lock_exclusive(&file)
            .with_context(|| format!("acquire relational catch-up lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for RelationalCatchUpLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(super) fn run_after_core_publication(
    data_root: &Path,
    core_generation_id: &str,
) -> Result<SourceBackedRelationalCatchUpRun> {
    let _lock = RelationalCatchUpLock::acquire(data_root)?;
    run_with(data_root, core_generation_id, project_exact_core_generation)
}

pub(super) fn generation_needs_catch_up(data_root: &Path, core_generation_id: &str) -> bool {
    !read_status(data_root).is_some_and(|status| status.is_completed_for(core_generation_id))
        || ready_projection_metadata(data_root, core_generation_id).is_none()
}

pub(super) fn status_generation(data_root: &Path) -> Option<String> {
    read_status(data_root).map(|status| status.core_generation_id)
}

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

pub(crate) fn converge_required_generation(
    data_root: &Path,
    core_generation_id: &str,
) -> Result<()> {
    if generation_needs_catch_up(data_root, core_generation_id) {
        run_after_core_publication(data_root, core_generation_id)?;
    }
    wait_for_completed_generation(data_root, core_generation_id, false)
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
                    .unwrap_or("daemon Core relational catch-up failed");
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
        let Some(prior) = prior else {
            anyhow::bail!("completed relational status disappeared");
        };
        return Ok(SourceBackedRelationalCatchUpRun {
            status: prior.to_json()?,
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
                "open verified Core index {}: {error}",
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
    let (mut projection, candidate_path) = open_disposable_projection(&projection_path)?;
    let plan = projection
        .plan_generation(&generation)
        .map_err(SourceBackedRelationalCatchUpError::projection)?;
    if let RelationalProjectionPlan::NoOp(receipt) = plan {
        return Ok(ProjectionOutcome {
            receipt,
            did_work: false,
        });
    }

    let (rebuild, selection) = match &plan {
        RelationalProjectionPlan::Rebuild => (true, RelationalSourceSelection::All),
        RelationalProjectionPlan::CatchUp { changed_source_ids } => (
            false,
            RelationalSourceSelection::Changed(changed_source_ids),
        ),
        RelationalProjectionPlan::NoOp(_) => {
            return Err(SourceBackedRelationalCatchUpError::Projection(
                "relational work plan changed during one pinned run".to_owned(),
            ));
        }
    };
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

    if let Some(candidate_path) = candidate_path {
        projection
            .seal_for_replacement()
            .map_err(SourceBackedRelationalCatchUpError::projection)?;
        drop(projection);
        publish_candidate(&candidate_path, &projection_path)?;
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
    let sources = manifest
        .sources
        .iter()
        .map(relational_source_metadata)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(CommittedCoreGeneration {
        generation_id: index.generation_id().to_owned(),
        manifest_version: manifest.manifest_version,
        core_record_version: manifest.core_record_version,
        core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
        lexical_schema_version: manifest.lexical_schema_version,
        policy_schema_hash: manifest.policy_schema_hash.clone(),
        indexed_documents: manifest.indexed_documents,
        sources,
    })
}

fn relational_source_metadata(
    certificate: &CertifiedSource,
) -> std::result::Result<RelationalSourceMetadata, SourceBackedRelationalCatchUpError> {
    let encoded = serde_json::to_vec(certificate).map_err(|error| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
            "serialize Core source revision: {error}"
        ))
    })?;
    Ok(RelationalSourceMetadata {
        source: certificate.observation().source().clone(),
        parser_revision: certificate.parser_revision().to_owned(),
        revision_digest: Sha256::digest(encoded).into(),
        indexed_event_count: certificate.counts().indexed_documents,
        health: RelationalSourceHealth::Ready,
    })
}

fn open_disposable_projection(
    path: &Path,
) -> std::result::Result<
    (SourceBackedRelationalProjection, Option<PathBuf>),
    SourceBackedRelationalCatchUpError,
> {
    match SourceBackedRelationalProjection::open(path) {
        Ok(projection) => Ok((projection, None)),
        Err(
            RelationalProjectionError::MissingSchema
            | RelationalProjectionError::UnsupportedSchema { .. }
            | RelationalProjectionError::IncompatibleState(_)
            | RelationalProjectionError::MissingStableView(_),
        ) => {
            let candidate = candidate_projection_path(path);
            reset_candidate_projection(&candidate)?;
            SourceBackedRelationalProjection::open(&candidate)
                .map(|projection| (projection, Some(candidate)))
                .map_err(SourceBackedRelationalCatchUpError::projection)
        }
        Err(error) => Err(SourceBackedRelationalCatchUpError::projection(error)),
    }
}

fn candidate_projection_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.core-candidate", path.display()))
}

fn reset_candidate_projection(
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
                    "reset disposable candidate {}: {error}",
                    candidate.display()
                )));
            }
        }
    }
    Ok(())
}

fn publish_candidate(
    candidate: &Path,
    destination: &Path,
) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
    fs::rename(candidate, destination).map_err(|error| {
        SourceBackedRelationalCatchUpError::Projection(format!(
            "publish Core relational candidate {} to {}: {error}",
            candidate.display(),
            destination.display()
        ))
    })
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
            self.current = Some(SourceRecordStream::new(
                relational_source_metadata(certificate)?,
                page,
            )?);
            return Ok(true);
        }
        Ok(false)
    }

    #[cfg(test)]
    fn observe_page(&mut self, page: &CoreSourceEventPage) {
        self.pages_loaded += 1;
        self.page_items_loaded += page.items.len();
        self.max_page_items = self.max_page_items.max(page.items.len());
    }

    #[cfg(not(test))]
    fn observe_page(&mut self, _page: &CoreSourceEventPage) {}

    #[cfg(test)]
    fn observe_page_items(&mut self, page_items: usize) {
        self.pages_loaded += 1;
        self.page_items_loaded += page_items;
        self.max_page_items = self.max_page_items.max(page_items);
    }

    #[cfg(not(test))]
    fn observe_page_items(&mut self, _page_items: usize) {}
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
                    Err(error) => return self.fail(error),
                }
            }
            let Some(current) = self.current.as_mut() else {
                return self.fail(SourceBackedRelationalCatchUpError::InvalidMetadata(
                    "Core source stream was not initialized".to_owned(),
                ));
            };
            match current.next_record(self.index, self.page_size) {
                Ok(Some((record, page_items))) => {
                    if let Some(page_items) = page_items {
                        self.observe_page_items(page_items);
                    }
                    return Some(Ok(record));
                }
                Ok(None) => self.current = None,
                Err(error) => return self.fail(error),
            }
        }
    }
}

impl RelationalRecordStream<'_> {
    fn fail(
        &mut self,
        error: SourceBackedRelationalCatchUpError,
    ) -> Option<std::result::Result<RelationalProjectionRecord, RelationalProjectionError>> {
        self.failed = true;
        Some(Err(RelationalProjectionError::InvalidRecord(
            error.to_string(),
        )))
    }
}

struct SourceRecordStream {
    source: RelationalSourceMetadata,
    stage: SourceRecordStage,
    page: CorePageStream,
}

impl SourceRecordStream {
    fn new(
        source: RelationalSourceMetadata,
        page: CoreSourceEventPage,
    ) -> std::result::Result<Self, SourceBackedRelationalCatchUpError> {
        Ok(Self {
            source,
            stage: SourceRecordStage::Begin,
            page: CorePageStream::from_page(page)?,
        })
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
                    self.stage = SourceRecordStage::Records;
                    return Ok(Some((
                        RelationalProjectionRecord::BeginSource(Box::new(self.source.clone())),
                        None,
                    )));
                }
                SourceRecordStage::Records => {
                    if let Some(record) = self.page.items.next() {
                        return Ok(Some((
                            RelationalProjectionRecord::CoreRecord(Box::new(record.core_record)),
                            None,
                        )));
                    }
                    if self.page.terminal {
                        self.stage = SourceRecordStage::End;
                        continue;
                    }
                    let page = load_source_page(
                        index,
                        &self.source.source,
                        self.page.cursor.as_ref(),
                        page_size,
                    )?;
                    let page_items = page.items.len();
                    self.page.replace_page(page)?;
                    if let Some(record) = self.page.items.next() {
                        return Ok(Some((
                            RelationalProjectionRecord::CoreRecord(Box::new(record.core_record)),
                            Some(page_items),
                        )));
                    }
                    self.stage = SourceRecordStage::End;
                    return self
                        .next_record(index, page_size)
                        .map(|record| record.map(|(record, _)| (record, Some(page_items))));
                }
                SourceRecordStage::End => {
                    self.stage = SourceRecordStage::Done;
                    return Ok(Some((
                        RelationalProjectionRecord::EndSource {
                            source_id: self.source.source.identity().as_uuid(),
                        },
                        None,
                    )));
                }
                SourceRecordStage::Done => return Ok(None),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SourceRecordStage {
    Begin,
    Records,
    End,
    Done,
}

struct CorePageStream {
    cursor: Option<SourceEventCursor>,
    items: std::vec::IntoIter<ctx_history_index::CoreEventRecord>,
    terminal: bool,
}

impl CorePageStream {
    fn from_page(
        page: CoreSourceEventPage,
    ) -> std::result::Result<Self, SourceBackedRelationalCatchUpError> {
        let mut stream = Self {
            cursor: None,
            items: Vec::new().into_iter(),
            terminal: false,
        };
        stream.replace_page(page)?;
        Ok(stream)
    }

    fn replace_page(
        &mut self,
        page: CoreSourceEventPage,
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
                "non-terminal Core page is empty".to_owned(),
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
) -> std::result::Result<CoreSourceEventPage, SourceBackedRelationalCatchUpError> {
    let page = index
        .core_source_event_page(source, cursor, page_size)
        .map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "enumerate Core source {}: {error}",
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
    page: &CoreSourceEventPage,
) -> std::result::Result<SourceEventCursor, SourceBackedRelationalCatchUpError> {
    page.next_cursor.clone().ok_or_else(|| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(
            "non-terminal Core page has no cursor".to_owned(),
        )
    })
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
            && metadata.active_materializer_revision == Some(RELATIONAL_MATERIALIZER_REVISION)
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

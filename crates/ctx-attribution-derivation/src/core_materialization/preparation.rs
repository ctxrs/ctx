use super::prepared_page::{
    PreparedCoreEventDeltaPageAccumulator, PreparedCoreUnitEncoding, SizedPreparedCoreUnit,
};
use super::*;

/// Reusable, thread-safe preparation of typed Core facts used by the shipping
/// serving projection.
///
/// Individual records may be prepared concurrently by callers. Page preparation
/// uses the same record method through one bounded Rayon pool.
#[derive(Clone)]
pub struct CoreProjectionPreparer {
    pub(super) inner: Arc<CoreProjectionPreparerInner>,
}

pub(super) struct CoreProjectionPreparerInner {
    pub(super) pool: rayon::ThreadPool,
    parallelism: usize,
    pub(super) workers: CorePreparationCredits,
    pub(super) credits: CorePreparationCredits,
    pub(super) budget: Arc<ProviderWorkerBudget>,
    repository: Mutex<RepositoryPreparationState>,
}

#[derive(Default)]
struct RepositoryPreparationState {
    core_generation_id: Option<String>,
    source: Option<crate::protocol::SourceKey>,
    adapter: ctx_repository_evidence::CoreRepositoryEvidenceAdapter,
}

pub(super) struct CorePreparationCredits {
    available: Mutex<usize>,
    ready: Condvar,
    capacity: usize,
}

pub(super) struct CorePreparationPermit<'a> {
    credits: &'a CorePreparationCredits,
}

pub(super) struct PreparedOutputBudget {
    pub(super) retained_bytes: usize,
    #[cfg(test)]
    pub(super) peak_reserved_bytes: usize,
    #[cfg(test)]
    pub(super) peak_retained_bytes: usize,
    #[cfg(test)]
    pub(super) maximum_wave_width: usize,
}

impl CoreProjectionPreparer {
    /// Builds the process-wide preparer from the sanitized host launch budget.
    pub fn new() -> Result<Self, ProtocolError> {
        let inner = DEFAULT_CORE_PROJECTION_PREPARER.get_or_init(|| {
            default_provider_worker_budget().and_then(|budget| {
                CoreProjectionPreparerInner::new(budget.preparation_workers(), Arc::clone(&budget))
                    .map(Arc::new)
            })
        });
        inner
            .as_ref()
            .map(|inner| Self {
                inner: Arc::clone(inner),
            })
            .map_err(Clone::clone)
    }

    /// Builds an isolated preparer with an explicit worker count from 1 through 32.
    pub fn with_parallelism(parallelism: usize) -> Result<Self, ProtocolError> {
        let budget = ProviderWorkerBudget::isolated(parallelism);
        CoreProjectionPreparerInner::new(parallelism, budget).map(|inner| Self {
            inner: Arc::new(inner),
        })
    }

    #[must_use]
    pub fn parallelism(&self) -> usize {
        self.inner.parallelism
    }

    /// Opens and identifies the preparation accounting interval owned by one
    /// concrete materializer session. It is deliberately not a wire request
    /// boundary.
    pub fn start_measurement(&self, materialization_id: String) -> Result<(), ProtocolError> {
        let lease = self.inner.budget.begin_preparation()?;
        self.inner.reset_repository_preparation()?;
        lease.commit(materialization_id)
    }

    pub fn finish_measurement(
        &self,
        materialization_id: &str,
    ) -> Result<CoreProjectionFinishGuard, ProtocolError> {
        self.inner
            .budget
            .close_preparation(materialization_id)
            .map(|phase| CoreProjectionFinishGuard { _phase: phase })
    }

    pub fn abort_measurement(&self, materialization_id: &str) {
        self.inner.budget.abort_preparation(materialization_id);
    }

    pub(super) fn peak_workers(&self) -> Result<u16, ProtocolError> {
        self.inner.budget.preparation_peak()
    }

    /// Prepares one complete Core record without opening or mutating graph storage.
    pub fn prepare_record(
        &self,
        core_generation_id: &str,
        source: &CoreSourceState,
        record: &CoreRecord,
    ) -> Result<PreparedCoreUnit, ProtocolError> {
        validate_core_generation_id(core_generation_id)?;
        source.validate()?;
        record.validate_contract().map_err(|_| {
            ProtocolError::new(
                ErrorClass::InvalidRequest,
                "Core record contract is invalid",
            )
        })?;
        if !record.source.exact_descriptor_eq(&source.source) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "Core record belongs to another source",
            ));
        }
        let record_bytes = record
            .encode_stored()
            .map_err(|_| ProtocolError::new(ErrorClass::Internal, "Core record encoding failed"))?;
        let record_sha256 = hex::encode(Sha256::digest(record_bytes));
        let repository = self.inner.evaluate_repository_record(
            core_generation_id,
            source,
            record,
            &record_sha256,
        )?;
        self.prepare_validated_record(
            core_generation_id,
            source,
            record,
            &record_sha256,
            &repository,
        )
    }

    fn prepare_validated_record(
        &self,
        core_generation_id: &str,
        _source: &CoreSourceState,
        record: &CoreRecord,
        record_sha256: &str,
        repository: &ctx_repository_evidence::RepositoryEvaluation,
    ) -> Result<PreparedCoreUnit, ProtocolError> {
        let _worker = self.inner.workers.acquire()?;
        let _active = self.inner.budget.enter_preparation()?;
        assert_current_core_record_shape(record);
        if record.record_version != CORE_RECORD_VERSION
            || record.normalization_revision != CORE_NORMALIZATION_REVISION
            || record.content.policy_revision != CORE_CONTENT_POLICY_REVISION
        {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "Core record revisions do not match the materializer",
            ));
        }
        let event_id = record.event_id.to_string();
        let citation_id = format!("core_event_{}", hex::encode(record.event_id.digest()));
        let observation = Observation {
            observation_id: format!("core_observation_{}", hex::encode(record.event_id.digest())),
            source_sequence: record.event_sequence,
            occurred_at: record.occurred_at_unix_ms.map(|value| value.to_string()),
            actor_session_id: Some(record.session_id.to_string()),
            citation: Citation {
                citation_id,
                source_id: core_source_storage_id(&record.source),
                provider_session_id: record
                    .provider_session_id
                    .clone()
                    .unwrap_or_else(|| record.session_id.to_string()),
                item_id: Some(event_id.clone()),
                line: Some(record.event_sequence),
                byte_start: None,
                byte_end: None,
            },
            payload: ObservationPayload::Message {
                role: MessageRole::System,
                text: String::new(),
            },
        };
        let producer_authority_disposition = producer_authority_disposition(record);
        let projected = project_repositories(
            record,
            repository,
            &observation,
            producer_authority_disposition,
        );
        let mut coverage = CoreProjectionCoverage::default();
        add_record_coverage(&mut coverage, repository, &projected);
        let mut detected = DetectionBatch {
            facts: projected.facts,
            warnings: Vec::new(),
        };
        detected
            .facts
            .retain(|fact| SCHEMA_CRITICAL_FACT_FAMILIES.contains(&fact.fact_type.as_str()));
        detected.canonicalize().map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core fact canonicalization was ambiguous",
            )
        })?;
        if detected
            .facts
            .iter()
            .flat_map(|fact| &fact.evidence)
            .any(|evidence| evidence.citation_id != observation.citation.citation_id)
        {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "Core fact projection emitted evidence outside its owning event",
            ));
        }
        let evidence = (!detected.facts.is_empty()).then_some(PreparedCoreEvidence {
            citation: EvidenceCitation {
                core_generation_id: core_generation_id.to_owned(),
                source: record.source.clone(),
                session_id: record.session_id,
                event_id: record.event_id,
                event_sequence: record.event_sequence,
                byte_range: None,
                evidence_sha256: Some(record_sha256.to_owned()),
            },
        });
        if evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.citation.is_usable())
        {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "Core evidence citation is invalid",
            ));
        }
        let mut stable_entities = vec![record.event_id, record.session_id];
        if let Some(root_session_id) = record.root_session_id {
            stable_entities.push(root_session_id);
        }
        Ok(PreparedCoreUnit {
            origin_event_id: event_id,
            producer_authority_disposition,
            stable_entities,
            facts: detected.facts,
            evidence,
            coverage,
        })
    }

    /// Prepares one complete event delta page without opening or mutating graph storage.
    ///
    /// Results and errors are adjudicated in delta order after each bounded
    /// wave of independent record work completes in parallel.
    pub fn prepare_event_delta_page(
        &self,
        page: CoreEventDeltaPage,
    ) -> Result<PreparedCoreEventDeltaPage, ProtocolError> {
        let mut prepared = self.prepare_event_delta_pages(vec![page])?;
        prepared.pop().ok_or_else(|| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core event delta page preparation produced no page",
            )
        })
    }

    /// Prepares one bounded, ordered batch without opening or mutating graph storage.
    ///
    /// Record jobs are flattened in page/delta order, dispatched through the
    /// shared Rayon pool in worst-case-credit waves, and adjudicated only in
    /// their original order. The returned pages preserve request order exactly.
    pub fn prepare_event_delta_pages(
        &self,
        pages: Vec<CoreEventDeltaPage>,
    ) -> Result<Vec<PreparedCoreEventDeltaPage>, ProtocolError> {
        let pages = canonical::canonicalize_event_delta_pages(self, pages)?;
        let mut accumulated_pages = pages
            .iter()
            .map(PreparedCoreEventDeltaPageAccumulator::new)
            .collect::<Vec<_>>();
        let wrapper_bytes = accumulated_pages.iter().try_fold(0_usize, |total, page| {
            checked_prepared_output_bytes(total, page.encoded_len)
        })?;
        let mut output_budget = PreparedOutputBudget::new(wrapper_bytes)?;
        let mut omitted_bound_events = Vec::new();

        let mut jobs = Vec::new();
        for (page_slot, canonical) in pages.iter().enumerate() {
            let page = canonical.page();
            let crate::protocol::CoreSourceDelta::Present(source) = &page.reconciliation.delta
            else {
                continue;
            };
            for delta in &page.deltas {
                let Some(record) = delta.record() else {
                    continue;
                };
                jobs.push(CorePreparationJob {
                    page_slot,
                    core_generation_id: &page.core_generation_id,
                    source,
                    record,
                    record_sha256: canonical.record_sha256(record.event_id).ok_or_else(|| {
                        ProtocolError::new(
                            ErrorClass::Internal,
                            "canonical Core event record digest is unavailable",
                        )
                    })?,
                    repository: self.inner.evaluate_repository_record(
                        &page.core_generation_id,
                        source,
                        record,
                        canonical.record_sha256(record.event_id).ok_or_else(|| {
                            ProtocolError::new(
                                ErrorClass::Internal,
                                "canonical Core event record digest is unavailable",
                            )
                        })?,
                    )?,
                });
            }
        }

        let mut adjudicated = 0_usize;
        ordered_parallel_for_each_with_budget(
            &self.inner.pool,
            &jobs,
            &self.inner.credits,
            &mut output_budget,
            |job| {
                (|| {
                    let unit = self.prepare_validated_record(
                        job.core_generation_id,
                        job.source,
                        job.record,
                        job.record_sha256,
                        &job.repository,
                    )?;
                    bounded_prepared_unit(unit)
                })()
                .map_err(|mut error: ProtocolError| {
                    if error.class == ErrorClass::Bounds {
                        error.message = format!(
                            "{} (source {}, event {})",
                            error.message,
                            core_source_storage_id(&job.source.source),
                            job.record.event_id
                        );
                    }
                    error
                })
            },
            |job| {
                let unit = PreparedCoreUnit {
                    origin_event_id: job.record.event_id.to_string(),
                    producer_authority_disposition: producer_authority_disposition(job.record),
                    stable_entities: vec![job.record.event_id],
                    facts: Vec::new(),
                    evidence: None,
                    coverage: CoreProjectionCoverage::default(),
                };
                let encoding = prepared_unit_encoding(&unit.origin_event_id, &unit)?;
                if encoding.entry_len_with_separator > MAX_OMITTED_PREPARED_UNIT_BYTES {
                    return Err(prepared_output_bound_error());
                }
                Ok((SizedPreparedCoreUnit { unit, encoding }, true))
            },
            |job, (sized, omitted), budget| {
                let accumulated = accumulated_pages
                    .get_mut(job.page_slot)
                    .ok_or_else(invalid_preparation_page_slot)?;
                if accumulated.units.contains_key(&sized.unit.origin_event_id) {
                    return Err(duplicate_prepared_event_error());
                }
                adjudicated += 1;
                let (sized, omitted, entry_bytes) = retain_prepared_unit(
                    sized,
                    omitted,
                    !accumulated.units.is_empty(),
                    jobs.len() - adjudicated,
                    budget,
                )?;
                if omitted {
                    omitted_bound_events.push((
                        core_source_storage_id(&job.source.source),
                        job.record.event_id.to_string(),
                    ));
                }
                accumulated.retain(
                    sized.unit.origin_event_id.clone(),
                    sized.unit,
                    sized.encoding,
                    entry_bytes,
                )
            },
        )?;

        let prepared_pages = pages
            .into_iter()
            .zip(accumulated_pages)
            .map(|(page, accumulated)| PreparedCoreEventDeltaPage::new(page, accumulated))
            .collect::<Vec<_>>();
        let exact_bytes = prepared_pages.iter().try_fold(0_usize, |total, page| {
            checked_prepared_output_bytes(total, page.prepared_output_encoded_len())
        })?;
        if exact_bytes != output_budget.retained_bytes {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "Core prepared event delta page accounting diverged",
            ));
        }
        for (source_id, event_id) in omitted_bound_events {
            eprintln!(
                "warning: Blame omitted event {event_id} from source {source_id}; prepared output size bound"
            );
        }
        Ok(prepared_pages)
    }
}

const MAX_OMITTED_PREPARED_UNIT_BYTES: usize = 4 * 1024;

pub(super) fn retain_prepared_unit(
    mut sized: SizedPreparedCoreUnit,
    mut omitted: bool,
    needs_separator: bool,
    remaining_jobs: usize,
    budget: &mut PreparedOutputBudget,
) -> Result<(SizedPreparedCoreUnit, bool, usize), ProtocolError> {
    let minimum_remaining = remaining_jobs
        .checked_mul(MAX_OMITTED_PREPARED_UNIT_BYTES)
        .ok_or_else(prepared_output_overflow_error)?;
    let mut entry_bytes = sized.encoding.entry_bytes(needs_separator)?;
    if checked_prepared_output_bytes(budget.retained_bytes, entry_bytes)
        .and_then(|total| checked_prepared_output_bytes(total, minimum_remaining))
        .is_err()
    {
        sized = omit_prepared_unit(sized.unit)?;
        entry_bytes = sized.encoding.entry_bytes(needs_separator)?;
        omitted = true;
    }
    budget.retain(entry_bytes)?;
    Ok((sized, omitted, entry_bytes))
}

fn omit_prepared_unit(mut unit: PreparedCoreUnit) -> Result<SizedPreparedCoreUnit, ProtocolError> {
    unit.stable_entities
        .retain(|entity| entity.to_string() == unit.origin_event_id);
    unit.facts.clear();
    unit.evidence = None;
    unit.coverage = CoreProjectionCoverage::default();
    let encoding = prepared_unit_encoding(&unit.origin_event_id, &unit)?;
    if encoding.entry_len_with_separator > MAX_OMITTED_PREPARED_UNIT_BYTES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "omitted Core event identity exceeds its prepared output allowance",
        ));
    }
    Ok(SizedPreparedCoreUnit { unit, encoding })
}

pub(super) fn bounded_prepared_unit(
    unit: PreparedCoreUnit,
) -> Result<(SizedPreparedCoreUnit, bool), ProtocolError> {
    let encoding = prepared_unit_encoding(&unit.origin_event_id, &unit)?;
    let omitted = encoding.entry_len_with_separator > MAX_CORE_PREPARED_UNIT_BYTES;
    if omitted {
        return Ok((omit_prepared_unit(unit)?, true));
    }
    validate_prepared_unit_hard_max(encoding)?;
    Ok((SizedPreparedCoreUnit { unit, encoding }, omitted))
}

pub fn core_preparation_peak_workers() -> Result<u16, ProtocolError> {
    CoreProjectionPreparer::new()?.peak_workers()
}

#[cfg(test)]
pub(super) fn configured_core_preparation_workers(
    value: Option<std::ffi::OsString>,
    available_parallelism: usize,
) -> Result<usize, ProtocolError> {
    configured_provider_worker_limits(value, available_parallelism)
        .map(|limits| limits.preparation_workers)
}

struct CorePreparationJob<'a> {
    page_slot: usize,
    core_generation_id: &'a str,
    source: &'a CoreSourceState,
    record: &'a CoreRecord,
    record_sha256: &'a str,
    repository: ctx_repository_evidence::RepositoryEvaluation,
}

impl CoreProjectionPreparerInner {
    fn new(parallelism: usize, budget: Arc<ProviderWorkerBudget>) -> Result<Self, ProtocolError> {
        if parallelism == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "Core preparation parallelism must be at least one",
            ));
        }
        if parallelism > MAX_CORE_PREPARATION_PARALLELISM {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                format!(
                    "Core preparation parallelism cannot exceed {MAX_CORE_PREPARATION_PARALLELISM}"
                ),
            ));
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(parallelism)
            .thread_name(|index| format!("ctx-core-prep-{index}"))
            .build()
            .map_err(|_| {
                ProtocolError::new(
                    ErrorClass::Internal,
                    "Core preparation worker pool is unavailable",
                )
            })?;
        Ok(Self {
            pool,
            parallelism,
            workers: CorePreparationCredits::new(parallelism),
            credits: CorePreparationCredits::new(MAX_CORE_PREPARATION_CREDITS),
            budget,
            repository: Mutex::new(RepositoryPreparationState::default()),
        })
    }

    fn reset_repository_preparation(&self) -> Result<(), ProtocolError> {
        let mut repository = self.repository.lock().map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core repository preparation state was poisoned",
            )
        })?;
        *repository = RepositoryPreparationState::default();
        Ok(())
    }

    fn evaluate_repository_record(
        &self,
        core_generation_id: &str,
        source: &CoreSourceState,
        record: &CoreRecord,
        record_sha256: &str,
    ) -> Result<ctx_repository_evidence::RepositoryEvaluation, ProtocolError> {
        let mut repository = self.repository.lock().map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core repository preparation state was poisoned",
            )
        })?;
        let source_changed = repository.core_generation_id.as_deref() != Some(core_generation_id)
            || repository
                .source
                .as_ref()
                .is_none_or(|prior| !prior.exact_descriptor_eq(&source.source));
        if source_changed {
            repository.adapter.begin_source();
            repository.core_generation_id = Some(core_generation_id.to_owned());
            repository.source = Some(source.source.clone());
        }
        Ok(repository.adapter.evaluate_record(record, record_sha256))
    }
}

impl CorePreparationCredits {
    fn new(capacity: usize) -> Self {
        Self {
            available: Mutex::new(capacity),
            ready: Condvar::new(),
            capacity,
        }
    }

    pub(super) fn acquire(&self) -> Result<CorePreparationPermit<'_>, ProtocolError> {
        let mut available = self.available.lock().map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core preparation credit state was poisoned",
            )
        })?;
        while *available == 0 {
            available = self.ready.wait(available).map_err(|_| {
                ProtocolError::new(
                    ErrorClass::Internal,
                    "Core preparation credit wait was poisoned",
                )
            })?;
        }
        *available -= 1;
        Ok(CorePreparationPermit { credits: self })
    }
}

impl Drop for CorePreparationPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut available) = self.credits.available.lock() {
            *available += 1;
            self.credits.ready.notify_one();
        }
    }
}

#[cfg(test)]
pub(super) fn ordered_parallel_map<T, U>(
    pool: &rayon::ThreadPool,
    items: &[T],
    operation: impl Fn(&T) -> U + Send + Sync,
) -> Vec<U>
where
    T: Sync,
    U: Send,
{
    pool.install(|| items.par_iter().map(operation).collect())
}

pub fn ordered_parallel_map_owned<T, U>(
    preparer: &CoreProjectionPreparer,
    items: Vec<T>,
    work_cap: usize,
    operation: impl Fn(T) -> Result<U, ProtocolError> + Send + Sync,
) -> Result<Vec<U>, ProtocolError>
where
    T: Send,
    U: Send,
{
    if items.is_empty() || items.len() > work_cap {
        return Err(ProtocolError::new(
            ErrorClass::Internal,
            "Core canonicalization ordered work count is invalid",
        ));
    }

    let item_count = items.len();
    // Vec's indexed parallel iterator preserves input order. Adjudicate only
    // after every result is present so completion order cannot select errors.
    let results = preparer.inner.pool.install(|| {
        items
            .into_par_iter()
            .map(|item| {
                let _worker = preparer.inner.workers.acquire()?;
                let _active = preparer.inner.budget.enter_preparation()?;
                operation(item)
            })
            .collect::<Vec<_>>()
    });
    let mut output = Vec::with_capacity(item_count);
    for result in results {
        output.push(result?);
    }
    Ok(output)
}

impl PreparedOutputBudget {
    pub(super) fn new(retained_bytes: usize) -> Result<Self, ProtocolError> {
        checked_prepared_output_bytes(0, retained_bytes)?;
        Ok(Self {
            retained_bytes,
            #[cfg(test)]
            peak_reserved_bytes: retained_bytes,
            #[cfg(test)]
            peak_retained_bytes: retained_bytes,
            #[cfg(test)]
            maximum_wave_width: 0,
        })
    }

    pub(super) fn next_wave_width(
        &mut self,
        pending_jobs: usize,
        credit_capacity: usize,
    ) -> Result<usize, ProtocolError> {
        if pending_jobs == 0 {
            return Ok(0);
        }
        if credit_capacity == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "Core preparation credit count is invalid",
            ));
        }
        let remaining_bytes = MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
            .checked_sub(self.retained_bytes)
            .ok_or_else(prepared_output_bound_error)?;
        let width = pending_jobs
            .min(credit_capacity)
            .min(remaining_bytes / MAX_CORE_PREPARED_UNIT_BYTES);
        if width == 0 {
            return Err(prepared_output_bound_error());
        }
        let active_bytes = width
            .checked_mul(MAX_CORE_PREPARED_UNIT_BYTES)
            .ok_or_else(prepared_output_overflow_error)?;
        let reserved_bytes = self
            .retained_bytes
            .checked_add(active_bytes)
            .ok_or_else(prepared_output_overflow_error)?;
        if reserved_bytes > MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES {
            return Err(prepared_output_bound_error());
        }
        #[cfg(test)]
        {
            self.peak_reserved_bytes = self.peak_reserved_bytes.max(reserved_bytes);
            self.maximum_wave_width = self.maximum_wave_width.max(width);
        }
        Ok(width)
    }

    pub(super) fn retain(&mut self, bytes: usize) -> Result<(), ProtocolError> {
        self.retained_bytes = checked_prepared_output_bytes(self.retained_bytes, bytes)?;
        #[cfg(test)]
        {
            self.peak_retained_bytes = self.peak_retained_bytes.max(self.retained_bytes);
        }
        Ok(())
    }
}

fn ordered_parallel_wave_with_credits<T, U>(
    pool: &rayon::ThreadPool,
    items: &[T],
    credits: &CorePreparationCredits,
    operation: &(impl Fn(&T) -> Result<U, ProtocolError> + Send + Sync),
) -> Result<Vec<Result<U, ProtocolError>>, ProtocolError>
where
    T: Sync,
    U: Send,
{
    if items.len() > credits.capacity || credits.capacity == 0 {
        return Err(ProtocolError::new(
            ErrorClass::Internal,
            "Core preparation wave exceeds its credit count",
        ));
    }
    let slots = (0..items.len())
        .map(|_| Mutex::new(None))
        .collect::<Vec<_>>();
    pool.install(|| {
        items.par_iter().enumerate().for_each(|(index, item)| {
            if let Ok(mut slot) = slots[index].lock() {
                *slot = Some(match credits.acquire() {
                    Ok(_permit) => operation(item),
                    Err(error) => Err(error),
                });
            }
        });
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .map_err(|_| {
                    ProtocolError::new(
                        ErrorClass::Internal,
                        "Core preparation indexed result slot was poisoned",
                    )
                })?
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Internal,
                        "Core preparation worker omitted an indexed result",
                    )
                })
        })
        .collect()
}

pub(super) fn ordered_parallel_for_each_with_budget<T, U>(
    pool: &rayon::ThreadPool,
    items: &[T],
    credits: &CorePreparationCredits,
    budget: &mut PreparedOutputBudget,
    operation: impl Fn(&T) -> Result<U, ProtocolError> + Send + Sync,
    fallback: impl Fn(&T) -> Result<U, ProtocolError>,
    mut adjudicate: impl FnMut(&T, U, &mut PreparedOutputBudget) -> Result<(), ProtocolError>,
) -> Result<(), ProtocolError>
where
    T: Sync,
    U: Send,
{
    let mut cursor = 0_usize;
    while cursor < items.len() {
        if credits.capacity == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "Core preparation credit count is invalid",
            ));
        }
        if MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES - budget.retained_bytes
            < MAX_CORE_PREPARED_UNIT_BYTES
        {
            for item in &items[cursor..] {
                adjudicate(item, fallback(item)?, budget)?;
            }
            break;
        }
        let width = budget.next_wave_width(items.len() - cursor, credits.capacity)?;
        let wave = &items[cursor..cursor + width];
        let results = ordered_parallel_wave_with_credits(pool, wave, credits, &operation)?;
        for (item, result) in wave.iter().zip(results) {
            adjudicate(item, result?, budget)?;
        }
        cursor += width;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn collect_prepared_units(
    prepared: Vec<Result<Option<PreparedCoreUnit>, ProtocolError>>,
) -> Result<BTreeMap<String, PreparedCoreUnit>, ProtocolError> {
    let mut units = BTreeMap::new();
    for result in prepared {
        let Some(unit) = result? else {
            continue;
        };
        if units.insert(unit.origin_event_id.clone(), unit).is_some() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core event delta page contains duplicate prepared events",
            ));
        }
    }
    Ok(units)
}

#[cfg(test)]
pub(super) fn collect_prepared_page_units(
    page_count: usize,
    prepared: impl IntoIterator<Item = (usize, Result<PreparedCoreUnit, ProtocolError>)>,
) -> Result<Vec<BTreeMap<String, PreparedCoreUnit>>, ProtocolError> {
    let mut page_units = vec![BTreeMap::new(); page_count];
    let mut retained_bytes = 0_usize;
    for (page_slot, result) in prepared {
        let unit = result?;
        retained_bytes =
            checked_prepared_output_bytes(retained_bytes, canonical_encoded_len(&unit)?)?;
        let units = page_units.get_mut(page_slot).ok_or_else(|| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core event delta preparation page slot is invalid",
            )
        })?;
        if units.insert(unit.origin_event_id.clone(), unit).is_some() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core event delta page batch contains duplicate prepared events",
            ));
        }
    }
    Ok(page_units)
}

pub(super) fn prepared_unit_encoding(
    key: &str,
    unit: &PreparedCoreUnit,
) -> Result<PreparedCoreUnitEncoding, ProtocolError> {
    let key_bytes = canonical_encoded_len(&key)?;
    let unit_bytes = canonical_encoded_len(unit)?;
    let entry_len_with_separator = key_bytes
        .checked_add(1)
        .and_then(|bytes| bytes.checked_add(unit_bytes))
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(prepared_output_overflow_error)?;
    Ok(PreparedCoreUnitEncoding {
        unit_len: unit_bytes,
        entry_len_with_separator,
    })
}

pub(super) fn validate_prepared_unit_hard_max(
    encoding: PreparedCoreUnitEncoding,
) -> Result<(), ProtocolError> {
    if encoding.entry_len_with_separator > MAX_CORE_PREPARED_UNIT_BYTES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "Core prepared unit exceeds its worst-case byte credit",
        ));
    }
    Ok(())
}

fn invalid_preparation_page_slot() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Internal,
        "Core event delta preparation page slot is invalid",
    )
}

fn duplicate_prepared_event_error() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Sequence,
        "Core event delta page batch contains duplicate prepared events",
    )
}

pub(super) fn prepared_output_overflow_error() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Bounds,
        "Core prepared event delta page bytes overflowed",
    )
}

fn prepared_output_bound_error() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Bounds,
        "Core prepared event delta page batch exceeds its aggregate byte bound",
    )
}

pub(super) fn checked_prepared_output_bytes(
    current: usize,
    additional: usize,
) -> Result<usize, ProtocolError> {
    let total = current
        .checked_add(additional)
        .ok_or_else(prepared_output_overflow_error)?;
    if total > MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES {
        return Err(prepared_output_bound_error());
    }
    Ok(total)
}

#[derive(Default)]
struct EncodedLengthWriter {
    bytes: usize,
}

impl Write for EncodedLengthWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("encoded length overflowed"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn canonical_encoded_len(value: &impl Serialize) -> Result<usize, ProtocolError> {
    let mut writer = EncodedLengthWriter::default();
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "Core prepared event delta page encoding failed",
        )
    })?;
    Ok(writer.bytes)
}

pub(super) fn prepared_page_base_encoded_len(
    request_sha256: &str,
    canonical_page_bytes: usize,
) -> Result<usize, ProtocolError> {
    let request_sha256_bytes = canonical_encoded_len(&request_sha256)?;
    b"{\"request_sha256\":"
        .len()
        .checked_add(request_sha256_bytes)
        .and_then(|bytes| bytes.checked_add(b",\"page\":".len()))
        .and_then(|bytes| bytes.checked_add(canonical_page_bytes))
        .and_then(|bytes| bytes.checked_add(b",\"units\":".len()))
        .and_then(|bytes| bytes.checked_add(b"{}".len()))
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(prepared_output_overflow_error)
}

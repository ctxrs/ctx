fn verify_searcher_with_options(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    requested_worker_budget: usize,
    instrument: bool,
    synchronize_first_wave: bool,
) -> Result<VerificationRunMetrics> {
    with_verification_scratch_budget(|| {
        verify_searcher_with_options_and_budget(
            searcher,
            manifest,
            requested_worker_budget,
            instrument,
            synchronize_first_wave,
        )
    })
}

fn verify_searcher_with_options_and_budget(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    requested_worker_budget: usize,
    instrument: bool,
    synchronize_first_wave: bool,
) -> Result<VerificationRunMetrics> {
    #[cfg(any(test, feature = "test-support"))]
    LOGICAL_PASSES.with(|count| count.set(count.get() + 1));
    verify_searcher_structure(searcher, manifest)?;
    let fields = fields_from_schema(searcher.schema())?;
    validate_verification_projection(fields)?;
    let verification_spill = VerificationSpill::create(
        searcher
            .segment_readers()
            .iter()
            .map(tantivy::SegmentReader::max_doc),
    )?;
    let (tasks, _task_scratch) =
        segment_verification_tasks(searcher, requested_worker_budget.max(1))?;
    let worker_budget = requested_worker_budget.max(1).min(tasks.len().max(1));
    let worker_scratch_bytes = worker_budget
        .checked_mul(
            VERIFICATION_SPILL_BUFFER_BYTES
                .checked_mul(2)
                .and_then(|bytes| bytes.checked_add(VERIFICATION_SPILL_RECORD_BYTES))
                .ok_or(IndexError::CountOverflow)?,
        )
        .ok_or(IndexError::CountOverflow)?;
    let task_heap_bytes = tasks
        .capacity()
        .checked_mul(std::mem::size_of::<SegmentVerificationTask>())
        .ok_or(IndexError::CountOverflow)?;
    let runtime_heap_bytes = worker_scratch_bytes;
    let _runtime_scratch = reserve_verification_scratch(
        0,
        u64::try_from(runtime_heap_bytes).map_err(|_| IndexError::CountOverflow)?,
    )?;
    let executor = if worker_budget == 1 {
        Executor::single_thread()
    } else {
        Executor::multi_thread(worker_budget, "ctx-generation-verify-")?
    };
    let counters = instrument.then(VerificationCounters::default);
    let first_wave_size = worker_budget.min(tasks.len());
    let rendezvous =
        (synchronize_first_wave && first_wave_size > 1).then(|| Barrier::new(first_wave_size));
    let mut metrics = VerificationRunMetrics {
        #[cfg(any(test, feature = "test-support"))]
        worker_budget,
        ..VerificationRunMetrics::default()
    };
    let mut total_documents = 0_u64;
    let mut expected_body_tokens = 0_u64;
    let mut parent_session_documents = 0_u64;
    let mut root_session_documents = 0_u64;
    let mut source_aggregates = BTreeMap::<String, SourceAggregate>::new();
    let source_ordinals = manifest
        .core_record_aggregates
        .iter()
        .enumerate()
        .map(|(ordinal, aggregate)| {
            Ok((
                aggregate.source_identity_digest().to_owned(),
                u32::try_from(ordinal).map_err(|_| IndexError::CountOverflow)?,
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    metrics.verification_spill_bytes = verification_spill.logical_bytes();
    metrics.verification_tracked_heap_bytes = verification_spill
        .segment_offsets_heap_bytes()?
        .checked_add(runtime_heap_bytes)
        .and_then(|bytes| bytes.checked_add(task_heap_bytes))
        .ok_or(IndexError::CountOverflow)?;

    for wave_start in (0..tasks.len()).step_by(worker_budget) {
        let wave_end = (wave_start + worker_budget).min(tasks.len());
        let wave = &tasks[wave_start..wave_end];
        let wave_rendezvous = (wave_start == 0).then_some(rendezvous.as_ref()).flatten();
        let segments = executor.map(
            |task_index| {
                let task = wave[task_index];
                Ok(verify_segment(
                    searcher,
                    task,
                    wave_rendezvous,
                    counters.as_ref(),
                    &verification_spill,
                    &source_ordinals,
                ))
            },
            0..wave.len(),
        )?;
        metrics.max_buffered_segments = metrics.max_buffered_segments.max(segments.len());
        for segment in segments {
            let segment = segment?;
            metrics.segment_tasks += 1;
            metrics.document_decodes = metrics
                .document_decodes
                .checked_add(segment.document_decodes)
                .ok_or(IndexError::CountOverflow)?;
            metrics.stored_core_bytes = metrics
                .stored_core_bytes
                .checked_add(segment.stored_core_bytes)
                .ok_or(IndexError::CountOverflow)?;
            metrics.body_tokens = metrics
                .body_tokens
                .checked_add(segment.body_tokens)
                .ok_or(IndexError::CountOverflow)?;
            metrics.source_terms = metrics
                .source_terms
                .checked_add(segment.source_aggregates.len())
                .ok_or(IndexError::CountOverflow)?;
            total_documents = total_documents
                .checked_add(segment.document_count)
                .ok_or(IndexError::CountOverflow)?;
            expected_body_tokens = expected_body_tokens
                .checked_add(segment.body_tokens)
                .ok_or(IndexError::CountOverflow)?;
            parent_session_documents = parent_session_documents
                .checked_add(segment.parent_session_documents)
                .ok_or(IndexError::CountOverflow)?;
            root_session_documents = root_session_documents
                .checked_add(segment.root_session_documents)
                .ok_or(IndexError::CountOverflow)?;
            merge_source_aggregates(&mut source_aggregates, segment.source_aggregates)?;
        }
    }

    metrics.max_active_workers = counters
        .as_ref()
        .map(|counters| counters.max_active_workers.load(Ordering::SeqCst))
        .unwrap_or(0);
    if total_documents != manifest.indexed_documents {
        return Err(IndexError::DocumentCountMismatch {
            manifest: manifest.indexed_documents,
            index: total_documents,
        });
    }
    verify_live_body_token_count(searcher, fields.body_search, expected_body_tokens)?;
    let mut projection_deltas = verification_spill.load_projection_deltas()?;
    metrics.verification_tracked_heap_bytes = metrics
        .verification_tracked_heap_bytes
        .checked_add(projection_deltas.heap_bytes())
        .ok_or(IndexError::CountOverflow)?;
    verify_event_identities(
        searcher,
        fields.event_id,
        total_documents,
        &mut projection_deltas,
    )?;
    verify_session_identities(
        searcher,
        [
            (fields.session_id, IdentityFieldRole::Session),
            (fields.parent_session_id, IdentityFieldRole::ParentSession),
            (fields.root_session_id, IdentityFieldRole::RootSession),
        ],
        [
            total_documents,
            parent_session_documents,
            root_session_documents,
        ],
        &verification_spill,
        &mut projection_deltas,
    )?;
    verify_remaining_query_projections(searcher, fields, &mut projection_deltas)?;
    verify_query_projection_completion(searcher, &projection_deltas)?;
    verify_manifest_aggregates(manifest, source_aggregates)?;
    metrics.max_buffered_event_identities = 0;
    metrics.max_buffered_session_identities = 0;
    Ok(metrics)
}

fn verify_segment(
    searcher: &Searcher,
    task: SegmentVerificationTask,
    rendezvous: Option<&Barrier>,
    counters: Option<&VerificationCounters>,
    verification_spill: &VerificationSpill,
    source_ordinals: &HashMap<String, u32>,
) -> Result<SegmentVerification> {
    let _active_worker = ActiveVerificationWorker::enter(counters);
    if let Some(rendezvous) = rendezvous {
        rendezvous.wait();
    }
    let segment = searcher.segment_reader(task.segment_ord as u32);
    let fields = fields_from_schema(searcher.schema())?;
    let body_search = segment.inverted_index(fields.body_search)?;
    let mut body_analyzer = crate::analyzer::body_analyzer();
    let mut source_aggregates = BTreeMap::<String, SourceAggregate>::new();
    let mut document_decodes = 0;
    let mut stored_core_bytes = 0_u64;
    let mut body_tokens = 0_u64;
    let mut parent_session_documents = 0_u64;
    let mut root_session_documents = 0_u64;
    let mut identity_writer = verification_spill.segment_range_writer(
        task.segment_ord,
        task.start_doc_id,
        task.end_doc_id,
        segment.max_doc(),
    )?;
    let mut document_count = 0_u64;
    for doc_id in task.start_doc_id..task.end_doc_id {
        if segment.is_deleted(doc_id) {
            identity_writer.write_deleted(doc_id)?;
            continue;
        }
        document_count = document_count
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        document_decodes += 1;
        let record = stored_verification_record(
            searcher,
            DocAddress::new(task.segment_ord as u32, doc_id),
            fields,
        )?;
        stored_core_bytes = stored_core_bytes
            .checked_add(
                u64::try_from(record.stored_core_bytes).map_err(|_| IndexError::CountOverflow)?,
            )
            .ok_or(IndexError::CountOverflow)?;
        verify_query_fast_fields(segment, doc_id, &record)?;
        let projection_delta = expected_query_projection_delta(fields, &record)?;
        let session_witness_present = verify_session_witness_key(&record)?;
        let source_ordinal = *source_ordinals
            .get(&record.source_owner)
            .ok_or(IndexError::InvalidStoredDocumentField("source_key"))?;
        let source = source_aggregates.entry(record.source_owner).or_default();
        source.count = source
            .count
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        let accumulator_leaf =
            core_record_accumulator_leaf(record.core_record.event_id, &record.core_record_leaf)?;
        accumulate_core_record(&mut source.accumulator, &accumulator_leaf);
        parent_session_documents = parent_session_documents
            .checked_add(u64::from(record.identities.parent_session.is_some()))
            .ok_or(IndexError::CountOverflow)?;
        root_session_documents = root_session_documents
            .checked_add(u64::from(record.identities.root_session.is_some()))
            .ok_or(IndexError::CountOverflow)?;
        let body_projection =
            crate::index_document::project_indexed_body_search(record.core_record.content)?;
        body_tokens = body_tokens
            .checked_add(verify_body_projection(
                &body_search,
                &mut body_analyzer,
                fields.body_search,
                body_projection.as_deref(),
                doc_id,
            )?)
            .ok_or(IndexError::CountOverflow)?;
        identity_writer.write_record(
            doc_id,
            SpillVerificationIdentities {
                event: record.identities.event,
                session: record.identities.session,
                parent_session: record.identities.parent_session,
                root_session: record.identities.root_session,
                session_source_ordinal: source_ordinal,
                session_relationship_kind: relationship_kind_tag(
                    record.core_record.session_relationship,
                ),
                session_witness_present,
            },
            projection_delta,
        )?;
    }
    identity_writer.finish()?;
    Ok(SegmentVerification {
        document_count,
        document_decodes,
        stored_core_bytes,
        body_tokens,
        source_aggregates,
        parent_session_documents,
        root_session_documents,
    })
}

fn segment_verification_tasks(
    searcher: &Searcher,
    worker_budget: usize,
) -> Result<(Vec<SegmentVerificationTask>, ScratchReservation)> {
    const MAX_VERIFICATION_TASK_HEAP_BYTES: usize = 16 * 1024 * 1024;
    let total_max_docs = searcher
        .segment_readers()
        .iter()
        .map(tantivy::SegmentReader::max_doc)
        .try_fold(0_u64, |total, max_doc| {
            total
                .checked_add(u64::from(max_doc))
                .ok_or(IndexError::CountOverflow)
        })?;
    let documents_per_task = verification_documents_per_task(total_max_docs, worker_budget)?;
    let task_count = searcher
        .segment_readers()
        .iter()
        .map(tantivy::SegmentReader::max_doc)
        .try_fold(0_usize, |count, max_doc| {
            let segment_tasks = usize::try_from(max_doc.div_ceil(documents_per_task))
                .map_err(|_| IndexError::CountOverflow)?;
            count
                .checked_add(segment_tasks)
                .ok_or(IndexError::CountOverflow)
        })?;
    let task_heap_bytes = task_count
        .checked_mul(std::mem::size_of::<SegmentVerificationTask>())
        .ok_or(IndexError::CountOverflow)?;
    if task_heap_bytes > MAX_VERIFICATION_TASK_HEAP_BYTES {
        return Err(IndexError::VerificationScratchLimitExceeded {
            required_bytes: u64::try_from(task_heap_bytes)
                .map_err(|_| IndexError::CountOverflow)?,
            maximum_bytes: MAX_VERIFICATION_TASK_HEAP_BYTES as u64,
        });
    }
    let reservation = reserve_verification_scratch(
        0,
        u64::try_from(task_heap_bytes).map_err(|_| IndexError::CountOverflow)?,
    )?;
    let mut tasks = Vec::with_capacity(task_count);
    append_segment_verification_tasks(
        &mut tasks,
        searcher
            .segment_readers()
            .iter()
            .map(tantivy::SegmentReader::max_doc),
        documents_per_task,
    );
    Ok((tasks, reservation))
}

#[cfg(test)]
fn segment_verification_tasks_for_max_docs(
    segment_max_docs: &[u32],
    worker_budget: usize,
) -> Result<Vec<SegmentVerificationTask>> {
    let total_max_docs = segment_max_docs.iter().try_fold(0_u64, |total, max_doc| {
        total
            .checked_add(u64::from(*max_doc))
            .ok_or(IndexError::CountOverflow)
    })?;
    let documents_per_task = verification_documents_per_task(total_max_docs, worker_budget)?;
    let mut tasks = Vec::new();
    append_segment_verification_tasks(
        &mut tasks,
        segment_max_docs.iter().copied(),
        documents_per_task,
    );
    Ok(tasks)
}

fn verification_documents_per_task(total_max_docs: u64, worker_budget: usize) -> Result<u32> {
    const MIN_DOCUMENTS_PER_TASK: u64 = 16 * 1024;
    const TASKS_PER_WORKER: u64 = 4;

    let target_tasks = u64::try_from(worker_budget)
        .map_err(|_| IndexError::CountOverflow)?
        .checked_mul(TASKS_PER_WORKER)
        .ok_or(IndexError::CountOverflow)?
        .max(1);
    let documents_per_task = total_max_docs
        .div_ceil(target_tasks)
        .max(MIN_DOCUMENTS_PER_TASK);
    let documents_per_task = u32::try_from(documents_per_task.min(u64::from(u32::MAX)))
        .map_err(|_| IndexError::CountOverflow)?;
    Ok(documents_per_task)
}

fn append_segment_verification_tasks(
    tasks: &mut Vec<SegmentVerificationTask>,
    segment_max_docs: impl Iterator<Item = u32>,
    documents_per_task: u32,
) {
    for (segment_ord, max_doc) in segment_max_docs.enumerate() {
        let mut start_doc_id = 0_u32;
        while start_doc_id < max_doc {
            let end_doc_id = start_doc_id.saturating_add(documents_per_task).min(max_doc);
            tasks.push(SegmentVerificationTask {
                segment_ord,
                start_doc_id,
                end_doc_id,
            });
            start_doc_id = end_doc_id;
        }
    }
}

fn expected_query_projection_delta(
    fields: crate::Fields,
    record: &VerificationRecord,
) -> Result<ProjectionAccumulator> {
    let core = &record.core_record;
    let mut expected = Vec::<[u8; 32]>::new();
    let mut add = |term: Term| {
        expected.push(query_projection_digest(
            term.field(),
            term.serialized_value_bytes(),
        ));
    };

    add(Term::from_field_text(
        fields.event_id,
        &core.event_id.to_string(),
    ));
    add(Term::from_field_text(
        fields.event_identity_digest,
        &hex(&core.event_id.digest()),
    ));
    add(Term::from_field_text(
        fields.session_id,
        &core.session_id.to_string(),
    ));
    if let Some(parent_session_id) = core.parent_session_id {
        add(Term::from_field_text(
            fields.parent_session_id,
            &parent_session_id.to_string(),
        ));
    }
    if let Some(root_session_id) = core.root_session_id {
        add(Term::from_field_text(
            fields.root_session_id,
            &root_session_id.to_string(),
        ));
    }
    if let Some(relationship) = core.session_relationship {
        add(Term::from_field_text(
            fields.provider_native_session_relationship,
            relationship.as_str(),
        ));
    }
    if let Some(copy) = &core.event_copy {
        add(Term::from_field_text(
            fields.event_copy_ancestor_session_id,
            &copy.ancestor_session_id.to_string(),
        ));
        add(Term::from_field_text(
            fields.event_copy_ancestor_event_id,
            &copy.ancestor_event_id.to_string(),
        ));
        add(Term::from_field_text(
            fields.event_copy_proof,
            crate::index_document::event_copy_proof_str(copy.proof),
        ));
    }
    add(Term::from_field_text(
        fields.source_key,
        &record.source_owner,
    ));
    add(Term::from_field_text(
        fields.provider,
        core.source.provider(),
    ));
    add(Term::from_field_text(
        fields.source_format,
        core.source.source_format(),
    ));
    if core.source.provider() == "custom" {
        if let Some(ctx_history_core::TypedKey::Composite(values)) = core.native_event_id.as_ref() {
            if let [ctx_history_core::TypedKey::Utf8(provider_key), ctx_history_core::TypedKey::Utf8(source_id), ctx_history_core::TypedKey::Utf8(_)] =
                values.as_slice()
            {
                add(Term::from_field_text(
                    fields.custom_provider_key,
                    provider_key,
                ));
                add(Term::from_field_text(fields.custom_source_id, source_id));
            }
        }
    }
    if let Some(provider_session_id) = &core.provider_session_id {
        add(Term::from_field_text(
            fields.provider_session_id,
            provider_session_id,
        ));
    }
    if let Some(agent_scope) = core.agent_scope {
        add(Term::from_field_text(
            fields.agent_scope,
            agent_scope.as_str(),
        ));
    }
    add(Term::from_field_u64(
        fields.event_sequence,
        core.event_sequence,
    ));
    if let Some(occurred_at_unix_ms) = core.occurred_at_unix_ms {
        add(Term::from_field_i64(
            fields.occurred_at_unix_ms,
            occurred_at_unix_ms,
        ));
    }
    add(Term::from_field_text(fields.event_type, &core.event_type));
    if let Some(role) = &core.role {
        add(Term::from_field_text(fields.role, role));
    }
    if let Some(activity) = &core.content.activity {
        for fact in &activity.facts {
            add(Term::from_field_text(
                fields.literal_fact(fact.kind),
                &fact.value,
            ));
        }
    }
    add(Term::from_field_bytes(
        fields.source_event_order,
        &record.source_event_order,
    ));
    add(Term::from_field_bytes(
        fields.session_event_order,
        &record.session_event_order,
    ));
    if let Some(session_authority) = record.session_authority {
        add(Term::from_field_bytes(
            fields.session_authority,
            &session_authority,
        ));
    }
    add(Term::from_field_bytes(
        fields.semantic_event_order,
        &record.semantic_event_order,
    ));
    add(Term::from_field_bytes(
        fields.event_range_order,
        &record.event_range_order,
    ));
    if core.content.is_discovery_eligible() {
        add(Term::from_field_u64(fields.discovery_eligible, 1));
    }

    // Basic postings expose one membership per distinct term and document even
    // when Core contributes the same exact value through multiple properties.
    expected.sort_unstable();
    expected.dedup();
    let mut delta = ProjectionAccumulator::default();
    for digest in expected {
        delta.subtract(&digest);
    }
    Ok(delta)
}

struct IncrementalProjectionVerifier {
    deltas: ProjectionDeltas,
    _spill: VerificationSpill,
    body_analyzer: tantivy::tokenizer::TextAnalyzer,
    expected_body_tokens: u64,
}

impl IncrementalProjectionVerifier {
    fn new(searcher: &Searcher, changed_segments: &[usize]) -> Result<Self> {
        let spill = VerificationSpill::create(searcher.segment_readers().iter().enumerate().map(
            |(segment_ord, segment)| {
                if changed_segments.binary_search(&segment_ord).is_ok() {
                    segment.max_doc()
                } else {
                    0
                }
            },
        ))?;
        let deltas = spill.load_projection_deltas()?;
        Ok(Self {
            deltas,
            _spill: spill,
            body_analyzer: crate::analyzer::body_analyzer(),
            expected_body_tokens: 0,
        })
    }

    fn verify_document(
        &mut self,
        searcher: &Searcher,
        fields: crate::Fields,
        address: DocAddress,
        record: VerificationRecord,
    ) -> Result<()> {
        note_candidate_projection_document();
        let segment = searcher.segment_reader(address.segment_ord);
        verify_query_fast_fields(segment, address.doc_id, &record)?;
        let delta = expected_query_projection_delta(fields, &record)?;
        let body_search = segment.inverted_index(fields.body_search)?;
        let body_projection =
            crate::index_document::project_indexed_body_search(record.core_record.content)?;
        self.expected_body_tokens = self
            .expected_body_tokens
            .checked_add(verify_body_projection(
                &body_search,
                &mut self.body_analyzer,
                fields.body_search,
                body_projection.as_deref(),
                address.doc_id,
            )?)
            .ok_or(IndexError::CountOverflow)?;
        self.deltas.set_expected(address, delta)
    }

    fn finish(
        mut self,
        searcher: &Searcher,
        fields: crate::Fields,
        changed_segments: &[usize],
    ) -> Result<()> {
        if live_body_token_count_for_segments(
            searcher,
            fields.body_search,
            changed_segments.iter().copied(),
        )? != self.expected_body_tokens
        {
            return Err(IndexError::InvalidStoredDocumentField("body_search"));
        }
        for field in incremental_query_projection_fields(fields) {
            for &segment_ord in changed_segments {
                let segment = searcher
                    .segment_readers()
                    .get(segment_ord)
                    .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?;
                let inverted = segment.inverted_index(field)?;
                let mut terms = inverted.terms().stream()?;
                while terms.advance() {
                    let digest = query_projection_digest(field, terms.key());
                    for_each_live_posting(
                        &inverted,
                        terms.value(),
                        segment_ord,
                        segment,
                        |address| self.accumulate(address, &digest),
                    )?;
                }
            }
        }
        for &segment_ord in changed_segments {
            let segment = searcher
                .segment_readers()
                .get(segment_ord)
                .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?;
            for doc_id in 0..segment.max_doc() {
                if !segment.is_deleted(doc_id)
                    && !self
                        .deltas
                        .is_complete(DocAddress::new(segment_ord as u32, doc_id))?
                {
                    return Err(IndexError::InvalidStoredDocumentField("query_projection"));
                }
            }
        }
        Ok(())
    }

    fn accumulate(&mut self, address: DocAddress, digest: &[u8; 32]) -> Result<()> {
        self.deltas.accumulate(address, digest)
    }
}

fn incremental_query_projection_fields(fields: crate::Fields) -> [Field; 39] {
    let remaining = remaining_query_projection_fields(fields);
    let mut all = [fields.event_id; 39];
    all[..4].copy_from_slice(&[
        fields.event_id,
        fields.session_id,
        fields.parent_session_id,
        fields.root_session_id,
    ]);
    all[4..].copy_from_slice(&remaining);
    all
}

fn query_projection_digest(field: Field, serialized_value: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.core-query-projection-v1\0");
    digest.update(field.field_id().to_be_bytes());
    digest.update((serialized_value.len() as u64).to_be_bytes());
    digest.update(serialized_value);
    digest.finalize().into()
}

fn verify_query_fast_fields(
    segment: &tantivy::SegmentReader,
    doc_id: u32,
    record: &VerificationRecord,
) -> Result<()> {
    macro_rules! verify_u64 {
        ($field:literal, $expected:expr) => {{
            let column = segment.fast_fields().u64($field)?;
            let mut values = column.values_for_doc(doc_id);
            if values.next() != Some($expected) || values.next().is_some() {
                return Err(IndexError::InvalidStoredDocumentField($field));
            }
        }};
    }
    macro_rules! verify_optional_i64 {
        ($field:literal, $expected:expr) => {{
            let column = segment.fast_fields().i64($field)?;
            let mut values = column.values_for_doc(doc_id);
            if values.next() != $expected || values.next().is_some() {
                return Err(IndexError::InvalidStoredDocumentField($field));
            }
        }};
    }

    let event_uuid = record.core_record.event_id.as_uuid().as_u128();
    verify_u64!("event_id_high", (event_uuid >> 64) as u64);
    verify_u64!("event_id_low", event_uuid as u64);
    verify_u64!("event_sequence", record.core_record.event_sequence);
    verify_optional_i64!(
        "occurred_at_unix_ms",
        record.core_record.occurred_at_unix_ms
    );
    verify_u64!(
        "core_content_bytes",
        u64::try_from(crate::index_document::core_content_bytes(
            &record.core_record.content
        )?)
        .map_err(|_| IndexError::CountOverflow)?
    );
    verify_u64!(
        "core_record_encoded_bytes",
        u64::try_from(record.stored_core_bytes).map_err(|_| IndexError::CountOverflow)?
    );
    let event_range_order = segment
        .fast_fields()
        .bytes("event_range_order")?
        .ok_or(IndexError::InvalidStoredDocumentField("event_range_order"))?;
    let mut term_ords = event_range_order.term_ords(doc_id);
    let term_ord = term_ords
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField("event_range_order"))?;
    if term_ords.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField("event_range_order"));
    }
    let mut actual_event_range_order = Vec::with_capacity(record.event_range_order.len());
    if !event_range_order.ord_to_bytes(term_ord, &mut actual_event_range_order)?
        || actual_event_range_order.as_slice() != record.event_range_order
    {
        return Err(IndexError::InvalidStoredDocumentField("event_range_order"));
    }
    Ok(())
}

fn verify_remaining_query_projections(
    searcher: &Searcher,
    fields: crate::Fields,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    for field in remaining_query_projection_fields(fields) {
        for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(field)?;
            let mut terms = inverted.terms().stream()?;
            while terms.advance() {
                let digest = query_projection_digest(field, terms.key());
                for_each_live_posting(&inverted, terms.value(), segment_ord, segment, |address| {
                    projection_deltas.accumulate(address, &digest)
                })?;
            }
        }
    }
    Ok(())
}

fn remaining_query_projection_fields(fields: crate::Fields) -> [Field; 35] {
    [
        fields.event_identity_digest,
        fields.provider_native_session_relationship,
        fields.event_copy_ancestor_session_id,
        fields.event_copy_ancestor_event_id,
        fields.event_copy_proof,
        fields.source_key,
        fields.provider,
        fields.source_format,
        fields.custom_provider_key,
        fields.custom_source_id,
        fields.provider_session_id,
        fields.agent_scope,
        fields.event_sequence,
        fields.occurred_at_unix_ms,
        fields.event_type,
        fields.role,
        fields.fact_session_cwd,
        fields.fact_tool_workdir,
        fields.fact_file,
        fields.fact_url,
        fields.fact_forge,
        fields.fact_project,
        fields.fact_vcs,
        fields.fact_commit,
        fields.fact_pull_request,
        fields.fact_command,
        fields.fact_branch,
        fields.fact_workspace,
        fields.fact_provider_disposition,
        fields.source_event_order,
        fields.session_event_order,
        fields.session_authority,
        fields.semantic_event_order,
        fields.event_range_order,
        fields.discovery_eligible,
    ]
}

fn verify_query_projection_completion(
    searcher: &Searcher,
    projection_deltas: &ProjectionDeltas,
) -> Result<()> {
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
        for doc_id in 0..segment.max_doc() {
            if !segment.is_deleted(doc_id)
                && !projection_deltas.is_complete(DocAddress::new(segment_ord, doc_id))?
            {
                return Err(IndexError::InvalidStoredDocumentField("query_projection"));
            }
        }
    }
    Ok(())
}

mod logical_body_projection;
use logical_body_projection::{
    live_body_token_count_for_segments, verify_body_projection, verify_live_body_token_count,
};

fn merge_source_aggregates(
    target: &mut BTreeMap<String, SourceAggregate>,
    source: BTreeMap<String, SourceAggregate>,
) -> Result<()> {
    for (source_id, aggregate) in source {
        let total = target.entry(source_id).or_default();
        total.count = total
            .count
            .checked_add(aggregate.count)
            .ok_or(IndexError::CountOverflow)?;
        accumulate_core_record(&mut total.accumulator, &aggregate.accumulator);
    }
    Ok(())
}

fn verify_manifest_aggregates(
    manifest: &GenerationManifest,
    mut actual: BTreeMap<String, SourceAggregate>,
) -> Result<()> {
    for expected in &manifest.core_record_aggregates {
        let source_id = expected.source_identity_digest().to_owned();
        let observed = actual.remove(&source_id).unwrap_or_default();
        if observed.count != expected.indexed_documents() {
            return Err(IndexError::CoreRecordAggregateCountMismatch {
                source_id,
                manifest: expected.indexed_documents(),
                index: observed.count,
            });
        }
        if observed.accumulator != expected.accumulator_bytes()? {
            return Err(IndexError::CoreRecordAggregateMismatch(source_id));
        }
    }
    if let Some((source_id, aggregate)) = actual.into_iter().next() {
        return Err(IndexError::SourceCountMismatch {
            source_id,
            manifest: 0,
            index: aggregate.count,
        });
    }
    Ok(())
}

mod logical_identity;
use logical_identity::{
    canonical_uuid_term, for_each_live_posting, relationship_kind_tag, verify_event_identities,
    verify_session_identities, verify_session_witness_key,
};

#[cfg(test)]
mod body_projection_tests {
    use ctx_history_core::{
        ActivityInvocation, ActivityJsonCapture, CoreActivity, CoreContent,
        CoreContentPolicyStatus, CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION,
    };
    use tantivy::TantivyDocument;

    use super::*;

    fn expected_invocation_projection() -> String {
        crate::project_body_search(CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some("normalized body".to_owned()),
            structured_content: None,
            discovery_exclusion: None,
            activity: Some(CoreActivity {
                revision: CORE_ACTIVITY_REVISION,
                provider_call_id: Some(ctx_history_core::TypedKey::U64(1)),
                invocation: Some(ActivityInvocation {
                    protocol: None,
                    server: Some("verificationservercanary".to_owned()),
                    tool: "verificationtoolcanary".to_owned(),
                    arguments: ActivityJsonCapture::Present {
                        value: serde_json::json!({
                            "verificationargumentkey": "verificationargumentvalue"
                        }),
                    },
                    started_at_unix_ms: None,
                }),
                result: None,
                facts: Vec::new(),
            }),
        })
        .unwrap()
        .unwrap()
    }

    fn searcher_with_body(body: &str) -> (Searcher, Field) {
        let schema = crate::lexical_schema();
        let fields = crate::fields_from_schema(&schema).unwrap();
        let index = tantivy::Index::create_in_ram(schema);
        crate::analyzer::register_body_analyzer(&index);
        let mut writer = index.writer(20_000_000).unwrap();
        let mut document = TantivyDocument::default();
        document.add_text(fields.body_search, body);
        writer.add_document(document).unwrap();
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        let reader = index.reader().unwrap();
        (reader.searcher(), fields.body_search)
    }

    #[test]
    fn body_verification_rejects_missing_and_extra_invocation_terms() {
        let expected = expected_invocation_projection();
        let mut analyzer = crate::analyzer::body_analyzer();

        let (missing_searcher, field) =
            searcher_with_body("normalized body\nverificationservercanary\nverificationtoolcanary");
        let missing_inverted = missing_searcher
            .segment_reader(0)
            .inverted_index(field)
            .unwrap();
        assert!(matches!(
            verify_body_projection(&missing_inverted, &mut analyzer, field, Some(&expected), 0,),
            Err(IndexError::InvalidStoredDocumentField("body_search"))
        ));

        let (extra_searcher, field) =
            searcher_with_body(&format!("{expected}\nextrainvocationterm"));
        let extra_inverted = extra_searcher
            .segment_reader(0)
            .inverted_index(field)
            .unwrap();
        let expected_token_count =
            verify_body_projection(&extra_inverted, &mut analyzer, field, Some(&expected), 0)
                .unwrap();
        assert!(matches!(
            verify_live_body_token_count(&extra_searcher, field, expected_token_count),
            Err(IndexError::InvalidStoredDocumentField("body_search"))
        ));
    }
}

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CoreRecord, ScannedSourceCounts, SourceFrontier,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;

use super::super::{
    JsonlCheckpoint, JsonlFileObservation, JsonlProbe, JsonlReader, JsonlSourceChange,
    JsonlSourceIdentity,
};
use super::{
    binding_digest, contract_error, route_internal, route_invalid, route_scan, FamilyCheckpoint,
    JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyLeaf, JsonlFamilyOptimizedLeafOutcome,
    JsonlFamilyProjectionMode, JsonlFamilyPublication, JsonlFamilyWorkerContext,
    TerminalSourceEvidence, FAMILY_FRONTIER_KIND, FAMILY_POLICY_REVISION,
    FAMILY_SOURCE_REVISION_KIND,
};
#[cfg(test)]
use super::{
    jsonl_family_scanner_probe, record_jsonl_family_scanner_activity, JsonlFamilyScannerProbe,
};
use crate::{
    provider::source_backed::{
        CoreRecordEmissionBatchBuilder, ParallelLeafScanBegin, ParallelLeafScanComplete,
        ParallelLeafScanError, ParallelLeafScanJob, ParallelLeafScanWorkerError,
        SourceBackedGenerationSink, SourceBackedRouteError, SourceBackedRouteResult,
    },
    CaptureError, Result,
};

pub(super) struct PreparedLeaf {
    pub(super) certificate: CertifiedSource,
    pub(super) append: Option<CertifiedSourceAppend>,
    pub(super) checkpoint: Option<JsonlCheckpoint>,
}

struct JsonlLeafJob {
    leaf: JsonlFamilyLeaf,
    base: Option<CertifiedSource>,
    context_shard: Option<u64>,
}

const JSONL_PARTITION_CONTEXT_SHARDS: u64 = 16;

// Partitioned adapters receive deterministic logical cache lanes rather than
// caches tied to the physical worker count. `begin_leaf()` clears event-time
// and other source-semantic attribution state; only revalidated positive and
// negative repository certification caches intentionally survive between
// components in the same lane. This keeps one/eight/sixteen-worker output
// identical while bounding expensive full Git probes at the runner maximum.
#[derive(Default)]
struct JsonlFamilyWorkerContexts {
    independent: JsonlFamilyWorkerContext,
    partition_cache_lanes: BTreeMap<u64, JsonlFamilyWorkerContext>,
}

impl JsonlFamilyWorkerContexts {
    fn for_job(&mut self, context_shard: Option<u64>) -> &mut JsonlFamilyWorkerContext {
        match context_shard {
            Some(context_shard) => self.partition_cache_lanes.entry(context_shard).or_default(),
            None => &mut self.independent,
        }
    }
}

// The large variant deliberately carries CoreRecord by value: boxing every
// projected record would add one allocation to the generic JSONL hot path.
#[allow(clippy::large_enum_variant)]
pub(super) enum JsonlLeafOutputEvent {
    Page {
        append: bool,
        records: Vec<CoreRecord>,
    },
    Record {
        append: bool,
        record: CoreRecord,
    },
    Flush,
}

pub(super) struct JsonlLeafOutput<'emit> {
    emit: &'emit mut dyn FnMut(JsonlLeafOutputEvent) -> Result<()>,
}

impl<'emit> JsonlLeafOutput<'emit> {
    pub(super) fn new(emit: &'emit mut dyn FnMut(JsonlLeafOutputEvent) -> Result<()>) -> Self {
        Self { emit }
    }

    fn emit_page(&mut self, append: bool, records: Vec<CoreRecord>) -> Result<()> {
        (self.emit)(JsonlLeafOutputEvent::Page { append, records })
    }

    fn emit_record(&mut self, append: bool, record: CoreRecord) -> Result<()> {
        (self.emit)(JsonlLeafOutputEvent::Record { append, record })
    }

    fn flush(&mut self) -> Result<()> {
        (self.emit)(JsonlLeafOutputEvent::Flush)
    }
}

fn scan_leaf_serial(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    base_event_lookup: &BaseEventIdentityLookup,
    worker: &mut JsonlFamilyWorkerContext,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<TerminalSourceEvidence> {
    let mut staging_started = false;
    let mut append_staging = false;
    let mut sink_failure = None;
    let mut emit = |event| {
        let append = match &event {
            JsonlLeafOutputEvent::Page { append, .. }
            | JsonlLeafOutputEvent::Record { append, .. } => *append,
            JsonlLeafOutputEvent::Flush => return Ok(()),
        };
        if !staging_started {
            if append {
                let expected = base.ok_or_else(|| {
                    CaptureError::InvalidPayload("JSONL append has no base".to_owned())
                })?;
                let staged = sink
                    .begin_source_append(leaf.source().clone())
                    .map_err(|error| preserve_coordinator_error(&mut sink_failure, error))?;
                if staged != expected {
                    return Err(CaptureError::InvalidPayload(
                        "JSONL append base changed before staging".to_owned(),
                    ));
                }
            } else {
                sink.begin_source(leaf.source().clone())
                    .map_err(|error| preserve_coordinator_error(&mut sink_failure, error))?;
            }
            staging_started = true;
            append_staging = append;
        } else if append_staging != append {
            return Err(CaptureError::SystemInvariant(
                "JSONL publication mode changed during one leaf scan",
            ));
        }
        match event {
            JsonlLeafOutputEvent::Page { records, .. } => {
                for record in records {
                    sink.add_core_record(record)
                        .map_err(|error| preserve_coordinator_error(&mut sink_failure, error))?;
                }
            }
            JsonlLeafOutputEvent::Record { record, .. } => {
                sink.add_core_record(record)
                    .map_err(|error| preserve_coordinator_error(&mut sink_failure, error))?;
            }
            JsonlLeafOutputEvent::Flush => unreachable!("flush returned before staging"),
        }
        Ok(())
    };
    let mut output = JsonlLeafOutput::new(&mut emit);
    let prepared = prepare_leaf(adapter, leaf, base, base_event_lookup, worker, &mut output);
    if let Some(error) = sink_failure {
        return Err(error);
    }
    let prepared = prepared.map_err(|error| route_scan(adapter, error))?;

    let PreparedLeaf {
        certificate,
        append,
        checkpoint,
    } = prepared;
    match append {
        Some(append) => {
            if staging_started && !append_staging {
                return Err(route_internal(
                    "append JSONL source emitted replacement documents",
                ));
            }
            if !staging_started {
                let staged = sink
                    .begin_source_append(leaf.source().clone())
                    .map_err(route_internal)?;
                if staged != append.base() {
                    return Err(route_invalid("JSONL append base changed before staging"));
                }
            }
            sink.certify_source_append(append).map_err(route_internal)?;
            Ok(TerminalSourceEvidence {
                certificate,
                checkpoint,
            })
        }
        None => {
            if staging_started && append_staging {
                return Err(route_internal(
                    "replacement JSONL source emitted append documents",
                ));
            }
            if !staging_started {
                sink.begin_source(leaf.source().clone())
                    .map_err(route_internal)?;
            }
            sink.certify_source(certificate.clone())
                .map_err(route_internal)?;
            Ok(TerminalSourceEvidence {
                certificate,
                checkpoint,
            })
        }
    }
}

fn run_parallel_leaf_job_batch(
    adapter: &dyn JsonlFamilyAdapter,
    jobs: Vec<ParallelLeafScanJob<JsonlLeafJob>>,
    worker_states: &mut [JsonlFamilyWorkerContexts],
    base_event_lookup: &BaseEventIdentityLookup,
    sink: &mut SourceBackedGenerationSink<'_>,
    #[cfg(test)] scanner_probe: Option<&JsonlFamilyScannerProbe>,
) -> SourceBackedRouteResult<Vec<TerminalSourceEvidence>> {
    sink.run_parallel_leaf_scans_with_worker_states(
        jobs,
        worker_states,
        |worker_contexts, job, emitter| {
            let worker = worker_contexts.for_job(job.leaf().context_shard);
            #[cfg(test)]
            let _active_scanner = scanner_probe.map(JsonlFamilyScannerProbe::enter);
            let leaf = &job.leaf().leaf;
            let mut staging_started = false;
            let mut append_staging = false;
            let mut emission_failure = None;
            let mut pending_emissions = CoreRecordEmissionBatchBuilder::default();
            let mut emit = |event| {
                let flush = matches!(
                    &event,
                    JsonlLeafOutputEvent::Page { .. } | JsonlLeafOutputEvent::Flush
                );
                let append = match &event {
                    JsonlLeafOutputEvent::Page { append, .. }
                    | JsonlLeafOutputEvent::Record { append, .. } => Some(*append),
                    JsonlLeafOutputEvent::Flush => None,
                };
                if let Some(append) = append {
                    if !staging_started {
                        let begin = if append {
                            let base = job.leaf().base.clone().ok_or_else(|| {
                                CaptureError::InvalidPayload(
                                    "parallel JSONL append has no base".to_owned(),
                                )
                            })?;
                            ParallelLeafScanBegin::append(leaf.source().clone(), base)
                        } else {
                            ParallelLeafScanBegin::replace(leaf.source().clone())
                        };
                        emitter.begin(begin).map_err(|_| {
                            CaptureError::SystemInvariant(
                                "JSONL parallel scan was cancelled before publication",
                            )
                        })?;
                        staging_started = true;
                        append_staging = append;
                    } else if append_staging != append {
                        return Err(CaptureError::SystemInvariant(
                            "parallel JSONL publication mode changed during one leaf scan",
                        ));
                    }
                    match event {
                        JsonlLeafOutputEvent::Page { records, .. } => {
                            for record in records {
                                emitter
                                    .emit_core_record_batched(&mut pending_emissions, record)
                                    .map_err(|error| {
                                        preserve_parallel_emit_error(&mut emission_failure, error)
                                    })?;
                            }
                        }
                        JsonlLeafOutputEvent::Record { record, .. } => {
                            emitter
                                .emit_core_record_batched(&mut pending_emissions, record)
                                .map_err(|error| {
                                    preserve_parallel_emit_error(&mut emission_failure, error)
                                })?;
                        }
                        JsonlLeafOutputEvent::Flush => {
                            unreachable!("flush has no publication mode")
                        }
                    }
                }
                if flush {
                    emitter
                        .emit_core_record_batch(&mut pending_emissions)
                        .map_err(|error| {
                            preserve_parallel_emit_error(&mut emission_failure, error)
                        })?;
                }
                Ok(())
            };
            let mut output = JsonlLeafOutput::new(&mut emit);
            let prepared = prepare_leaf(
                adapter,
                leaf,
                job.leaf().base.as_ref(),
                base_event_lookup,
                worker,
                &mut output,
            );
            if let Some(error) = emission_failure {
                return Err(ParallelLeafScanWorkerError::provider(error));
            }
            let prepared = prepared
                .map_err(|error| route_scan(adapter, error))
                .map_err(ParallelLeafScanWorkerError::provider)?;

            let PreparedLeaf {
                certificate,
                append,
                checkpoint,
            } = prepared;
            match append {
                Some(append) => {
                    if staging_started && !append_staging {
                        return Err(ParallelLeafScanWorkerError::provider(route_invalid(
                            "parallel JSONL append emitted replacement documents",
                        )));
                    }
                    if !staging_started {
                        emitter
                            .begin(ParallelLeafScanBegin::append(
                                leaf.source().clone(),
                                append.base().clone(),
                            ))
                            .map_err(ParallelLeafScanWorkerError::from)?;
                    }
                    emitter
                        .complete(ParallelLeafScanComplete::append(
                            append,
                            TerminalSourceEvidence {
                                certificate,
                                checkpoint,
                            },
                        ))
                        .map_err(ParallelLeafScanWorkerError::from)?;
                }
                None => {
                    if staging_started && append_staging {
                        return Err(ParallelLeafScanWorkerError::provider(route_invalid(
                            "parallel JSONL replacement emitted append documents",
                        )));
                    }
                    if !staging_started {
                        emitter
                            .begin(ParallelLeafScanBegin::replace(leaf.source().clone()))
                            .map_err(ParallelLeafScanWorkerError::from)?;
                    }
                    let evidence = TerminalSourceEvidence {
                        certificate: certificate.clone(),
                        checkpoint,
                    };
                    emitter
                        .complete(ParallelLeafScanComplete::replace(certificate, evidence))
                        .map_err(ParallelLeafScanWorkerError::from)?;
                }
            }
            Ok(())
        },
    )
    .map_err(map_parallel_leaf_error)
}

pub(super) fn scan_leaves(
    adapter: &dyn JsonlFamilyAdapter,
    leaves: &[JsonlFamilyLeaf],
    bases: &HashMap<[u8; 32], &CertifiedSource>,
    base_event_lookup: BaseEventIdentityLookup,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<HashMap<[u8; 32], TerminalSourceEvidence>> {
    let worker_limit = adapter
        .prepare_leaf_scans(leaves, bases)
        .map_err(|error| route_scan(adapter, error))?;
    let recommended_workers = sink.recommended_leaf_workers(leaves.len());
    let recommended_workers = worker_limit
        .map(|limit| recommended_workers.min(limit.max(1)))
        .unwrap_or(recommended_workers);
    let worker_count = family_scanner_worker_count(recommended_workers);
    let mut leaf_metadata = Vec::new();
    leaf_metadata
        .try_reserve_exact(leaves.len())
        .map_err(|_| route_internal("JSONL leaf scheduling allocation failed"))?;
    let mut saw_partition = false;
    let mut saw_unpartitioned = false;
    let mut previous_phase = None;
    for leaf in leaves {
        let phase = adapter
            .leaf_scan_phase(leaf)
            .map_err(|error| route_scan(adapter, error))?;
        if previous_phase.is_some_and(|previous| previous > phase) {
            return Err(route_invalid(
                "JSONL adapter returned non-monotonic leaf scan phases",
            ));
        }
        previous_phase = Some(phase);
        let partition = adapter
            .leaf_scan_partition(leaf)
            .map_err(|error| route_scan(adapter, error))?;
        saw_partition |= partition.is_some();
        saw_unpartitioned |= partition.is_none();
        leaf_metadata.push((phase, partition));
    }
    if saw_partition && saw_unpartitioned {
        return Err(route_invalid(
            "JSONL adapter mixed partitioned and unpartitioned leaf scans",
        ));
    }
    let partition_count = if saw_partition {
        leaf_metadata
            .iter()
            .filter_map(|(_, partition)| *partition)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    } else {
        0
    };
    let active_worker_count = if saw_partition {
        worker_count.min(partition_count.max(1))
    } else {
        worker_count
    };
    let mut serial_worker = JsonlFamilyWorkerContext::default();
    #[cfg(test)]
    let scanner_probe = jsonl_family_scanner_probe(if saw_partition {
        1
    } else {
        active_worker_count
    });
    // A dependency cap of one limits concurrent scanners, but a multi-leaf
    // family still benefits from overlapping that one scanner with writer
    // admission of the preceding page. Keep the direct path for the truly
    // single-leaf case where spawning a pipeline cannot amortize its setup.
    if worker_count <= 1 && leaves.len() <= 1 {
        let mut terminal_sources = HashMap::with_capacity(leaves.len());
        for (leaf_index, leaf) in leaves.iter().enumerate() {
            let partition = leaf_metadata
                .get(leaf_index)
                .and_then(|(_, partition)| *partition);
            if let Some(partition) = partition {
                adapter
                    .begin_leaf_scan_partition(partition)
                    .map_err(|error| route_scan(adapter, error))?;
            }
            #[cfg(test)]
            let _active_scanner = scanner_probe.as_ref().map(|probe| probe.enter());
            let evidence = scan_leaf_serial(
                adapter,
                leaf,
                base_for_leaf(bases, leaf),
                &base_event_lookup,
                &mut serial_worker,
                sink,
            );
            let finish_partition = partition
                .map(|partition| {
                    adapter
                        .finish_leaf_scan_partition(partition)
                        .map_err(|error| route_scan(adapter, error))
                })
                .transpose();
            let evidence = evidence?;
            finish_partition?;
            if terminal_sources
                .insert(leaf.source().exact_descriptor_digest(), evidence)
                .is_some()
            {
                return Err(route_invalid("duplicate JSONL source identity"));
            }
        }
        #[cfg(test)]
        record_jsonl_family_scanner_activity(active_worker_count, scanner_probe.as_deref());
        return Ok(terminal_sources);
    }

    let state_worker_count = if saw_partition {
        active_worker_count
    } else {
        worker_count
    };
    let mut worker_states = (0..state_worker_count)
        .map(|_| JsonlFamilyWorkerContexts::default())
        .collect::<Vec<_>>();

    if saw_partition {
        let mut partitions = BTreeMap::<u64, (u64, Vec<JsonlFamilyLeaf>)>::new();
        for (leaf, (_, partition)) in leaves.iter().cloned().zip(leaf_metadata.iter()) {
            let partition = partition.ok_or_else(|| {
                route_invalid("JSONL partition metadata disappeared before scheduling")
            })?;
            let worker_affinity = adapter
                .leaf_worker_affinity(&leaf)
                .map_err(|error| route_scan(adapter, error))?
                .unwrap_or(partition);
            let context_shard = worker_affinity % JSONL_PARTITION_CONTEXT_SHARDS;
            let (partition_context_shard, partition_leaves) = partitions
                .entry(partition)
                .or_insert_with(|| (context_shard, Vec::new()));
            if *partition_context_shard != context_shard {
                return Err(route_invalid(
                    "JSONL adapter returned multiple worker-affinity lanes for one partition",
                ));
            }
            partition_leaves.push(leaf);
        }
        let partitions = partitions.into_iter().collect::<Vec<_>>();
        let mut evidences = Vec::with_capacity(leaves.len());
        for wave in partitions.chunks(active_worker_count.max(1)) {
            let mut begun = Vec::with_capacity(wave.len());
            for (partition, _) in wave {
                if let Err(error) = adapter.begin_leaf_scan_partition(*partition) {
                    for begun_partition in begun.into_iter().rev() {
                        let _ = adapter.finish_leaf_scan_partition(begun_partition);
                    }
                    return Err(route_scan(adapter, error));
                }
                begun.push(*partition);
            }
            let mut jobs = Vec::new();
            for (_, (context_shard, partition_leaves)) in wave {
                for leaf in partition_leaves.iter().cloned() {
                    let base = base_for_leaf(bases, &leaf).cloned();
                    jobs.push(
                        ParallelLeafScanJob::new(
                            leaf.source().clone(),
                            JsonlLeafJob {
                                leaf,
                                base,
                                context_shard: Some(*context_shard),
                            },
                        )
                        .with_worker_affinity(*context_shard),
                    );
                }
            }
            let batch = run_parallel_leaf_job_batch(
                adapter,
                jobs,
                &mut worker_states,
                &base_event_lookup,
                sink,
                #[cfg(test)]
                scanner_probe.as_deref(),
            );
            let mut finish_error = None;
            for partition in begun.into_iter().rev() {
                if let Err(error) = adapter.finish_leaf_scan_partition(partition) {
                    if finish_error.is_none() {
                        finish_error = Some(route_scan(adapter, error));
                    }
                }
            }
            let batch = batch?;
            if let Some(error) = finish_error {
                return Err(error);
            }
            evidences.extend(batch);
        }
        #[cfg(test)]
        record_jsonl_family_scanner_activity(active_worker_count, scanner_probe.as_deref());
        return collect_terminal_sources(evidences);
    }

    let phases = leaf_metadata
        .iter()
        .map(|(phase, _)| *phase)
        .collect::<Vec<_>>();

    let mut evidences = Vec::with_capacity(leaves.len());
    let mut phase_start = 0_usize;
    while phase_start < leaves.len() {
        let phase = phases[phase_start];
        let mut phase_end = phase_start.saturating_add(1);
        while phase_end < leaves.len() && phases[phase_end] == phase {
            phase_end = phase_end.saturating_add(1);
        }
        let mut jobs = Vec::with_capacity(phase_end.saturating_sub(phase_start));
        for leaf in leaves[phase_start..phase_end].iter().cloned() {
            let base = base_for_leaf(bases, &leaf).cloned();
            let worker_affinity = adapter
                .leaf_worker_affinity(&leaf)
                .map_err(|error| route_scan(adapter, error))?;
            let job = ParallelLeafScanJob::new(
                leaf.source().clone(),
                JsonlLeafJob {
                    leaf,
                    base,
                    context_shard: None,
                },
            );
            jobs.push(match worker_affinity {
                Some(worker_affinity) => job.with_worker_affinity(worker_affinity),
                None => job,
            });
        }
        let phase_evidences = run_parallel_leaf_job_batch(
            adapter,
            jobs,
            &mut worker_states,
            &base_event_lookup,
            sink,
            #[cfg(test)]
            scanner_probe.as_deref(),
        )?;
        evidences.extend(phase_evidences);
        phase_start = phase_end;
    }
    #[cfg(test)]
    record_jsonl_family_scanner_activity(worker_count, scanner_probe.as_deref());

    collect_terminal_sources(evidences)
}

fn collect_terminal_sources(
    evidences: Vec<TerminalSourceEvidence>,
) -> SourceBackedRouteResult<HashMap<[u8; 32], TerminalSourceEvidence>> {
    let mut terminal_sources = HashMap::with_capacity(evidences.len());
    for evidence in evidences {
        let digest = evidence
            .certificate
            .observation()
            .source()
            .exact_descriptor_digest();
        if terminal_sources.insert(digest, evidence).is_some() {
            return Err(route_invalid("duplicate JSONL source identity"));
        }
    }
    Ok(terminal_sources)
}

pub(super) fn prepare_leaf(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    base_event_lookup: &BaseEventIdentityLookup,
    worker: &mut JsonlFamilyWorkerContext,
    output: &mut JsonlLeafOutput<'_>,
) -> Result<PreparedLeaf> {
    worker.begin_leaf();
    if let Some(outcome) = adapter.scan_optimized_leaf(
        leaf,
        base,
        base_event_lookup,
        worker,
        &mut |publication, records| {
            if records
                .iter()
                .any(|record| !record.source.exact_descriptor_eq(leaf.source()))
            {
                return Err(CaptureError::InvalidPayload(
                    "optimized JSONL leaf emitted a record for another source".to_owned(),
                ));
            }
            output.emit_page(publication == JsonlFamilyPublication::Append, records)
        },
    )? {
        return validate_optimized_outcome(adapter, leaf, base, outcome);
    }

    let (leaf, opened) = leaf.open_for_scan()?;
    let previous = base.and_then(|base| decode_checkpoint(adapter, &leaf, base).ok());
    let previous_physical = previous.as_ref().filter(|checkpoint| {
        checkpoint.physical.terminal()
            && (checkpoint.physical.source_observation() == leaf.observation()
                || adapter.append_mode() == JsonlFamilyAppendMode::CertifiedSuffix)
    });
    let mut reader = if leaf.whole_record {
        JsonlReader::open_whole_record(
            physical_identity(adapter, &leaf),
            Arc::clone(&opened),
            previous_physical.map(|checkpoint| &checkpoint.physical),
        )
    } else {
        JsonlReader::open(
            physical_identity(adapter, &leaf),
            Arc::clone(&opened),
            previous_physical.map(|checkpoint| &checkpoint.physical),
            leaf.identity_probe.clone(),
        )
    }?;
    reader.set_oversized_record_policy(adapter.oversized_record_policy());

    if reader.source_change() == JsonlSourceChange::Unchanged {
        let base = base.ok_or_else(|| {
            CaptureError::InvalidPayload("unchanged JSONL source has no base".to_owned())
        })?;
        while reader
            .visit_page(&mut |_record| -> Result<()> { Ok(()) })?
            .is_some()
        {}
        let outcome = reader.outcome().ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL no-op scan has no terminal checkpoint".to_owned())
        })?;
        let decoded = previous.ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL no-op checkpoint is absent".to_owned())
        })?;
        if outcome.checkpoint() != &decoded.physical {
            return Err(CaptureError::InvalidPayload(
                "JSONL no-op checkpoint changed".to_owned(),
            ));
        }
        let frontier = base.frontier().ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL no-op base frontier is absent".to_owned())
        })?;
        let append = CertifiedSourceAppend::certify(
            base,
            base.clone(),
            frontier.certified_prefix_bytes(),
            *frontier.certified_prefix_digest(),
        )
        .map_err(contract_error)?;
        return Ok(PreparedLeaf {
            certificate: base.clone(),
            append: Some(append),
            checkpoint: Some(decoded.physical),
        });
    }

    let is_append = reader.source_change() == JsonlSourceChange::Append;
    if is_append && adapter.append_mode() != JsonlFamilyAppendMode::CertifiedSuffix {
        return Err(CaptureError::SystemInvariant(
            "replacement-only JSONL adapter resumed an append",
        ));
    }
    let resumed = if is_append {
        Some(previous.as_ref().ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL append checkpoint is absent".to_owned())
        })?)
    } else {
        None
    };
    let projection_mode = if is_append {
        JsonlFamilyProjectionMode::CertifiedAppend
    } else if base.is_some() {
        JsonlFamilyProjectionMode::Replacement
    } else {
        JsonlFamilyProjectionMode::Cold
    };
    let mut projector = adapter.projector_with_provider_checkpoint(
        &leaf,
        opened,
        DateTime::<Utc>::UNIX_EPOCH,
        resumed.and_then(|checkpoint| checkpoint.provider_checkpoint.as_ref()),
        base.is_some().then(|| base_event_lookup.clone()),
        projection_mode,
    )?;
    let mut physical_records = resumed.map_or_else(
        || {
            leaf.identity_probe
                .as_ref()
                .map(JsonlProbe::next_physical_ordinal)
                .unwrap_or(0)
        },
        |checkpoint| checkpoint.physical.next_physical_ordinal(),
    );
    let mut represented_records =
        resumed.map_or(0, |checkpoint| checkpoint.represented_physical_records);
    let mut documents = resumed.map_or(0, |checkpoint| checkpoint.indexed_documents);
    loop {
        let page = reader.visit_page(&mut |record| -> Result<()> {
            physical_records = checked_increment(physical_records)?;
            let before = documents;
            projector.project(record, worker, &mut |core_record| {
                if !core_record.source.exact_descriptor_eq(leaf.source()) {
                    return Err(CaptureError::InvalidPayload(
                        "JSONL projector changed the bound source".to_owned(),
                    ));
                }
                output.emit_record(is_append, core_record)?;
                documents = checked_increment(documents)?;
                Ok(())
            })?;
            if documents != before {
                represented_records = checked_increment(represented_records)?;
            }
            Ok(())
        })?;
        output.flush()?;
        if page.is_none() {
            break;
        }
    }
    let before_finish = documents;
    projector.finish_projecting(worker, &mut |core_record| {
        if !core_record.source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::InvalidPayload(
                "JSONL projector changed the bound source".to_owned(),
            ));
        }
        output.emit_record(is_append, core_record)?;
        documents = checked_increment(documents)?;
        Ok(())
    })?;
    output.flush()?;
    let rejected_records = resumed
        .map_or(leaf.identity_probe_rejected_records, |checkpoint| {
            checkpoint.rejected_records
        })
        .checked_add(projector.rejected_records())
        .ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL rejected count overflowed".to_owned())
        })?;
    let provider_checkpoint = projector.provider_checkpoint()?;
    if documents != before_finish {
        represented_records = physical_records;
    }
    let outcome = reader.outcome().ok_or_else(|| {
        CaptureError::InvalidPayload("JSONL replacement scan has no terminal checkpoint".to_owned())
    })?;
    if physical_records != outcome.checkpoint().next_physical_ordinal() {
        return Err(CaptureError::InvalidPayload(
            "JSONL physical record count did not reconcile".to_owned(),
        ));
    }
    let checkpoint = FamilyCheckpoint {
        version: FamilyCheckpoint::VERSION,
        provider_parser_revision: adapter.parser_revision().to_owned(),
        event_identity_revision: adapter.event_identity_revision().to_owned(),
        binding_digest: binding_digest(&leaf)?,
        physical: outcome.checkpoint().clone(),
        represented_physical_records: represented_records,
        rejected_records,
        indexed_documents: documents,
        provider_checkpoint,
    };
    let terminal_checkpoint = outcome.checkpoint().clone();
    let certificate = certify(adapter, &leaf, checkpoint)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let append = if is_append {
        let base = base.ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL append base is absent".to_owned())
        })?;
        let frontier = base.frontier().ok_or_else(|| {
            CaptureError::InvalidPayload("JSONL append base frontier is absent".to_owned())
        })?;
        Some(
            CertifiedSourceAppend::certify(
                base,
                certificate.clone(),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .map_err(contract_error)?,
        )
    } else {
        None
    };
    Ok(PreparedLeaf {
        certificate,
        append,
        checkpoint: Some(terminal_checkpoint),
    })
}

fn validate_optimized_outcome(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    outcome: JsonlFamilyOptimizedLeafOutcome,
) -> Result<PreparedLeaf> {
    outcome
        .certificate
        .validate_contract()
        .map_err(contract_error)?;
    leaf.source()
        .validate_exact_descriptor(outcome.certificate.observation().source())
        .map_err(contract_error)?;
    if outcome.certificate.parser_revision() != adapter.parser_revision() {
        return Err(CaptureError::InvalidPayload(
            "optimized JSONL leaf changed the parser revision".to_owned(),
        ));
    }
    if let Some(append) = outcome.append.as_ref() {
        let base = base.ok_or_else(|| {
            CaptureError::InvalidPayload("optimized JSONL append has no base".to_owned())
        })?;
        if append.base() != base || append.current() != &outcome.certificate {
            return Err(CaptureError::InvalidPayload(
                "optimized JSONL append evidence does not reconcile".to_owned(),
            ));
        }
    }
    Ok(PreparedLeaf {
        certificate: outcome.certificate,
        append: outcome.append,
        checkpoint: None,
    })
}

fn certify(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    checkpoint: FamilyCheckpoint,
) -> SourceBackedRouteResult<CertifiedSource> {
    if !checkpoint.valid_for(adapter, leaf) {
        return Err(route_invalid("JSONL checkpoint is internally inconsistent"));
    }
    let classified = checkpoint
        .represented_physical_records
        .checked_add(checkpoint.rejected_records)
        .ok_or_else(|| route_invalid("JSONL classified count overflowed"))?;
    let ignored = checkpoint
        .physical
        .next_physical_ordinal()
        .checked_sub(classified)
        .ok_or_else(|| route_invalid("JSONL ignored count underflowed"))?;
    let complete_records = checkpoint
        .indexed_documents
        .checked_add(checkpoint.rejected_records)
        .and_then(|records| records.checked_add(ignored))
        .ok_or_else(|| route_invalid("JSONL complete count overflowed"))?;
    let frontier = SourceFrontier::new(
        FAMILY_FRONTIER_KIND,
        TypedKey::bytes(serde_json::to_vec(&checkpoint).map_err(route_invalid)?)
            .map_err(route_invalid)?,
        checkpoint.physical.complete_prefix_end(),
        *checkpoint.physical.complete_prefix_sha256(),
    )
    .map_err(route_invalid)?;
    CertifiedSource::certify_with_frontier(
        source_observation(&leaf.source, &leaf.observation).map_err(route_invalid)?,
        source_observation(&leaf.source, &leaf.observation).map_err(route_invalid)?,
        adapter.parser_revision(),
        *checkpoint.physical.complete_prefix_sha256(),
        ScannedSourceCounts {
            complete_records,
            retained_records: checkpoint.indexed_documents,
            rejected_records: checkpoint.rejected_records,
            ignored_records: ignored,
            indexed_documents: checkpoint.indexed_documents,
            certified_bytes: checkpoint.physical.complete_prefix_end(),
        },
        Some(frontier),
    )
    .map_err(route_invalid)
}

fn decode_checkpoint(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    certificate: &CertifiedSource,
) -> Result<FamilyCheckpoint> {
    certificate.validate_contract().map_err(contract_error)?;
    leaf.source
        .validate_exact_descriptor(certificate.observation().source())
        .map_err(contract_error)?;
    if certificate.parser_revision() != adapter.parser_revision() {
        return Err(CaptureError::InvalidPayload(
            "JSONL base parser revision changed".to_owned(),
        ));
    }
    let frontier = certificate
        .frontier()
        .ok_or_else(|| CaptureError::InvalidPayload("JSONL base frontier is absent".to_owned()))?;
    if frontier.checkpoint_kind() != FAMILY_FRONTIER_KIND {
        return Err(CaptureError::InvalidPayload(
            "JSONL base frontier kind changed".to_owned(),
        ));
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint is malformed".to_owned(),
        ));
    };
    let checkpoint: FamilyCheckpoint = serde_json::from_slice(bytes)?;
    let classified = checkpoint
        .represented_physical_records
        .checked_add(checkpoint.rejected_records)
        .ok_or_else(|| CaptureError::InvalidPayload("JSONL base counts are invalid".to_owned()))?;
    let ignored = checkpoint
        .physical
        .next_physical_ordinal()
        .checked_sub(classified)
        .ok_or_else(|| CaptureError::InvalidPayload("JSONL base counts are invalid".to_owned()))?;
    let counts = certificate.counts();
    if !checkpoint.valid_for(adapter, leaf)
        || checkpoint.physical.complete_prefix_end() != frontier.certified_prefix_bytes()
        || checkpoint.physical.complete_prefix_sha256() != frontier.certified_prefix_digest()
        || checkpoint.physical.complete_prefix_sha256() != certificate.content_digest()
        || checkpoint.indexed_documents != counts.retained_records
        || checkpoint.indexed_documents != counts.indexed_documents
        || checkpoint.rejected_records != counts.rejected_records
        || ignored != counts.ignored_records
        || checkpoint
            .indexed_documents
            .checked_add(checkpoint.rejected_records)
            .and_then(|records| records.checked_add(ignored))
            != Some(counts.complete_records)
        || checkpoint.physical.complete_prefix_end() != counts.certified_bytes
        || certificate.observation()
            != &source_observation(&leaf.source, checkpoint.physical.source_observation())?
    {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint does not reconcile".to_owned(),
        ));
    }
    Ok(checkpoint)
}

fn preserve_coordinator_error(
    failure: &mut Option<SourceBackedRouteError>,
    error: crate::provider::source_backed::SourceBackedCoordinatorError,
) -> CaptureError {
    preserve_route_error(
        failure,
        crate::provider::source_backed::registration::route_coordinator_error(error),
    )
}

fn preserve_route_error(
    failure: &mut Option<SourceBackedRouteError>,
    error: SourceBackedRouteError,
) -> CaptureError {
    let detail = error.to_string();
    *failure = Some(error);
    CaptureError::InvalidPayload(detail)
}

fn preserve_parallel_emit_error(
    failure: &mut Option<SourceBackedRouteError>,
    error: crate::provider::source_backed::ParallelLeafScanEmitError,
) -> CaptureError {
    match error {
        crate::provider::source_backed::ParallelLeafScanEmitError::Route(error) => {
            preserve_route_error(failure, error)
        }
        crate::provider::source_backed::ParallelLeafScanEmitError::Cancelled(_) => {
            CaptureError::SystemInvariant("JSONL parallel scan was cancelled during replacement")
        }
    }
}

pub(super) fn physical_identity(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
) -> JsonlSourceIdentity {
    JsonlSourceIdentity::new(
        adapter.provider().as_str(),
        adapter.parser_revision(),
        FAMILY_POLICY_REVISION,
        leaf.source.exact_descriptor_digest(),
        leaf.source_path.clone(),
    )
}

pub(super) fn source_observation(
    source: &SourceKey,
    observation: &JsonlFileObservation,
) -> Result<SourceObservation> {
    SourceObservation::new(
        source.clone(),
        FAMILY_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )
    .map_err(contract_error)
}

fn base_for_leaf<'a>(
    bases: &'a HashMap<[u8; 32], &CertifiedSource>,
    leaf: &JsonlFamilyLeaf,
) -> Option<&'a CertifiedSource> {
    bases
        .get(&leaf.source().exact_descriptor_digest())
        .copied()
        .filter(|base| {
            base.observation()
                .source()
                .exact_descriptor_eq(leaf.source())
        })
}

fn map_parallel_leaf_error(
    error: ParallelLeafScanError<SourceBackedRouteError>,
) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanError::Worker { source, .. } => source,
        other => route_internal(other),
    }
}

pub(super) fn family_scanner_worker_count_policy(
    recommended: usize,
    requested_workers: Option<usize>,
) -> usize {
    if recommended == 0 {
        return 0;
    }
    requested_workers
        .unwrap_or(recommended)
        .clamp(1, recommended)
}

fn family_scanner_worker_count(recommended: usize) -> usize {
    #[cfg(test)]
    {
        super::FAMILY_SCANNER_WORKERS_OVERRIDE.with(|value| {
            family_scanner_worker_count_policy(recommended, Some(value.get().unwrap_or(1)))
        })
    }
    #[cfg(not(test))]
    {
        family_scanner_worker_count_policy(recommended, None)
    }
}

fn checked_increment(value: u64) -> Result<u64> {
    value.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "JSONL work counter overflowed",
    ))
}

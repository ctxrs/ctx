use std::{collections::HashMap, sync::Arc};

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
    binding_digest, contract_error, route_internal, route_invalid, FamilyCheckpoint,
    JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyLeaf, TerminalSourceEvidence,
    FAMILY_FRONTIER_KIND, FAMILY_POLICY_REVISION, FAMILY_SOURCE_REVISION_KIND,
};
#[cfg(test)]
use super::{jsonl_family_scanner_probe, record_jsonl_family_scanner_activity};
use crate::{
    provider::source_backed::{
        ParallelLeafScanBegin, ParallelLeafScanComplete, ParallelLeafScanError,
        ParallelLeafScanJob, ParallelLeafScanWorkerError, SourceBackedGenerationSink,
        SourceBackedRouteError, SourceBackedRouteResult,
    },
    CaptureError, Result,
};

struct PreparedLeaf {
    certificate: CertifiedSource,
    append: Option<CertifiedSourceAppend>,
    checkpoint: JsonlCheckpoint,
}

struct JsonlLeafJob {
    leaf: JsonlFamilyLeaf,
    base: Option<CertifiedSource>,
}

fn scan_leaf_serial(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    base_event_lookup: &BaseEventIdentityLookup,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<TerminalSourceEvidence> {
    let mut staging_started = false;
    let mut append_staging = false;
    let mut sink_failure = None;
    let prepared = prepare_leaf(
        adapter,
        leaf,
        base,
        base_event_lookup,
        &mut |append, core_records| {
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
            for record in core_records {
                sink.add_core_record(record)
                    .map_err(|error| preserve_coordinator_error(&mut sink_failure, error))?;
            }
            Ok(())
        },
    );
    if let Some(error) = sink_failure {
        return Err(error);
    }
    let prepared = prepared.map_err(route_invalid)?;

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
                checkpoint: Some(checkpoint),
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
                checkpoint: Some(checkpoint),
            })
        }
    }
}

pub(super) fn scan_leaves(
    adapter: &dyn JsonlFamilyAdapter,
    leaves: &[JsonlFamilyLeaf],
    bases: &HashMap<[u8; 32], &CertifiedSource>,
    base_event_lookup: BaseEventIdentityLookup,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<HashMap<[u8; 32], TerminalSourceEvidence>> {
    let worker_count = family_scanner_worker_count(sink.recommended_leaf_workers(leaves.len()));
    #[cfg(test)]
    let scanner_probe = jsonl_family_scanner_probe(worker_count);
    if worker_count <= 1 {
        let mut terminal_sources = HashMap::with_capacity(leaves.len());
        for leaf in leaves {
            #[cfg(test)]
            let _active_scanner = scanner_probe.as_ref().map(|probe| probe.enter());
            let evidence = scan_leaf_serial(
                adapter,
                leaf,
                base_for_leaf(bases, leaf),
                &base_event_lookup,
                sink,
            )?;
            if terminal_sources
                .insert(leaf.source().exact_descriptor_digest(), evidence)
                .is_some()
            {
                return Err(route_invalid("duplicate JSONL source identity"));
            }
        }
        #[cfg(test)]
        record_jsonl_family_scanner_activity(worker_count, scanner_probe.as_deref());
        return Ok(terminal_sources);
    }

    let jobs = leaves
        .iter()
        .cloned()
        .map(|leaf| {
            let base = base_for_leaf(bases, &leaf).cloned();
            ParallelLeafScanJob::new(leaf.source().clone(), JsonlLeafJob { leaf, base })
        })
        .collect::<Vec<_>>();
    let evidences = sink
        .run_parallel_leaf_scans(jobs, worker_count, |job, emitter| {
            #[cfg(test)]
            let _active_scanner = scanner_probe.as_ref().map(|probe| probe.enter());
            let leaf = &job.leaf().leaf;
            let mut staging_started = false;
            let mut append_staging = false;
            let mut emission_failure = None;
            let prepared = prepare_leaf(
                adapter,
                leaf,
                job.leaf().base.as_ref(),
                &base_event_lookup,
                &mut |append, core_records| {
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
                    for record in core_records {
                        emitter.emit_core_record(record).map_err(|error| {
                            preserve_parallel_emit_error(&mut emission_failure, error)
                        })?;
                    }
                    Ok(())
                },
            );
            if let Some(error) = emission_failure {
                return Err(ParallelLeafScanWorkerError::provider(error));
            }
            let prepared = prepared
                .map_err(route_invalid)
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
                                checkpoint: Some(checkpoint),
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
                        checkpoint: Some(checkpoint),
                    };
                    emitter
                        .complete(ParallelLeafScanComplete::replace(certificate, evidence))
                        .map_err(ParallelLeafScanWorkerError::from)?;
                }
            }
            Ok(())
        })
        .map_err(map_parallel_leaf_error);
    #[cfg(test)]
    record_jsonl_family_scanner_activity(worker_count, scanner_probe.as_deref());
    let evidences = evidences?;

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

fn prepare_leaf(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    base: Option<&CertifiedSource>,
    base_event_lookup: &BaseEventIdentityLookup,
    emit_page: &mut dyn FnMut(bool, Vec<CoreRecord>) -> Result<()>,
) -> Result<PreparedLeaf> {
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
            checkpoint: decoded.physical,
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
    let mut projector = adapter.projector_with_provider_checkpoint(
        &leaf,
        opened,
        DateTime::<Utc>::UNIX_EPOCH,
        resumed.and_then(|checkpoint| checkpoint.provider_checkpoint.as_ref()),
        is_append.then(|| base_event_lookup.clone()),
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
        let mut page_core_records = Vec::new();
        let page = reader.visit_page(&mut |record| -> Result<()> {
            physical_records = checked_increment(physical_records)?;
            let before = documents;
            projector.project(record, &mut |core_record| {
                if !core_record.source.exact_descriptor_eq(leaf.source()) {
                    return Err(CaptureError::InvalidPayload(
                        "JSONL projector changed the bound source".to_owned(),
                    ));
                }
                page_core_records.push(core_record);
                documents = checked_increment(documents)?;
                Ok(())
            })?;
            if documents != before {
                represented_records = checked_increment(represented_records)?;
            }
            Ok(())
        })?;
        if !page_core_records.is_empty() {
            emit_page(is_append, page_core_records)?;
        }
        if page.is_none() {
            break;
        }
    }
    let before_finish = documents;
    let mut final_core_records = Vec::new();
    projector.finish_projecting(&mut |core_record| {
        if !core_record.source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::InvalidPayload(
                "JSONL projector changed the bound source".to_owned(),
            ));
        }
        final_core_records.push(core_record);
        documents = checked_increment(documents)?;
        Ok(())
    })?;
    if !final_core_records.is_empty() {
        emit_page(is_append, final_core_records)?;
    }
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
        checkpoint: terminal_checkpoint,
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

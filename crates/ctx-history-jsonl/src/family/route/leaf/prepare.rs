use super::*;

#[cfg(test)]
pub(in super::super) fn prepare_leaf<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    base: Option<&CertifiedSource>,
    base_event_lookup: &JsonlRuntimeLookup<R>,
    worker: &mut JsonlFamilyWorkerContext<R>,
    output: &mut JsonlLeafOutput<'_, JsonlRuntimeError<R>>,
    append_only_trust_allowed: bool,
) -> JsonlResult<PreparedLeaf<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    let resources = ctx_history_capture_runtime::SourceBackedRouteResources::production(1);
    prepare_leaf_with_resources(
        adapter,
        leaf,
        base,
        base_event_lookup,
        worker,
        output,
        append_only_trust_allowed,
        &resources,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn prepare_leaf_with_resources<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    base: Option<&CertifiedSource>,
    base_event_lookup: &JsonlRuntimeLookup<R>,
    worker: &mut JsonlFamilyWorkerContext<R>,
    output: &mut JsonlLeafOutput<'_, JsonlRuntimeError<R>>,
    append_only_trust_allowed: bool,
    route_resources: &ctx_history_capture_runtime::SourceBackedRouteResources,
) -> JsonlResult<PreparedLeaf<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    worker.begin_leaf();
    let optimized_outcome = if append_only_trust_allowed
        || adapter.append_trust_contract()
            != super::super::JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
    {
        adapter.scan_optimized_leaf(
            leaf,
            base,
            base_event_lookup,
            worker,
            &mut |publication, completed_bytes, records| {
                if records
                    .iter()
                    .any(|record| !record.source.exact_descriptor_eq(leaf.source()))
                {
                    return Err(JsonlRuntimeError::<R>::invalid_payload(
                        "optimized JSONL leaf emitted a record for another source".to_owned(),
                    ));
                }
                output.emit_page(
                    publication == JsonlFamilyPublication::Append,
                    completed_bytes,
                    records,
                )
            },
        )?
    } else {
        None
    };
    if let Some(outcome) = optimized_outcome {
        return validate_optimized_outcome(adapter, leaf, base, outcome);
    }

    let (leaf, opened) = leaf.open_for_scan()?;
    let append_mode = adapter.append_mode();
    let projector_preflight = matches!(append_mode, JsonlFamilyAppendMode::ProjectorPreflight(_));
    if projector_preflight && leaf.identity_probe.is_some() {
        return Err(JsonlRuntimeError::<R>::system_invariant(
            "JSONL projector preflight cannot follow an identity probe",
        ));
    }
    let previous = base.and_then(|base| decode_checkpoint(adapter, &leaf, base).ok());
    // A nonterminal checkpoint still certifies every complete record before
    // its unfinished tail. Reuse it for an exact no-op, or let append-capable
    // adapters resume at that complete frontier so the unfinished bytes are
    // reconsidered without replaying already certified records.
    let previous_physical = previous.as_ref().filter(|checkpoint| {
        checkpoint.physical.source_observation() == leaf.observation()
            || (checkpoint
                .physical
                .source_observation()
                .differs_only_by_change_identity(leaf.observation())
                && checkpoint.authenticates_admitted_eof())
            || append_mode.certified_suffix()
    });
    let open_reader = |previous| {
        open_leaf_reader(
            adapter,
            &leaf,
            &opened,
            previous,
            projector_preflight,
            append_only_trust_allowed,
            route_resources,
        )
    };
    let mut reader = open_reader(previous_physical)?;

    if reader.source_change() == JsonlSourceChange::Unchanged {
        let base = base.ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("unchanged JSONL source has no base".to_owned())
        })?;
        while reader
            .visit_page(&mut |_record| -> JsonlResult<(), JsonlRuntimeError<R>> { Ok(()) })?
            .is_some()
        {}
        let outcome = reader.outcome().ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL no-op scan has no terminal checkpoint".to_owned(),
            )
        })?;
        let decoded = previous.ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL no-op checkpoint is absent".to_owned())
        })?;
        if outcome.checkpoint() != &decoded.physical {
            return Err(JsonlRuntimeError::<R>::invalid_payload(
                "JSONL no-op checkpoint changed".to_owned(),
            ));
        }
        let frontier = base.frontier().ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL no-op base frontier is absent".to_owned(),
            )
        })?;
        let append = CertifiedSourceAppend::certify(
            base,
            base.clone(),
            frontier.certified_prefix_bytes(),
            *frontier.certified_prefix_digest(),
        )
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
        return Ok(PreparedLeaf {
            certificate: base.clone(),
            append: Some(append),
            terminal_proof: terminal_proof_for_checkpoint(
                adapter,
                &leaf,
                base,
                &decoded,
                append_only_trust_allowed,
            )?,
            record_rejections: SourceBackedRecordRejectionDrafts::default(),
            logical_source_quarantine: None,
        });
    }

    if reader.source_change() == JsonlSourceChange::Append
        && previous
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.complete_prefix_ends_with_terminal_nul_padding)
    {
        reader = open_reader(None)?;
    }
    let mut is_append = reader.source_change() == JsonlSourceChange::Append;
    if is_append && !append_mode.certified_suffix() {
        return Err(JsonlRuntimeError::<R>::system_invariant(
            "replacement-only JSONL adapter resumed an append",
        ));
    }
    let mut resumed = if is_append {
        Some(previous.as_ref().ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL append checkpoint is absent".to_owned())
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
    if let Some(mut executor) = adapter.semantic_executor(
        &leaf,
        resumed.and_then(|checkpoint| checkpoint.provider_checkpoint.as_ref()),
        base.is_some().then(|| base_event_lookup.clone()),
        projection_mode,
    )? {
        let mut input = JsonlFamilyExecutionIo::new(reader);
        let preflight_start = input.position()?;
        let preflight = executor.preflight(&mut input)?;
        let physical_ready = match preflight {
            JsonlFamilySemanticPreflight::Ready => input.settle_preflight(preflight_start)?,
            JsonlFamilySemanticPreflight::RetryReplacement if is_append => false,
            JsonlFamilySemanticPreflight::RetryReplacement => {
                return Err(JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL semantic executor requested replacement outside append preflight"
                        .to_owned(),
                ));
            }
        };
        match (preflight, physical_ready) {
            (JsonlFamilySemanticPreflight::Ready, true) => {}
            (JsonlFamilySemanticPreflight::RetryReplacement, _) | (_, false) if is_append => {
                drop(input);
                input = JsonlFamilyExecutionIo::new(open_reader(None)?);
                executor = adapter
                    .semantic_executor(
                        &leaf,
                        None,
                        Some(base_event_lookup.clone()),
                        JsonlFamilyProjectionMode::Replacement,
                    )?
                    .ok_or_else(|| {
                        JsonlRuntimeError::<R>::system_invariant(
                            "JSONL semantic executor disappeared for replacement retry",
                        )
                    })?;
                let replacement_start = input.position()?;
                let replacement_preflight = executor.preflight(&mut input)?;
                if replacement_preflight != JsonlFamilySemanticPreflight::Ready {
                    return Err(JsonlRuntimeError::<R>::invalid_payload(
                        "JSONL semantic executor requested more than one replacement retry"
                            .to_owned(),
                    ));
                }
                let replacement_physical_ready = input.settle_preflight(replacement_start)?;
                if !replacement_physical_ready {
                    return Err(JsonlRuntimeError::<R>::invalid_payload(
                        "JSONL semantic executor requested more than one replacement retry"
                            .to_owned(),
                    ));
                }
                return prepare_semantic_leaf(
                    adapter,
                    &leaf,
                    SemanticLeafPlan {
                        base,
                        resumed: None,
                        is_append: false,
                        append_only_trust_allowed,
                    },
                    worker,
                    output,
                    SemanticLeafExecution { executor, input },
                );
            }
            (_, false) => {
                return Err(JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL semantic executor requested replacement outside append preflight"
                        .to_owned(),
                ));
            }
            (JsonlFamilySemanticPreflight::RetryReplacement, true) => {
                return Err(JsonlRuntimeError::<R>::system_invariant(
                    "JSONL replacement retry was marked physically ready",
                ));
            }
        }
        return prepare_semantic_leaf(
            adapter,
            &leaf,
            SemanticLeafPlan {
                base,
                resumed,
                is_append,
                append_only_trust_allowed,
            },
            worker,
            output,
            SemanticLeafExecution { executor, input },
        );
    }
    let mut projector = adapter.projector_with_provider_checkpoint(
        &leaf,
        opened,
        DateTime::<Utc>::UNIX_EPOCH,
        resumed.and_then(|checkpoint| checkpoint.provider_checkpoint.as_ref()),
        base.is_some().then(|| base_event_lookup.clone()),
        projection_mode,
    )?;
    if projector_preflight {
        let initial = reader.execution_position()?;
        let retry = projector.preflight(
            &mut reader,
            resumed.map(|checkpoint| checkpoint.physical.complete_prefix_end()),
        )?;
        let physical_ready = reader.settle_semantic_preflight(initial, !retry, true)?;
        if (retry || !physical_ready) && !is_append {
            return Err(JsonlRuntimeError::<R>::system_invariant(
                "JSONL projector replaced a non-append",
            ));
        }
        if retry || !physical_ready {
            projector.retry_replacement();
            resumed = None;
            is_append = false;
        }
    }
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
        let page = reader.visit_page(&mut |record| -> JsonlResult<(), JsonlRuntimeError<R>> {
            physical_records = checked_increment::<JsonlRuntimeError<R>>(physical_records)?;
            let before = documents;
            projector.project(record, worker, &mut |core_record| {
                if !core_record.source.exact_descriptor_eq(leaf.source()) {
                    return Err(JsonlRuntimeError::<R>::invalid_payload(
                        "JSONL projector changed the bound source".to_owned(),
                    ));
                }
                output.emit_record(is_append, core_record)?;
                documents = checked_increment::<JsonlRuntimeError<R>>(documents)?;
                Ok(())
            })?;
            if documents != before {
                represented_records =
                    checked_increment::<JsonlRuntimeError<R>>(represented_records)?;
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
            return Err(JsonlRuntimeError::<R>::invalid_payload(
                "JSONL projector changed the bound source".to_owned(),
            ));
        }
        output.emit_record(is_append, core_record)?;
        documents = checked_increment::<JsonlRuntimeError<R>>(documents)?;
        Ok(())
    })?;
    output.flush()?;
    let rejected_records = resumed
        .map_or(leaf.identity_probe_rejected_records, |checkpoint| {
            checkpoint.rejected_records
        })
        .checked_add(projector.rejected_records())
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL rejected count overflowed".to_owned())
        })?;
    let record_rejections = projector.take_record_rejections();
    let provider_checkpoint = projector.provider_checkpoint()?;
    if documents != before_finish {
        represented_records = physical_records;
    }
    let classified_physical_records = represented_records
        .checked_add(rejected_records)
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL classified physical count overflowed".to_owned(),
            )
        })?;
    let physical_ignored_records = physical_records
        .checked_sub(classified_physical_records)
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL classified physical count exceeded physical records".to_owned(),
            )
        })?;
    let logical_complete_records = documents
        .checked_add(rejected_records)
        .and_then(|count| count.checked_add(physical_ignored_records))
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL logical complete count overflowed".to_owned(),
            )
        })?;
    let admitted_eof_sha256 = reader.admitted_eof_sha256()?;
    let complete_prefix_ends_with_terminal_nul_padding =
        reader.complete_prefix_ends_with_terminal_nul_padding();
    let outcome = reader.outcome().ok_or_else(|| {
        JsonlRuntimeError::<R>::invalid_payload(
            "JSONL replacement scan has no terminal checkpoint".to_owned(),
        )
    })?;
    if physical_records != outcome.checkpoint().next_physical_ordinal() {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL physical record count did not reconcile".to_owned(),
        ));
    }
    let checkpoint = FamilyCheckpoint {
        version: FamilyCheckpoint::VERSION,
        provider_parser_revision: adapter.parser_revision().to_owned(),
        event_identity_revision: adapter.event_identity_revision().to_owned(),
        binding_digest: binding_digest(&leaf)?,
        physical: outcome.checkpoint().clone(),
        admitted_eof_sha256,
        complete_prefix_ends_with_terminal_nul_padding,
        represented_physical_records: represented_records,
        rejected_records,
        logical_complete_records,
        rejected_logical_records: rejected_records,
        indexed_documents: documents,
        provider_checkpoint,
    };
    let checkpoint = fit_semantic_provider_checkpoint(adapter, checkpoint)?;
    let certificate = certify(adapter, &leaf, checkpoint.clone())
        .map_err(|error| JsonlRuntimeError::<R>::invalid_payload(error.to_string()))?;
    let append = if is_append {
        let base = base.ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL append base is absent".to_owned())
        })?;
        let frontier = base.frontier().ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL append base frontier is absent".to_owned(),
            )
        })?;
        Some(
            CertifiedSourceAppend::certify(
                base,
                certificate.clone(),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .map_err(contract_error::<JsonlRuntimeError<R>>)?,
        )
    } else {
        None
    };
    let terminal_proof = terminal_proof_for_checkpoint(
        adapter,
        &leaf,
        &certificate,
        &checkpoint,
        append_only_trust_allowed,
    )?;
    Ok(PreparedLeaf {
        certificate,
        append,
        terminal_proof,
        record_rejections,
        logical_source_quarantine: None,
    })
}

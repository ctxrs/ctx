use super::*;

pub(super) struct SemanticLeafPlan<'a> {
    pub(super) base: Option<&'a CertifiedSource>,
    pub(super) resumed: Option<&'a FamilyCheckpoint>,
    pub(super) is_append: bool,
    pub(super) append_only_trust_allowed: bool,
}

pub(super) struct SemanticLeafExecution<R: JsonlFamilyRuntime> {
    pub(super) executor: Box<dyn JsonlFamilySemanticExecutor<Runtime = R>>,
    pub(super) input: JsonlFamilyExecutionIo<R>,
}

pub(super) fn prepare_semantic_leaf<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    plan: SemanticLeafPlan<'_>,
    worker: &mut JsonlFamilyWorkerContext<R>,
    output: &mut JsonlLeafOutput<'_, JsonlRuntimeError<R>>,
    execution: SemanticLeafExecution<R>,
) -> JsonlResult<PreparedLeaf<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    let SemanticLeafPlan {
        base,
        resumed,
        is_append,
        append_only_trust_allowed,
    } = plan;
    let SemanticLeafExecution {
        mut executor,
        mut input,
    } = execution;
    let initial_ordinal = resumed.map_or_else(
        || {
            leaf.identity_probe
                .as_ref()
                .map(JsonlProbe::next_physical_ordinal)
                .unwrap_or(0)
        },
        |checkpoint| checkpoint.physical.next_physical_ordinal(),
    );
    let mut documents = resumed.map_or(0, |checkpoint| checkpoint.indexed_documents);
    let mut reported_prefix_end = input.complete_prefix_end()?;
    while let Some(page) = executor.next_page(&mut input, worker)? {
        let complete_prefix_end = input.complete_prefix_end()?;
        let completed_bytes = complete_prefix_end
            .checked_sub(reported_prefix_end)
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL semantic physical progress regressed".to_owned(),
                )
            })?;
        reported_prefix_end = complete_prefix_end;
        let records = page.into_bounded_records::<JsonlRuntimeError<R>>()?;
        if records
            .iter()
            .any(|record| !record.source.exact_descriptor_eq(leaf.source()))
        {
            return Err(JsonlRuntimeError::<R>::invalid_payload(
                "JSONL semantic executor changed the bound source".to_owned(),
            ));
        }
        documents = documents
            .checked_add(u64::try_from(records.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL document count overflowed".to_owned(),
                )
            })?;
        input.release_record_buffer()?;
        output.emit_page(is_append, completed_bytes, records)?;
    }
    input.release_record_buffer()?;
    let summary = executor.finish()?;
    let admitted_eof_sha256 = input.admitted_eof_sha256()?;
    let complete_prefix_ends_with_terminal_nul_padding =
        input.complete_prefix_ends_with_terminal_nul_padding();
    let reader = input.into_reader();
    let outcome = reader.outcome().ok_or_else(|| {
        JsonlRuntimeError::<R>::invalid_payload(
            "JSONL semantic scan has no terminal checkpoint".to_owned(),
        )
    })?;
    let terminal_checkpoint = outcome.checkpoint().clone();
    let scanned_physical_records = terminal_checkpoint
        .next_physical_ordinal()
        .checked_sub(initial_ordinal)
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL semantic ordinal regressed".to_owned())
        })?;
    let classified = summary
        .represented_physical_records()
        .checked_add(summary.rejected_records())
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL semantic classified count overflowed".to_owned(),
            )
        })?;
    if classified > scanned_physical_records {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL semantic classified count exceeds physical records".to_owned(),
        ));
    }
    let represented_physical_records = resumed
        .map_or(0, |checkpoint| checkpoint.represented_physical_records)
        .checked_add(summary.represented_physical_records())
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL represented count overflowed".to_owned())
        })?;
    let rejected_records = resumed
        .map_or(leaf.identity_probe_rejected_records, |checkpoint| {
            checkpoint.rejected_records
        })
        .checked_add(summary.rejected_records())
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL rejected count overflowed".to_owned())
        })?;
    let logical_complete_records = if let Some(current) = summary.logical_complete_records() {
        resumed
            .map_or(0, |checkpoint| checkpoint.logical_complete_records)
            .checked_add(current)
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL logical complete count overflowed".to_owned(),
                )
            })?
    } else {
        let classified_physical_records = represented_physical_records
            .checked_add(rejected_records)
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL classified physical count overflowed".to_owned(),
                )
            })?;
        let physical_ignored_records = terminal_checkpoint
            .next_physical_ordinal()
            .checked_sub(classified_physical_records)
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL classified physical count exceeded physical records".to_owned(),
                )
            })?;
        documents
            .checked_add(rejected_records)
            .and_then(|count| count.checked_add(physical_ignored_records))
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL logical complete count overflowed".to_owned(),
                )
            })?
    };
    let rejected_logical_records = if let Some(current) = summary.rejected_logical_records() {
        resumed
            .map_or(0, |checkpoint| checkpoint.rejected_logical_records)
            .checked_add(current)
            .ok_or_else(|| {
                JsonlRuntimeError::<R>::invalid_payload(
                    "JSONL logical rejected count overflowed".to_owned(),
                )
            })?
    } else {
        rejected_records
    };
    let logical_source_quarantine = summary.logical_source_quarantine().cloned();
    let provider_checkpoint = summary.provider_checkpoint();
    let record_rejections = summary.into_record_rejections();
    let checkpoint = FamilyCheckpoint {
        version: FamilyCheckpoint::VERSION,
        provider_parser_revision: adapter.parser_revision().to_owned(),
        event_identity_revision: adapter.event_identity_revision().to_owned(),
        binding_digest: continuation_binding_digest(leaf)?,
        exact_terminal_binding_digest: exact_terminal_binding_digest(leaf)?,
        physical: terminal_checkpoint.clone(),
        admitted_eof_sha256,
        complete_prefix_ends_with_terminal_nul_padding,
        represented_physical_records,
        rejected_records,
        logical_complete_records,
        rejected_logical_records,
        indexed_documents: documents,
        provider_checkpoint,
    };
    let checkpoint = fit_semantic_provider_checkpoint(adapter, checkpoint)?;
    let certificate = certify(adapter, leaf, checkpoint.clone())
        .map_err(|error| JsonlRuntimeError::<R>::invalid_payload(error.to_string()))?;
    let append = if is_append {
        let base = base.ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL semantic append base is absent".to_owned(),
            )
        })?;
        let frontier = base.frontier().ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL semantic append frontier is absent".to_owned(),
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
        leaf,
        &certificate,
        &checkpoint,
        append_only_trust_allowed,
    )?;
    Ok(PreparedLeaf {
        certificate,
        append,
        terminal_proof,
        record_rejections,
        logical_source_quarantine,
    })
}

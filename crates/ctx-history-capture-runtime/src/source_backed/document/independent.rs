use super::*;

enum IndependentDocumentLeaf<'leaf, L> {
    Replay {
        base: Box<CertifiedSource>,
    },
    Changed {
        observed: &'leaf ObservedDocumentLeaf<L>,
        logical_base: Option<Box<CertifiedSource>>,
        unsafe_base_transition: bool,
    },
}

pub(super) fn scan_document_leaves_independently<A>(
    adapter: &A,
    tree: &CompleteDocumentTree<A::Leaf, A::TreeAuthority>,
    base_sources: &[CertifiedSource],
    mut replayable: HashMap<DocumentLeafFingerprint, CertifiedSource>,
    parser_revision: &'static str,
    worker_count: usize,
    sink: &mut SourceBackedGenerationSink<'_, A::Lifecycle>,
) -> SourceBackedRouteResult<(CurrentDocumentSources, Vec<CertifiedSource>)>
where
    A: ReplacementDocumentTree,
{
    let mut planned_sources = CurrentDocumentSources::with_capacity(tree.leaves.len());
    let base_by_source = base_sources
        .iter()
        .filter(|source| adapter.owns_source(source.observation().source()))
        .map(|source| {
            (
                source.observation().source().identity().digest(),
                source.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut jobs = Vec::with_capacity(tree.leaves.len());
    for observed in &tree.leaves {
        let replay = exact_replay_for_observed(observed, &mut replayable);
        let (source, leaf) = if let Some(base) = replay {
            (
                base.observation().source().clone(),
                IndependentDocumentLeaf::Replay {
                    base: Box::new(base),
                },
            )
        } else {
            let source = match observed.bound_replay_source.as_ref() {
                Some(source) => source.clone(),
                None => {
                    adapter.independent_leaf_source(&tree.authority, &observed.provider_leaf)?
                }
            };
            let canonical_base = base_by_source.get(&source.identity().digest()).cloned();
            let logical_base = canonical_base
                .as_ref()
                .filter(|base| base.observation().source().exact_descriptor_eq(&source))
                .cloned()
                .map(Box::new);
            let unsafe_base_transition = canonical_base.is_some() && logical_base.is_none();
            (
                source,
                IndependentDocumentLeaf::Changed {
                    observed,
                    logical_base,
                    unsafe_base_transition,
                },
            )
        };
        validate_current_document_source(adapter, &mut planned_sources, source.clone())?;
        jobs.push(ParallelLeafScanJob::new(source, leaf));
    }

    // Jobs and returned certificates retain discovery order. The runner may
    // interleave bounded staging messages, but the writer canonicalizes source
    // publication and the family certifies one ordered complete inventory.
    let outcomes = sink
        .run_parallel_leaf_scans_with_source_outcomes(jobs, worker_count, |job, emitter| match job
            .leaf()
        {
            IndependentDocumentLeaf::Replay { base } => {
                let append = exact_document_replay_append(base)
                    .map_err(ParallelLeafScanWorkerError::provider)?;
                emitter.begin(ParallelLeafScanBegin::append(
                    job.source().clone(),
                    base.as_ref().clone(),
                ))?;
                emitter.complete(ParallelLeafScanComplete::append(
                    append,
                    DocumentLeafCompletion::replay(base.as_ref().clone()),
                ))?;
                Ok(())
            }
            IndependentDocumentLeaf::Changed {
                observed,
                logical_base,
                unsafe_base_transition,
            } => scan_independent_document_leaf(
                IndependentDocumentScanContext {
                    adapter,
                    authority: &tree.authority,
                    observed,
                    parser_revision,
                    expected_source: job.source(),
                    logical_base: logical_base.as_deref(),
                    unsafe_base_transition: *unsafe_base_transition,
                },
                emitter,
            ),
        })
        .map_err(document_parallel_error)?;
    let mut certificates = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            SourceBackedSourceOutcome::Success(completion) => {
                sink.record_rejections(completion.record_rejections);
                certificates.push(completion.certificate);
            }
            SourceBackedSourceOutcome::Failed(mut failure) => {
                let source = &failure.source;
                if !planned_sources.contains_exact(source)
                    || !failure.failure.kind.is_logical_source_failure()
                {
                    return Err(document_internal(
                        "independent document source outcome no longer matches its plan",
                    ));
                }
                sink.record_failed_attempt_rejections(std::mem::take(
                    &mut failure.record_rejections,
                ));
                if let Some(retained) = failure.retained {
                    certificates.push(retained);
                }
            }
        }
    }
    let mut current_sources = CurrentDocumentSources::with_capacity(certificates.len());
    for certificate in &certificates {
        validate_current_document_source(
            adapter,
            &mut current_sources,
            certificate.observation().source().clone(),
        )?;
    }
    Ok((current_sources, certificates))
}

struct IndependentDocumentScanContext<'scan, A>
where
    A: ReplacementDocumentTree,
{
    adapter: &'scan A,
    authority: &'scan A::TreeAuthority,
    observed: &'scan ObservedDocumentLeaf<A::Leaf>,
    parser_revision: &'static str,
    expected_source: &'scan SourceKey,
    logical_base: Option<&'scan CertifiedSource>,
    unsafe_base_transition: bool,
}

fn scan_independent_document_leaf<A>(
    context: IndependentDocumentScanContext<'_, A>,
    emitter: &mut ParallelLeafScanEmitter<
        '_,
        DocumentLeafCompletion,
        SourceBackedRouteError,
        <A::Lifecycle as CaptureLifecycleSink>::Preparation,
    >,
) -> Result<(), ParallelLeafScanWorkerError<SourceBackedRouteError>>
where
    A: ReplacementDocumentTree,
{
    let IndependentDocumentScanContext {
        adapter,
        authority,
        observed,
        parser_revision,
        expected_source,
        logical_base,
        unsafe_base_transition,
    } = context;
    let (scan_result, record_rejections) = {
        // Independent workers must complete their scans without waiting for
        // the deterministic writer lane assigned to an earlier leaf. Stage
        // each bounded leaf privately, then replay it in discovery order.
        let mut changed = Some(
            ChangedDocumentSink::parallel_logical(emitter, logical_base.cloned())
                .map_err(ParallelLeafScanWorkerError::provider)?,
        );
        let scan_result = (|| {
            let active = changed
                .as_mut()
                .ok_or_else(|| document_internal("document leaf sink was consumed early"))?;
            let terminal = adapter.scan_changed(authority, &observed.provider_leaf, active)?;
            if terminal.parser_revision != parser_revision {
                return Err(document_changed(
                    "document adapter terminal used an unexpected parser revision",
                ));
            }
            if !active.source()?.exact_descriptor_eq(expected_source) {
                return Err(document_changed(
                    "independent document leaf derived a different exact source",
                ));
            }
            let replay_fingerprint = observed
                .replay_from_frontier
                .then_some(observed.fingerprint);
            let append_base = adapter.append_base(authority, &observed.provider_leaf);
            let terminal =
                active.preflight_terminal(terminal, replay_fingerprint, append_base.as_ref())?;
            changed
                .take()
                .ok_or_else(|| document_internal("document leaf sink was consumed early"))?
                .finish(terminal, append_base)
        })();
        let record_rejections = changed
            .as_mut()
            .map(ChangedDocumentSink::take_record_rejections)
            .unwrap_or_default();
        (scan_result, record_rejections)
    };
    let certificate = match scan_result {
        Ok(certificate) => certificate,
        Err(_) if emitter.is_cancelled() => {
            return Err(ParallelLeafScanCancelled.into());
        }
        Err(error) if error.kind.is_logical_source_failure() && unsafe_base_transition => {
            return Err(ParallelLeafScanWorkerError::provider(document_changed(
                "failed document replacement has an unsafe source descriptor transition",
            )));
        }
        Err(error) if error.kind.is_logical_source_failure() => {
            let retained = logical_base
                .filter(|base| {
                    base.observation()
                        .source()
                        .exact_descriptor_eq(expected_source)
                })
                .cloned();
            emitter.complete(ParallelLeafScanComplete::source_failure_with_rejections(
                expected_source.clone(),
                retained,
                error,
                record_rejections,
            ))?;
            return Ok(());
        }
        Err(error) => return Err(ParallelLeafScanWorkerError::provider(error)),
    };
    let _ = certificate;
    Ok(())
}

use super::*;

#[cfg(test)]
pub(super) fn ingest_codex_source_backed_v0(
    session_root: impl AsRef<Path>,
    global_index_root: impl AsRef<Path>,
) -> CodexSourceBackedResultV0<CodexSourceBackedIngestReceiptV0> {
    ingest_codex_source_backed_inner_v0(
        session_root.as_ref(),
        global_index_root.as_ref(),
        ColdParallelOptionsV0::default(),
    )
}

#[cfg(test)]
pub(super) fn ingest_codex_source_backed_inner_v0(
    session_root: &Path,
    global_index_root: &Path,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<CodexSourceBackedIngestReceiptV0> {
    let total_started = Instant::now();
    let mut timings = CodexSourceBackedPhaseTimingsV0::default();
    let mut counters = CodexSourceBackedCountersV0::default();

    let writer_options = WriterOptions::default();
    let phase_started = Instant::now();
    let mut writer = GenerationWriter::open(global_index_root, writer_options.clone())?;
    timings.writer_open = phase_started.elapsed();
    let base_sources = writer_base_sources(&writer);
    let session_roots = vec![session_root.to_path_buf()];
    let phase_started = Instant::now();
    let opening_inventory =
        discover_codex_session_tree_inventory_from_base_v0(&session_roots, &base_sources)?;
    timings.discovery = phase_started.elapsed();
    let opening_certificate = opening_inventory.certificate.clone();
    counters.add_catalog_work(opening_inventory.work);
    let mut revalidation = HashMap::<SourceKey, CodexTerminalSourceEvidenceV0>::new();

    ingest_codex_sources_with_options_v0(
        opening_inventory.sources.clone(),
        &base_sources,
        &mut writer,
        &mut revalidation,
        &mut timings,
        &mut counters,
        writer_options.indexer_threads,
        cold_options,
    )?;

    for base in base_sources.values() {
        let source = base.observation().source();
        if managed_codex_session_source(source) && !opening_certificate.contains(source) {
            let deletion =
                CertifiedSourceDeletion::from_inventory(source.clone(), &opening_certificate)?;
            writer.delete_source(deletion, opening_certificate.clone())?;
            counters.deleted_sources = counters.deleted_sources.saturating_add(1);
        }
    }

    #[cfg(test)]
    if let Some(hook) = cold_options.before_commit_revalidation {
        hook(session_root);
    }

    // An empty first generation has no source or deletion target through which
    // GenerationWriter can invoke its terminal callback. Revalidate that
    // degenerate inventory explicitly; every non-empty refresh is fenced
    // inside prepare_commit below.
    if revalidation.is_empty() && counters.deleted_sources == 0 {
        let closing = discover_codex_session_tree_inventory_from_plans_v0(
            &session_roots,
            &opening_inventory,
        )?;
        counters.add_catalog_work(closing.work);
        if closing.certificate != opening_certificate {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
    }

    let commit_started = Instant::now();
    let mut closing_inventory = None::<Option<CodexSessionTreeInventoryV0>>;
    let commit = writer.commit(|target| {
        if closing_inventory.is_none() {
            closing_inventory = Some(
                discover_codex_session_tree_inventory_from_plans_v0(
                    &session_roots,
                    &opening_inventory,
                )
                .ok()
                .and_then(|closing| {
                    counters.add_catalog_work(closing.work);
                    (closing.certificate == opening_certificate).then_some(closing)
                }),
            );
        }
        let Some(closing) = closing_inventory
            .as_ref()
            .and_then(std::option::Option::as_ref)
        else {
            return false;
        };
        match target {
            RevalidationTarget::Source(certificate) => revalidation
                .get_key_value(certificate.observation().source())
                .is_some_and(|(source_key, evidence)| {
                    closing.certificate.contains(source_key)
                        && source_key.exact_descriptor_eq(certificate.observation().source())
                        && evidence.revalidate()
                }),
            RevalidationTarget::Deletion(deletion) => deletion.verifies(&closing.certificate),
        }
    })?;
    timings.commit = commit_started.elapsed();
    timings.total = total_started.elapsed();
    Ok(CodexSourceBackedIngestReceiptV0 {
        commit,
        timings,
        counters,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn ingest_codex_sources_v0(
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    base_sources: &HashMap<SourceKey, CertifiedSource>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
    indexer_threads: usize,
    scanner_workers: Option<usize>,
) -> CodexSourceBackedResultV0<()> {
    ingest_codex_sources_with_options_v0(
        sources,
        base_sources,
        writer,
        revalidation,
        timings,
        counters,
        indexer_threads,
        ColdParallelOptionsV0 {
            scanner_workers,
            #[cfg(test)]
            fail_source_index: None,
            #[cfg(test)]
            before_commit_revalidation: None,
            #[cfg(test)]
            scanner_rendezvous: None,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn ingest_codex_sources_with_options_v0(
    mut sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    base_sources: &HashMap<SourceKey, CertifiedSource>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
    indexer_threads: usize,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<()> {
    counters.catalog_sources =
        u64::try_from(sources.len()).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    counters.catalog_source_bytes = sources.iter().fold(0_u64, |total, (source, _, _)| {
        total.saturating_add(source.catalog_observation.len)
    });
    sources.sort_by_key(|(_, source_key, _)| source_key.identity().digest());
    let mut changed_sources = Vec::new();
    for (source, source_key, native_session_id) in sources {
        let base = base_sources.get(&source_key).cloned();
        if base
            .as_ref()
            .is_some_and(|base| !base.observation().source().exact_descriptor_eq(&source_key))
        {
            return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                native_session_id,
            ));
        }
        let proof = base
            .as_ref()
            .filter(|base| base.parser_revision() == CODEX_PARSER_REVISION)
            .and_then(|base| decode_append_proof(&source, &source_key, base).ok());
        if replay_unchanged_source_v0(
            &source,
            &source_key,
            base.as_ref(),
            proof.as_ref(),
            writer,
            revalidation,
            timings,
            counters,
        )? {
            continue;
        }
        changed_sources.push(super::cold::ChangedSourceV0 {
            source,
            source_key,
            native_session_id,
            base,
            proof,
        });
    }
    if changed_sources.is_empty() {
        return Ok(());
    }
    let changed_source_count = u64::try_from(changed_sources.len())
        .map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let worker_count = cold_scanner_worker_count(
        changed_source_count,
        indexer_threads,
        cold_options.scanner_workers,
    )?;
    let base_event_lookup = writer.base_event_identity_lookup();
    ingest_codex_cold_parallel_v0(
        changed_sources,
        base_event_lookup,
        ColdIngestionTargetV0 {
            writer,
            revalidation,
            timings,
            counters,
        },
        worker_count,
        cold_options,
    )
}

pub(crate) fn ingest_codex_sources_serial_v0(
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    base_sources: &HashMap<SourceKey, CertifiedSource>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
) -> CodexSourceBackedResultV0<()> {
    let mut repository_attributor = crate::repository_attribution::RepositoryAttributor::default();
    let base_event_lookup = writer.base_event_identity_lookup();
    for (source, source_key, native_session_id) in sources {
        let base = base_sources.get(&source_key).cloned();
        if base
            .as_ref()
            .is_some_and(|base| !base.observation().source().exact_descriptor_eq(&source_key))
        {
            return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                native_session_id,
            ));
        }
        let proof = base
            .as_ref()
            .filter(|base| base.parser_revision() == CODEX_PARSER_REVISION)
            .and_then(|base| decode_append_proof(&source, &source_key, base).ok());

        if replay_unchanged_source_v0(
            &source,
            &source_key,
            base.as_ref(),
            proof.as_ref(),
            writer,
            revalidation,
            timings,
            counters,
        )? {
            continue;
        }

        let scan_started = Instant::now();
        counters.scanner_workers = 1;
        let scanner_started = Instant::now();
        let append_base = match (base.as_ref(), proof.as_ref()) {
            (Some(base), Some(proof))
                if source.catalog_observation.len > proof.checkpoint.observation.len =>
            {
                match CodexNativeScanner::new_source_backed_v0(source.clone(), Some(proof)) {
                    Ok(scanner) => Some((base, scanner)),
                    Err(error) if invalid_append_proof(&error) => None,
                    Err(error) => return Err(error.into()),
                }
            }
            _ => None,
        };
        let (append_base, mut scanner) = match append_base {
            Some((base, scanner)) => {
                let writer_base = writer.begin_source_append(source_key.clone())?;
                if writer_base != base {
                    return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
                }
                counters.writer_mutated_sources = counters.writer_mutated_sources.saturating_add(1);
                (Some(base), scanner)
            }
            None => {
                writer.begin_source(source_key.clone())?;
                counters.writer_mutated_sources = counters.writer_mutated_sources.saturating_add(1);
                (
                    None,
                    CodexNativeScanner::new_source_backed_v0(source.clone(), None)?,
                )
            }
        };
        counters.scanner_sources_started = counters.scanner_sources_started.saturating_add(1);
        counters.peak_active_scanners = counters.peak_active_scanners.max(1);
        timings.scanner_worker_busy += scanner_started.elapsed();
        let session_id = codex_session_identity(&source_key, &native_session_id)?;
        let mut event_identity_state = match append_base {
            Some(_) => CodexEventIdentityStateV0::for_append(base_event_lookup.clone()),
            None => CodexEventIdentityStateV0::default(),
        };
        let mut staged_for_source = 0_u64;
        loop {
            let scanner_started = Instant::now();
            let page = scanner.next_page()?;
            timings.scanner_worker_busy += scanner_started.elapsed();
            let Some(page) = page else {
                break;
            };
            let CodexNativeOwnedPage::Core(page) = page;
            if !page.core_rows.is_empty() {
                return Err(CodexSourceBackedErrorV0::UnexpectedLegacyRow);
            }
            if page.source_backed_rows.is_empty() {
                continue;
            }
            let owner = page
                .owner
                .as_ref()
                .ok_or(CodexSourceBackedErrorV0::MissingPageOwner)?;
            validate_owner(owner, &native_session_id)?;
            for row in page.source_backed_rows {
                let conversion_started = Instant::now();
                let record = codex_core_record(
                    &source_key,
                    session_id,
                    owner,
                    row,
                    &mut event_identity_state,
                    &mut repository_attributor,
                )?;
                timings.scanner_worker_busy += conversion_started.elapsed();
                let add_started = Instant::now();
                let add_result = writer.add_core_record(record);
                timings.writer_add_document += add_started.elapsed();
                add_result?;
                staged_for_source = staged_for_source
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
        }
        let scanner_started = Instant::now();
        let scan = scanner.finish()?;
        counters.scanner_sources_completed = counters.scanner_sources_completed.saturating_add(1);
        timings.scanner_worker_busy += scanner_started.elapsed();
        timings.scan_and_stage += scan_started.elapsed();
        let scan_counters = scan.counters;
        counters.add_scan(scan_counters);
        counters.staged_documents = counters
            .staged_documents
            .checked_add(staged_for_source)
            .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;

        let certification_started = Instant::now();
        match (append_base, scan.disposition) {
            (None, CodexParseDisposition::FullGeneration) => {
                let current =
                    certify_scan(&source_key, &scan, None, staged_for_source, scan_counters)?;
                writer.certify_source(current)?;
                if base.is_some() {
                    counters.replaced_sources = counters.replaced_sources.saturating_add(1);
                } else {
                    counters.cold_sources = counters.cold_sources.saturating_add(1);
                }
            }
            (Some(base), CodexParseDisposition::AppendDelta) => {
                let current = certify_scan(
                    &source_key,
                    &scan,
                    Some(base),
                    staged_for_source,
                    scan_counters,
                )?;
                let base_frontier = base
                    .frontier()
                    .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
                let append = CertifiedSourceAppend::certify(
                    base,
                    current,
                    base_frontier.certified_prefix_bytes(),
                    *base_frontier.certified_prefix_digest(),
                )?;
                writer.certify_source_append(append)?;
                counters.appended_sources = counters.appended_sources.saturating_add(1);
            }
            _ => {
                return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                    native_session_id,
                ));
            }
        }
        timings.certification += certification_started.elapsed();
        revalidation.insert(
            source_key,
            CodexTerminalSourceEvidenceV0::new(
                source,
                scan.after_observation.clone(),
                scan.before_observation.len,
                scan.full_revision_sha256,
            ),
        );
    }
    counters.repository_full_git_certification_probes = counters
        .repository_full_git_certification_probes
        .saturating_add(
            u64::try_from(repository_attributor.full_certification_probe_count())
                .unwrap_or(u64::MAX),
        );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn replay_unchanged_source_v0(
    source: &CodexCatalogSource,
    source_key: &SourceKey,
    base: Option<&CertifiedSource>,
    proof: Option<&CodexAppendProof>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
) -> CodexSourceBackedResultV0<bool> {
    let (Some(base), Some(proof)) = (base, proof) else {
        return Ok(false);
    };
    // An unchanged strong file observation (identity + ctime-backed change
    // token + length + mtime) means the already-certified generation is still
    // the provider source. Final commit revalidation repeats the observation
    // before publishing, so exact replay does not need a scanner body pass.
    if proof.checkpoint.observation != source.catalog_observation {
        return Ok(false);
    }

    let certification_started = Instant::now();
    let writer_base = writer.begin_source_append(source_key.clone())?;
    if writer_base != base {
        return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
    }
    let base_frontier = base
        .frontier()
        .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
    let append = CertifiedSourceAppend::certify(
        base,
        base.clone(),
        base_frontier.certified_prefix_bytes(),
        *base_frontier.certified_prefix_digest(),
    )?;
    writer.certify_source_append(append)?;
    timings.certification += certification_started.elapsed();
    counters.replayed_sources = counters.replayed_sources.saturating_add(1);
    counters.writer_exact_replay_sources = counters.writer_exact_replay_sources.saturating_add(1);
    revalidation.insert(
        source_key.clone(),
        CodexTerminalSourceEvidenceV0 {
            source: source.clone(),
            observation: proof.checkpoint.observation.clone(),
            certified_len: proof.checkpoint.observation.len,
            full_revision_sha256: proof.checkpoint.full_revision_sha256,
        },
    );
    Ok(true)
}

fn invalid_append_proof(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidPayload(detail)
            if detail.starts_with("invalid Codex append proof:")
    )
}

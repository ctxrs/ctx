use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexJsonlFamilyPublicationV0 {
    Append,
    Replace,
}

#[derive(Debug)]
pub(super) struct CodexJsonlFamilyLeafOutcomeV0 {
    pub(super) certificate: CertifiedSource,
    pub(super) append: Option<CertifiedSourceAppend>,
    pub(super) terminal_prefix_bytes: u64,
    pub(super) terminal_prefix_sha256: [u8; 32],
    pub(super) counters: CodexSourceBackedCountersV0,
}

pub(super) struct CodexJsonlFamilyLeafContextV0<'a> {
    pub(super) base_event_lookup: &'a BaseEventIdentityLookup,
    pub(super) outcome_lineage: &'a CodexOutcomeLineageAuthorityV0,
    pub(super) repository_attributor: &'a mut crate::repository_attribution::RepositoryAttributor,
}

/// Runs exactly one Codex session leaf through the native scanner while the
/// shared JSONL family owns scheduling and writer publication.
///
/// This is intentionally the same NativePath path used by the former custom
/// route: exact replay validates only the native checkpoint observation,
/// certified growth scans only the suffix, and failed append evidence falls
/// back to one full native replacement scan.
pub(super) fn scan_codex_jsonl_family_leaf_v0(
    source: CodexCatalogSource,
    source_key: SourceKey,
    native_session_id: String,
    base: Option<&CertifiedSource>,
    collect_lineage_facts: bool,
    context: &mut CodexJsonlFamilyLeafContextV0<'_>,
    mut emit: impl FnMut(
        CodexJsonlFamilyPublicationV0,
        Vec<CoreRecord>,
    ) -> CodexSourceBackedResultV0<()>,
) -> CodexSourceBackedResultV0<CodexJsonlFamilyLeafOutcomeV0> {
    let probes_before = context
        .repository_attributor
        .full_certification_probe_count();
    let lineage_dependency_sha256 = context
        .outcome_lineage
        .dependency_digest(&native_session_id);
    if base.is_some_and(|base| !base.observation().source().exact_descriptor_eq(&source_key)) {
        return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
            native_session_id,
        ));
    }
    let proof = base
        .filter(|base| base.parser_revision() == CODEX_PARSER_REVISION)
        .and_then(|base| decode_append_proof(&source, &source_key, base).ok())
        .filter(|proof| proof.checkpoint.lineage_dependency_sha256 == lineage_dependency_sha256);
    if let (Some(base), Some(proof)) = (base, proof.as_ref()) {
        if proof.checkpoint.observation == source.catalog_observation {
            let frontier = base
                .frontier()
                .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
            let append = CertifiedSourceAppend::certify(
                base,
                base.clone(),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )?;
            return Ok(CodexJsonlFamilyLeafOutcomeV0 {
                certificate: base.clone(),
                append: Some(append),
                terminal_prefix_bytes: proof.checkpoint.observation.len,
                terminal_prefix_sha256: proof.checkpoint.full_revision_sha256,
                counters: CodexSourceBackedCountersV0 {
                    catalog_sources: 1,
                    catalog_source_bytes: proof.checkpoint.observation.len,
                    replayed_sources: 1,
                    writer_exact_replay_sources: 1,
                    ..CodexSourceBackedCountersV0::default()
                },
            });
        }
    }

    let append_scanner = match (base, proof.as_ref()) {
        (Some(base), Some(proof))
            if source.catalog_observation.len > proof.checkpoint.observation.len =>
        {
            let scanner = if collect_lineage_facts {
                CodexNativeScanner::new_source_backed_with_lineage_v0(
                    source.clone(),
                    Some(proof),
                    context.outcome_lineage.new_fact_set(&native_session_id)?,
                )
            } else {
                CodexNativeScanner::new_source_backed_without_lineage_v0(
                    source.clone(),
                    Some(proof),
                )
            };
            match scanner {
                Ok(scanner) => Some((base, scanner)),
                Err(error) if invalid_append_proof(&error) => None,
                Err(error) => return Err(map_lineage_capture_error(error)),
            }
        }
        _ => None,
    };
    let (append_base, publication, mut scanner) = match append_scanner {
        Some((base, scanner)) => (Some(base), CodexJsonlFamilyPublicationV0::Append, scanner),
        None => (
            None,
            CodexJsonlFamilyPublicationV0::Replace,
            if collect_lineage_facts {
                CodexNativeScanner::new_source_backed_with_lineage_v0(
                    source.clone(),
                    None,
                    context.outcome_lineage.new_fact_set(&native_session_id)?,
                )
            } else {
                CodexNativeScanner::new_source_backed_without_lineage_v0(source.clone(), None)
            }
            .map_err(map_lineage_capture_error)?,
        ),
    };
    let session_id = codex_session_identity(&source_key, &native_session_id)?;
    let mut event_identity_state = match append_base {
        Some(_) => CodexEventIdentityStateV0::for_append(context.base_event_lookup.clone()),
        None => CodexEventIdentityStateV0::default(),
    };
    let mut staged_documents = 0_u64;
    while let Some(page) = scanner.next_page().map_err(map_lineage_capture_error)? {
        let CodexNativeOwnedPage::Core(page) = page;
        if !page.core_rows.is_empty() {
            return Err(CodexSourceBackedErrorV0::UnexpectedLegacyRow);
        }
        let mut records = Vec::with_capacity(page.source_backed_rows.len());
        if !page.source_backed_rows.is_empty() {
            let owner = page
                .owner
                .as_ref()
                .ok_or(CodexSourceBackedErrorV0::MissingPageOwner)?;
            validate_owner(owner, &native_session_id)?;
            for row in page.source_backed_rows {
                records.push(codex_core_record(
                    &source_key,
                    session_id,
                    owner,
                    row,
                    &mut event_identity_state,
                    context.repository_attributor,
                    context.outcome_lineage,
                )?);
                staged_documents = staged_documents
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
        }
        scanner.release_transient_record_buffer();
        if !records.is_empty() {
            emit(publication, records)?;
        }
    }
    let mut scan = scanner.finish().map_err(map_lineage_capture_error)?;
    match scan.lineage_facts.take() {
        Some(lineage_facts) if collect_lineage_facts => context
            .outcome_lineage
            .register(&native_session_id, lineage_facts)?,
        None if !collect_lineage_facts => {}
        Some(_) | None => return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable),
    }
    let scan_counters = scan.counters;
    let certificate = match (append_base, scan.disposition) {
        (None, CodexParseDisposition::FullGeneration) => certify_scan(
            &source_key,
            &scan,
            None,
            staged_documents,
            scan_counters,
            lineage_dependency_sha256,
        )?,
        (Some(base), CodexParseDisposition::AppendDelta) => certify_scan(
            &source_key,
            &scan,
            Some(base),
            staged_documents,
            scan_counters,
            lineage_dependency_sha256,
        )?,
        _ => {
            return Err(CodexSourceBackedErrorV0::UnsupportedLifecycle(
                native_session_id,
            ));
        }
    };
    let append = match append_base {
        Some(base) => {
            let frontier = base
                .frontier()
                .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
            Some(CertifiedSourceAppend::certify(
                base,
                certificate.clone(),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )?)
        }
        None => None,
    };
    let terminal_prefix_bytes = scan.before_observation.len;
    let terminal_prefix_sha256 = scan.full_revision_sha256;
    let mut counters = CodexSourceBackedCountersV0 {
        catalog_sources: 1,
        catalog_source_bytes: source.catalog_observation.len,
        cold_sources: u64::from(base.is_none()),
        appended_sources: u64::from(append.is_some()),
        replaced_sources: u64::from(base.is_some() && append.is_none()),
        writer_mutated_sources: 1,
        scanner_workers: 1,
        scanner_sources_started: 1,
        scanner_sources_completed: 1,
        peak_active_scanners: 1,
        repository_full_git_certification_probes: u64::try_from(
            context
                .repository_attributor
                .full_certification_probe_count()
                .saturating_sub(probes_before),
        )
        .unwrap_or(u64::MAX),
        staged_documents,
        ..CodexSourceBackedCountersV0::default()
    };
    counters.add_scan(scan_counters);
    Ok(CodexJsonlFamilyLeafOutcomeV0 {
        certificate,
        append,
        terminal_prefix_bytes,
        terminal_prefix_sha256,
        counters,
    })
}

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
    let mut writer = GenerationWriter::open(global_index_root, writer_options.clone())?
        .into_writer()
        .expect("generation writer opening is infallible after validation");
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

#[cfg(test)]
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
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources)?;
    if !normalized.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(
                "Codex lineage source graph contains rejected components".to_owned(),
            ),
        ));
    }
    sources = normalized.sources;
    let outcome_lineage = Arc::new(normalized.authority);
    counters.catalog_sources =
        u64::try_from(sources.len()).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    counters.catalog_source_bytes = sources.iter().fold(0_u64, |total, (source, _, _)| {
        total.saturating_add(source.catalog_observation.len)
    });
    sources.sort_by_key(|(_, source_key, _)| source_key.identity().digest());
    let mut changed_sources = Vec::new();
    let mut replay_sources = Vec::new();
    for (source, source_key, native_session_id) in sources {
        let lineage_dependency_sha256 = outcome_lineage.dependency_digest(&native_session_id);
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
            .and_then(|base| decode_append_proof(&source, &source_key, base).ok())
            .filter(|proof| {
                proof.checkpoint.lineage_dependency_sha256 == lineage_dependency_sha256
            });
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
            if let Some(proof) = proof {
                replay_sources.push((source, proof, native_session_id));
            }
            continue;
        }
        changed_sources.push(super::cold::ChangedSourceV0 {
            source,
            source_key,
            native_session_id,
            base,
            proof,
            lineage_dependency_sha256,
        });
    }
    if changed_sources.is_empty() {
        return Ok(());
    }
    prepare_replayed_lineage_v0(&replay_sources, &outcome_lineage)?;
    changed_sources.sort_by_key(|source| outcome_lineage.depth(&source.native_session_id));
    let changed_ids = changed_sources
        .iter()
        .map(|source| source.native_session_id.as_str())
        .collect::<HashSet<_>>();
    let has_changed_dependency = changed_sources.iter().any(|source| {
        source
            .source
            .catalog_parent_native_session_id
            .as_deref()
            .is_some_and(|parent| changed_ids.contains(parent))
    });
    let changed_source_count = u64::try_from(changed_sources.len())
        .map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let worker_count = if has_changed_dependency {
        1
    } else {
        cold_scanner_worker_count(
            changed_source_count,
            indexer_threads,
            cold_options.scanner_workers,
        )?
    };
    let base_event_lookup = writer.base_event_identity_lookup();
    ingest_codex_cold_parallel_v0(
        changed_sources,
        base_event_lookup,
        outcome_lineage,
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

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn ingest_codex_sources_serial_v0(
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    base_sources: &HashMap<SourceKey, CertifiedSource>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
) -> CodexSourceBackedResultV0<()> {
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources)?;
    if !normalized.rejections.is_empty() {
        return Err(CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(
                "Codex lineage source graph contains rejected components".to_owned(),
            ),
        ));
    }
    let sources = normalized.sources;
    let outcome_lineage = normalized.authority;
    let mut repository_attributor = crate::repository_attribution::RepositoryAttributor::default();
    let base_event_lookup = writer.base_event_identity_lookup();
    for (source, source_key, native_session_id) in sources {
        let lineage_dependency_sha256 = outcome_lineage.dependency_digest(&native_session_id);
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
            .and_then(|base| decode_append_proof(&source, &source_key, base).ok())
            .filter(|proof| {
                proof.checkpoint.lineage_dependency_sha256 == lineage_dependency_sha256
            });

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
                let facts = outcome_lineage.new_fact_set(&native_session_id)?;
                match CodexNativeScanner::new_source_backed_with_lineage_v0(
                    source.clone(),
                    Some(proof),
                    facts,
                ) {
                    Ok(scanner) => Some((base, scanner)),
                    Err(error) if invalid_append_proof(&error) => None,
                    Err(error) => return Err(map_lineage_capture_error(error)),
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
                    CodexNativeScanner::new_source_backed_with_lineage_v0(
                        source.clone(),
                        None,
                        outcome_lineage.new_fact_set(&native_session_id)?,
                    )
                    .map_err(map_lineage_capture_error)?,
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
            let page = scanner.next_page().map_err(map_lineage_capture_error)?;
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
                    &outcome_lineage,
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
        let mut scan = scanner.finish().map_err(map_lineage_capture_error)?;
        let lineage_facts = scan
            .lineage_facts
            .take()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        outcome_lineage.register(&native_session_id, lineage_facts)?;
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
                let current = certify_scan(
                    &source_key,
                    &scan,
                    None,
                    staged_for_source,
                    scan_counters,
                    lineage_dependency_sha256,
                )?;
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
                    lineage_dependency_sha256,
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

pub(super) fn prepare_replayed_lineage_v0(
    replay_sources: &[(CodexCatalogSource, CodexAppendProof, String)],
    outcome_lineage: &CodexOutcomeLineageAuthorityV0,
) -> CodexSourceBackedResultV0<()> {
    for (source, proof, native_session_id) in replay_sources {
        let mut scanner = CodexNativeScanner::new_source_backed_with_lineage_v0(
            source.clone(),
            Some(proof),
            outcome_lineage.new_fact_set(native_session_id)?,
        )
        .map_err(map_lineage_capture_error)?;
        while scanner
            .next_page()
            .map_err(map_lineage_capture_error)?
            .is_some()
        {}
        let mut scan = scanner.finish().map_err(map_lineage_capture_error)?;
        let facts = scan
            .lineage_facts
            .take()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        outcome_lineage.register(native_session_id, facts)?;
    }
    Ok(())
}

/// Scans each selected ancestor exactly once, component by component, and
/// moves its sealed facts into the generation's capability-owned authenticated
/// spill before starting the next component. Route-local partition leases load
/// at most four components from that spill and never reread provider bodies.
/// Terminal leaves receive an empty completed state without a body pass.
pub(super) fn prepare_generation_lineage_v0(
    sources: &[(CodexCatalogSource, SourceKey, String)],
    outcome_lineage: &mut CodexOutcomeLineageAuthorityV0,
) -> CodexSourceBackedResultV0<u64> {
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        outcome_lineage
            .component_partition(&left.2)
            .cmp(&outcome_lineage.component_partition(&right.2))
            .then_with(|| {
                outcome_lineage
                    .depth(&left.2)
                    .cmp(&outcome_lineage.depth(&right.2))
            })
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.0.source_path.cmp(&right.0.source_path))
    });
    let mut active_component = None;
    let mut source_scans = 0_u64;
    for (source, _, native_session_id) in ordered {
        let component = outcome_lineage
            .component_partition(native_session_id)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        if active_component.is_some_and(|active| active != component) {
            outcome_lineage.spill_generation_component(
                active_component.ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?,
            )?;
        }
        active_component = Some(component);
        let facts = outcome_lineage.new_fact_set(native_session_id)?;
        if !outcome_lineage.needs_descendant_facts(native_session_id)? {
            outcome_lineage.register(native_session_id, facts)?;
            continue;
        }
        let mut scanner =
            CodexNativeScanner::new_source_backed_with_lineage_v0(source.clone(), None, facts)
                .map_err(map_lineage_capture_error)?;
        while scanner
            .next_page()
            .map_err(map_lineage_capture_error)?
            .is_some()
        {
            scanner.release_transient_record_buffer();
        }
        let mut scan = scanner.finish().map_err(map_lineage_capture_error)?;
        let facts = scan
            .lineage_facts
            .take()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        outcome_lineage.register(native_session_id, facts)?;
        source_scans = source_scans
            .checked_add(1)
            .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
    }
    if let Some(component) = active_component {
        outcome_lineage.spill_generation_component(component)?;
    }
    Ok(source_scans)
}

#[cfg(test)]
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

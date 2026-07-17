pub(crate) fn import_append_capable_catalog_work(
    store: &mut Store,
    source: &SourceInfo,
    work: &CatalogImportWork,
    inventory_generation: u64,
    record: &HistoryRecord,
) -> Result<AppendImportOutcome> {
    import_append_capable_work(
        store,
        AppendInventoryUnit::Catalog {
            source,
            work,
            inventory_generation,
        },
        record,
    )
}

fn import_append_capable_source_file_work(
    store: &mut Store,
    source: &SourceInfo,
    work: &SourceImportFileWork,
    inventory_generation: u64,
    record: &HistoryRecord,
) -> Result<AppendImportOutcome> {
    import_append_capable_work(
        store,
        AppendInventoryUnit::SourceFile {
            source,
            work,
            inventory_generation,
        },
        record,
    )
}

fn import_manifested_append_source_file_work(
    store: &mut Store,
    source: &SourceInfo,
    pending_source: &SourceInfo,
    work: &SourceImportFileWork,
    inventory_generation: u64,
) -> Result<AppendImportOutcome> {
    let record = import_record_for_source(pending_source);
    let record_existed = history_record_exists(store, record.id)
        .context("inspect manifested append history record")?;
    let result =
        import_append_capable_source_file_work(store, source, work, inventory_generation, &record)
            .context("run manifested append publication");
    let remove_orphan = match &result {
        Ok(AppendImportOutcome::Imported(summary)) => summary == &ProviderImportSummary::default(),
        Ok(AppendImportOutcome::Deferred { durable_progress }) => !durable_progress,
        Err(_) => !store.has_pending_provider_file_publications()?,
    };
    if remove_orphan && !record_existed {
        store
            .delete_orphan_record(record.id)
            .context("clean up manifested append history record")?;
    }
    result
}

fn import_append_capable_work(
    store: &mut Store,
    unit: AppendInventoryUnit<'_>,
    record: &HistoryRecord,
) -> Result<AppendImportOutcome> {
    let provider = unit.provider();
    let source_format = unit.source_format().to_owned();
    let material_source_format =
        provider_canonical_material_source_format(provider, &source_format).ok_or_else(|| {
            anyhow!(
                "missing canonical material format for {}:{source_format}",
                provider.as_str()
            )
        })?;
    if provider_file_mutation_contract(provider, &source_format)
        != ProviderFileMutationContract::AppendOnlyNewlineDelimited
    {
        return Err(anyhow!(
            "{}:{source_format} is not append-capable",
            provider.as_str()
        ));
    }

    let mut replacement = unit.reason().requires_replacement() || unit.has_active_publication();
    for _ in 0..2 {
        let admitted_checkpoint = if replacement {
            None
        } else {
            load_admitted_append_checkpoint(store, &unit)?
        };
        let admitted_offset = admitted_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.checkpoint().committed_offset);
        if admitted_checkpoint.is_none() {
            replacement = true;
        }
        let observed_offset = admitted_offset.unwrap_or(0);
        if unit.file_size_bytes() > observed_offset
            && !provider_jsonl_range_has_complete_line(
                std::path::Path::new(unit.source_path()),
                observed_offset,
                unit.file_size_bytes(),
            )?
        {
            return Ok(AppendImportOutcome::Deferred {
                durable_progress: false,
            });
        }
        let kind = if replacement {
            ProviderFilePublicationKind::Replacement
        } else {
            ProviderFilePublicationKind::Incremental
        };
        let observation = unit.observation(utc_now().timestamp_millis(), None);
        let scope = store
            .begin_provider_file_publication(
                provider,
                observation,
                material_source_format,
                kind,
                utc_now().timestamp_millis(),
            )
            .context("begin append-capable provider publication")?;
        let mut scope = Some(scope);
        let attempt = (|| -> Result<AppendPublicationAttempt> {
            let publication = scope
                .as_ref()
                .expect("append publication scope must remain owned until completion");
            let effective_replacement =
                publication.kind() == ProviderFilePublicationKind::Replacement;
            if effective_replacement {
                match store.provider_file_publication_phase(publication)? {
                    ProviderFilePublicationPhase::Preparing => {
                        let preparation = store
                            .prepare_provider_file_publication_slice(
                                publication,
                                PROVIDER_PUBLICATION_SLICE_ROWS,
                            )
                            .context("prepare append-capable replacement publication")?;
                        if !preparation.complete {
                            store.abandon_provider_file_publication(
                                scope.take().expect("append publication scope must exist"),
                            )?;
                            return Ok(AppendPublicationAttempt::Deferred {
                                durable_progress: preparation.rows_processed > 0,
                            });
                        }
                    }
                    ProviderFilePublicationPhase::Reconciling => {
                        store
                            .reconcile_provider_file_publication_slice(
                                publication,
                                PROVIDER_PUBLICATION_SLICE_ROWS,
                            )
                            .context("reconcile append-capable replacement publication")?;
                        store.abandon_provider_file_publication(
                            scope.take().expect("append publication scope must exist"),
                        )?;
                        return Ok(AppendPublicationAttempt::Deferred {
                            durable_progress: true,
                        });
                    }
                    ProviderFilePublicationPhase::ReadyToFinalize => {
                        let completion = store
                            .load_provider_file_publication_completion(publication)?
                            .ok_or_else(|| {
                                anyhow::Error::new(CaptureError::SystemInvariant(
                                    "ready provider publication has no staged completion",
                                ))
                            })?;
                        let staged = decode_staged_append_completion(completion)?;
                        if !staged_append_completion_matches_current_file(&unit, &staged)? {
                            invalidate_active_append_publication_observation(store, &unit)?;
                            queue_current_append_observation(store, &unit, true)?;
                            store.abandon_provider_file_publication(
                                scope.take().expect("append publication scope must exist"),
                            )?;
                            return Ok(AppendPublicationAttempt::Deferred {
                                durable_progress: true,
                            });
                        }
                        let (mut summary, checkpoint, indexed_at_ms) = staged.into_restored();
                        let status = provider_summary_import_status(&summary);
                        let outcome_error =
                            (summary.failed > 0).then(|| source_import_file_failure(&summary));
                        let event_count = Some(
                            summary
                                .imported_events
                                .saturating_add(summary.skipped_events)
                                as u64,
                        );
                        let outcome = ProviderFileImportOutcome {
                            provider,
                            observation: unit.observation(indexed_at_ms, event_count),
                            status,
                            error: outcome_error.as_deref(),
                        };
                        let checkpoint = checkpoint
                            .map(|checkpoint| checkpoint.into_store_checkpoint(&unit))
                            .transpose()?;
                        let commit = ProviderFilePublicationCommit::Replacement(
                            (summary.failed == 0)
                                .then_some(checkpoint.as_ref())
                                .flatten(),
                        );
                        let finalized = store.finalize_provider_file_publication(
                            scope.take().expect("append publication scope must exist"),
                            outcome,
                            commit,
                        )?;
                        if let Some(warning) = finalized.maintenance_warning {
                            push_publication_maintenance_warning(&mut summary, warning);
                        }
                        queue_current_append_observation(store, &unit, false)?;
                        return Ok(AppendPublicationAttempt::Imported(summary));
                    }
                    ProviderFilePublicationPhase::Importing => {}
                }
            }

            let mode = if effective_replacement {
                ProviderAppendFileImportMode::AppendCapableReplacement
            } else {
                ProviderAppendFileImportMode::Append(admitted_checkpoint.ok_or_else(|| {
                    anyhow::Error::new(CaptureError::SystemInvariant(
                        "incremental publication has no admitted checkpoint",
                    ))
                })?)
            };
            let decision = store
                .with_provider_file_publication_writes_mut(publication, |store| {
                    if !store
                        .provider_file_publication_history_record_exists(publication, record.id)?
                    {
                        store.upsert_record(record)?;
                    }
                    import_append_capable_provider_file(
                        provider,
                        store,
                        ProviderAppendFileImportOptions {
                            machine_id: CodexSessionImportOptions::default().machine_id,
                            inventory_source_format: source_format.clone(),
                            material_source_format: material_source_format.to_owned(),
                            source_path: PathBuf::from(unit.source_path()),
                            source_root: PathBuf::from(unit.material_source_root()),
                            imported_at: utc_now(),
                            history_record_id: Some(record.id),
                            observed_size: unit.file_size_bytes(),
                            mode,
                        },
                    )
                })
                .map_err(anyhow::Error::new)
                .context("write append-capable provider publication")?;
            #[cfg(test)]
            if take_append_source_failure_after_mutation() {
                return Err(anyhow::Error::new(CaptureError::InvalidPayload(
                    "injected append source failure after publication mutation".to_owned(),
                )));
            }

            if let ProviderAppendFileImportDecision::ReplacementRequired(_) = decision {
                let abort = store.abort_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                if let ControlFlow::Break(warning) = abort {
                    return Err(publication_recovery_required_error(
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "append importer requested replacement after mutating its publication",
                        )),
                        warning,
                    ));
                }
                if effective_replacement {
                    return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                        "append-capable replacement importer requested replacement",
                    )));
                }
                return Ok(AppendPublicationAttempt::RetryReplacement);
            }

            let deferred_without_boundary_progress = !effective_replacement
                && admitted_offset.is_some_and(|prior_offset| {
                    unit.file_size_bytes() > prior_offset
                        && match &decision {
                            ProviderAppendFileImportDecision::Imported(result) => {
                                result.summary == ProviderImportSummary::default()
                                    && result.checkpoint.committed_offset == prior_offset
                            }
                            ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
                                result.summary == ProviderImportSummary::default()
                            }
                            ProviderAppendFileImportDecision::DeferredPartial
                            | ProviderAppendFileImportDecision::ReplacementRequired(_) => false,
                        }
                });
            if deferred_without_boundary_progress {
                let abort = store.abort_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                if let ControlFlow::Break(warning) = abort {
                    return Err(publication_recovery_required_error(
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "append tail deferred after mutating its publication",
                        )),
                        warning,
                    ));
                }
                return Ok(AppendPublicationAttempt::Deferred {
                    durable_progress: false,
                });
            }

            let (mut summary, checkpoint, source_prefix_sha256, retain_checkpoint) = match decision
            {
                ProviderAppendFileImportDecision::Imported(result) => {
                    (result.summary, Some(result.checkpoint), None, false)
                }
                ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => (
                    result.summary,
                    None,
                    result.source_prefix_sha256,
                    !effective_replacement,
                ),
                ProviderAppendFileImportDecision::DeferredPartial => {
                    let abort = store.abort_provider_file_publication(
                        scope.take().expect("append publication scope must exist"),
                    )?;
                    if let ControlFlow::Break(warning) = abort {
                        return Err(publication_recovery_required_error(
                            anyhow::Error::new(CaptureError::SystemInvariant(
                                "partial append deferred after mutating its publication",
                            )),
                            warning,
                        ));
                    }
                    return Ok(AppendPublicationAttempt::Deferred {
                        durable_progress: false,
                    });
                }
                ProviderAppendFileImportDecision::ReplacementRequired(_) => unreachable!(),
            };
            let mut status = provider_summary_import_status(&summary);
            if status == CatalogIndexedStatus::Rejected
                && effective_replacement
                && publication.tracks_prior_material()
            {
                let abort = store.abort_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                match abort {
                    ControlFlow::Continue(warning) => {
                        summary.mark_retained_existing_content();
                        if let Some(warning) = warning {
                            push_publication_maintenance_warning(&mut summary, warning);
                        }
                        return Ok(AppendPublicationAttempt::Imported(summary));
                    }
                    ControlFlow::Break(warning) => {
                        return Err(publication_recovery_required_error(
                            provider_import_summary_failure_for_unit(
                                provider,
                                unit.source_path(),
                                &summary,
                            ),
                            warning,
                        ));
                    }
                }
            }
            if status == CatalogIndexedStatus::Rejected && !effective_replacement {
                summary.mark_retained_existing_content();
                status = provider_summary_import_status(&summary);
            }
            if status == CatalogIndexedStatus::Rejected {
                for failure in &mut summary.failures {
                    failure.error = format!("{}: {}", unit.source_path(), failure.error);
                }
                let deleted = store
                    .discard_provider_file_publication_orphan_record(publication, record.id)?;
                if deleted == Some(false)
                    && store
                        .provider_file_publication_history_record_exists(publication, record.id)?
                {
                    return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                        "rejected replacement left material attached to its history record",
                    )));
                }
                store.discard_provider_file_publication_orphan_capture_sources(publication)?;
                let completion = encode_staged_append_completion(
                    summary,
                    None,
                    None,
                    utc_now().timestamp_millis(),
                )?;
                store.stage_provider_file_publication_completion(publication, &completion)?;
                store.abandon_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                return Ok(AppendPublicationAttempt::Deferred {
                    durable_progress: true,
                });
            }
            let event_count = Some(
                summary
                    .imported_events
                    .saturating_add(summary.skipped_events) as u64,
            );
            let indexed_at_ms = utc_now().timestamp_millis();
            let outcome_error = (summary.failed > 0).then(|| source_import_file_failure(&summary));
            let outcome = ProviderFileImportOutcome {
                provider,
                observation: unit.observation(indexed_at_ms, event_count),
                status,
                error: outcome_error.as_deref(),
            };
            let stored_checkpoint = checkpoint
                .as_ref()
                .map(|checkpoint| store_checkpoint_from_capture(&unit, checkpoint, indexed_at_ms))
                .transpose()?;
            if effective_replacement {
                let completion = encode_staged_append_completion(
                    summary,
                    stored_checkpoint,
                    source_prefix_sha256,
                    indexed_at_ms,
                )?;
                store.stage_provider_file_publication_completion(publication, &completion)?;
                store.abandon_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                return Ok(AppendPublicationAttempt::Deferred {
                    durable_progress: true,
                });
            }
            let commit = if effective_replacement {
                ProviderFilePublicationCommit::Replacement(stored_checkpoint.as_ref())
            } else if retain_checkpoint {
                ProviderFilePublicationCommit::RetainCheckpoint
            } else {
                ProviderFilePublicationCommit::Append(stored_checkpoint.as_ref().ok_or_else(
                    || {
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "append import completed without a checkpoint decision",
                        ))
                    },
                )?)
            };
            let finalized = store.finalize_provider_file_publication(
                scope.take().expect("append publication scope must exist"),
                outcome,
                commit,
            )?;
            if let Some(warning) = finalized.maintenance_warning {
                push_publication_maintenance_warning(&mut summary, warning);
            }
            queue_current_append_observation(store, &unit, false)?;
            Ok(AppendPublicationAttempt::Imported(summary))
        })();

        match attempt {
            Ok(AppendPublicationAttempt::Imported(summary)) => {
                return Ok(AppendImportOutcome::Imported(summary));
            }
            Ok(AppendPublicationAttempt::Deferred { durable_progress }) => {
                return Ok(AppendImportOutcome::Deferred { durable_progress });
            }
            Ok(AppendPublicationAttempt::RetryReplacement) => {
                replacement = true;
            }
            Err(error) => {
                if let Some(scope) = scope.take() {
                    match store.abort_provider_file_publication(scope) {
                        Ok(ControlFlow::Continue(_)) => {}
                        Ok(ControlFlow::Break(warning)) => {
                            return Err(publication_recovery_required_error(error, warning));
                        }
                        Err(abort_error) => {
                            return Err(error.context(format!(
                                "release failed append publication: {abort_error}"
                            )));
                        }
                    }
                }
                return Err(error);
            }
        }
    }
    Err(anyhow::Error::new(CaptureError::SystemInvariant(
        "append import replacement retry did not converge",
    )))
}

fn staged_append_completion_matches_current_file(
    unit: &AppendInventoryUnit<'_>,
    staged: &StagedAppendPublicationCompletion,
) -> Result<bool> {
    let metadata = fs::symlink_metadata(unit.source_path())
        .with_context(|| format!("stat active append source {}", unit.source_path()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Ok(false);
    }
    if metadata.len() < unit.file_size_bytes() {
        return Ok(false);
    }
    let Some(checkpoint) = staged.checkpoint.as_ref() else {
        if !staged.has_accepted_content {
            return Ok(true);
        }
        let Some(expected_sha256) = staged.source_prefix_sha256.as_deref() else {
            return Ok(false);
        };
        return Ok(sha256_file_prefix_hex(
            std::path::Path::new(unit.source_path()),
            unit.file_size_bytes(),
        )? == expected_sha256);
    };
    if checkpoint.import_revision != unit.import_revision()
        || checkpoint.committed_byte_offset != unit.file_size_bytes()
    {
        return Ok(false);
    }
    let checkpoint = checkpoint.to_capture_checkpoint()?;
    provider_jsonl_checkpoint_matches_file(unit.source_path(), &checkpoint).map_err(Into::into)
}

fn invalidate_active_append_publication_observation(
    store: &Store,
    unit: &AppendInventoryUnit<'_>,
) -> Result<bool> {
    let Some(owner) = store.effective_provider_file_publication_inventory_owner()? else {
        return Ok(false);
    };
    if owner.provider != unit.provider()
        || owner.source_format != unit.source_format()
        || owner.source_root != unit.source_root()
        || owner.source_path != unit.source_path()
        || owner.file_size_bytes != unit.file_size_bytes()
        || owner.import_revision != unit.import_revision()
    {
        return Ok(false);
    }
    store
        .invalidate_effective_provider_file_publication_observation(
            &owner,
            utc_now().timestamp_millis(),
        )
        .map_err(Into::into)
}

fn queue_current_append_observation(
    store: &Store,
    unit: &AppendInventoryUnit<'_>,
    include_source_roots: bool,
) -> Result<()> {
    match unit {
        AppendInventoryUnit::Catalog {
            source,
            inventory_generation,
            work,
        } => {
            let summary = ctx_history_capture::catalog_codex_session_paths_page(
                vec![PathBuf::from(&work.session.source_path)],
                &source.path,
                store,
                *inventory_generation,
                ctx_history_capture::CodexSessionCatalogOptions {
                    source_root: Some(source.path.clone()),
                    observation_generation: Some(*inventory_generation),
                    ..ctx_history_capture::CodexSessionCatalogOptions::default()
                },
            )
            .with_context(|| {
                format!(
                    "reobserve Codex session after publishing {}",
                    work.session.source_path
                )
            })?;
            if summary.failed_sessions > 0 {
                return Err(
                    anyhow::Error::new(CaptureError::InventorySuperseded).context(format!(
                        "Codex session changed during publication: {}",
                        work.session.source_path
                    )),
                );
            }
        }
        AppendInventoryUnit::SourceFile { source, work, .. }
            if source_uses_import_file_manifest(source) =>
        {
            if let Some(current) =
                observe_selected_source_import_file(source, &work.file.source_path)?
            {
                store.upsert_source_import_files(
                    unit.inventory_generation(),
                    std::slice::from_ref(&current),
                )?;
            }
        }
        AppendInventoryUnit::SourceFile { source, .. } if include_source_roots => {
            ctx_history_capture::pace_current_filesystem_operation(
                source.path.as_os_str().len() as u64
            );
            let metadata = fs::symlink_metadata(&source.path)
                .with_context(|| format!("stat import source {}", source.path.display()))?;
            if metadata.file_type().is_dir() {
                return Err(anyhow::Error::new(CaptureError::InventorySuperseded).context(
                    "directory source changed during publication and requires bounded inventory",
                ));
            }
            let (_, current) = observe_source_root(source)?;
            persist_new_source_import_observation(store, source, std::slice::from_ref(&current))?;
        }
        AppendInventoryUnit::SourceFile { .. } => {}
    }
    Ok(())
}

fn load_admitted_append_checkpoint(
    store: &Store,
    unit: &AppendInventoryUnit<'_>,
) -> Result<Option<ProviderAdmittedJsonlAppendCheckpoint>> {
    let Some(checkpoint) = store.provider_file_checkpoint(ProviderFileCheckpointKey {
        provider: unit.provider(),
        source_format: unit.source_format(),
        source_root: unit.source_root(),
        source_path: unit.source_path(),
    })?
    else {
        return Ok(None);
    };
    if checkpoint.import_revision != unit.import_revision() {
        return Ok(None);
    }
    let Some(stable_identity) =
        ProviderFileStableIdentity::from_storage_key(&checkpoint.stable_file_identity)
    else {
        return Ok(None);
    };
    let resume_state = match checkpoint.resume_state {
        Some(bytes) => {
            let Ok(json) = std::str::from_utf8(&bytes) else {
                return Ok(None);
            };
            let Ok(state) = ProviderJsonlResumeState::decode_persisted_json(json) else {
                return Ok(None);
            };
            Some(state)
        }
        None => None,
    };
    Ok(Some(
        ProviderAdmittedJsonlAppendCheckpoint::from_persisted_admitted_replacement(
            ProviderJsonlAppendCheckpoint {
                version: checkpoint.checkpoint_version,
                stable_identity,
                committed_offset: checkpoint.committed_byte_offset,
                complete_line_count: checkpoint.committed_complete_line_count,
                head_sha256: checkpoint.head_sha256,
                boundary_sha256: checkpoint.boundary_sha256,
                resume_state,
            },
        ),
    ))
}

fn store_checkpoint_from_capture(
    unit: &AppendInventoryUnit<'_>,
    checkpoint: &ProviderJsonlAppendCheckpoint,
    updated_at_ms: i64,
) -> Result<ProviderFileCheckpoint> {
    let resume_state = checkpoint
        .resume_state
        .as_ref()
        .map(ProviderJsonlResumeState::encode_persisted_json)
        .transpose()?
        .map(String::into_bytes);
    Ok(ProviderFileCheckpoint {
        provider: unit.provider(),
        source_format: unit.source_format().to_owned(),
        source_root: unit.source_root().to_owned(),
        source_path: unit.source_path().to_owned(),
        import_revision: unit.import_revision(),
        checkpoint_version: checkpoint.version,
        stable_file_identity: checkpoint.stable_identity.to_storage_key(),
        committed_byte_offset: checkpoint.committed_offset,
        committed_complete_line_count: checkpoint.complete_line_count,
        head_sha256: checkpoint.head_sha256.clone(),
        boundary_sha256: checkpoint.boundary_sha256.clone(),
        resume_state,
        updated_at_ms,
    })
}

fn encode_staged_append_completion(
    summary: ProviderImportSummary,
    checkpoint: Option<ProviderFileCheckpoint>,
    source_prefix_sha256: Option<String>,
    indexed_at_ms: i64,
) -> Result<ProviderFilePublicationCompletion> {
    let has_accepted_content = summary.has_accepted_content();
    if has_accepted_content && checkpoint.is_none() && source_prefix_sha256.is_none() {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "accepted staged append completion has no content certificate",
        )));
    }
    let staged = StagedAppendPublicationCompletion {
        summary,
        has_accepted_content,
        checkpoint: checkpoint.map(StagedProviderFileCheckpoint::from_store_checkpoint),
        source_prefix_sha256,
        indexed_at_ms,
    };
    Ok(ProviderFilePublicationCompletion {
        version: STAGED_APPEND_PUBLICATION_VERSION,
        payload: serde_json::to_value(staged).context("encode staged append completion")?,
    })
}

fn decode_staged_append_completion(
    completion: ProviderFilePublicationCompletion,
) -> Result<StagedAppendPublicationCompletion> {
    if completion.version != STAGED_APPEND_PUBLICATION_VERSION {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "unsupported staged append publication version",
        )));
    }
    serde_json::from_value(completion.payload).context("decode staged append completion")
}

impl StagedProviderFileCheckpoint {
    fn from_store_checkpoint(checkpoint: ProviderFileCheckpoint) -> Self {
        Self {
            import_revision: checkpoint.import_revision,
            checkpoint_version: checkpoint.checkpoint_version,
            stable_file_identity: checkpoint.stable_file_identity,
            committed_byte_offset: checkpoint.committed_byte_offset,
            committed_complete_line_count: checkpoint.committed_complete_line_count,
            head_sha256: checkpoint.head_sha256,
            boundary_sha256: checkpoint.boundary_sha256,
            resume_state_base64: checkpoint.resume_state.map(|bytes| BASE64.encode(bytes)),
            updated_at_ms: checkpoint.updated_at_ms,
        }
    }

    fn into_store_checkpoint(
        self,
        unit: &AppendInventoryUnit<'_>,
    ) -> Result<ProviderFileCheckpoint> {
        let resume_state = self
            .resume_state_base64
            .map(|value| {
                BASE64
                    .decode(value)
                    .context("decode staged append resume state")
            })
            .transpose()?;
        Ok(ProviderFileCheckpoint {
            provider: unit.provider(),
            source_format: unit.source_format().to_owned(),
            source_root: unit.source_root().to_owned(),
            source_path: unit.source_path().to_owned(),
            import_revision: self.import_revision,
            checkpoint_version: self.checkpoint_version,
            stable_file_identity: self.stable_file_identity,
            committed_byte_offset: self.committed_byte_offset,
            committed_complete_line_count: self.committed_complete_line_count,
            head_sha256: self.head_sha256,
            boundary_sha256: self.boundary_sha256,
            resume_state,
            updated_at_ms: self.updated_at_ms,
        })
    }

    fn to_capture_checkpoint(&self) -> Result<ProviderJsonlAppendCheckpoint> {
        let stable_identity = ProviderFileStableIdentity::from_storage_key(
            &self.stable_file_identity,
        )
        .ok_or_else(|| {
            anyhow::Error::new(CaptureError::SystemInvariant(
                "staged append checkpoint has invalid file identity",
            ))
        })?;
        let resume_state = self
            .resume_state_base64
            .as_ref()
            .map(|value| {
                let bytes = BASE64
                    .decode(value)
                    .context("decode staged append resume state")?;
                let json = std::str::from_utf8(&bytes)
                    .context("decode staged append resume state as UTF-8")?;
                ProviderJsonlResumeState::decode_persisted_json(json)
                    .context("decode staged append resume state payload")
            })
            .transpose()?;
        Ok(ProviderJsonlAppendCheckpoint {
            version: self.checkpoint_version,
            stable_identity,
            committed_offset: self.committed_byte_offset,
            complete_line_count: self.committed_complete_line_count,
            head_sha256: self.head_sha256.clone(),
            boundary_sha256: self.boundary_sha256.clone(),
            resume_state,
        })
    }
}

fn provider_import_summary_failure_for_unit(
    provider: CaptureProvider,
    source_path: &str,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    let detail = summary
        .failures
        .first()
        .map(|failure| format!("line {}: {}", failure.line, failure.error))
        .unwrap_or_else(|| "unknown provider import failure".to_owned());
    rejected_source_error(
        format!(
            "import {} source {} failed with {} failure(s); first failure: {detail}",
            provider.as_str(),
            source_path,
            summary.failed
        ),
        summary,
    )
}

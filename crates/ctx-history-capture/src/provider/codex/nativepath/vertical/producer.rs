use super::*;

pub(crate) fn prepare_codex_native_producer_task(
    store: &Store,
    source: CodexCatalogSource,
    options: CodexNativeStoreOptions,
) -> VerticalResult<CodexNativeProducerTask> {
    ensure_active_journal(store)?;
    let identity = source_projection_identity(&source)?;
    let committed = load_committed_source(store, &source, &options, &identity)?;
    Ok(CodexNativeProducerTask {
        source,
        options,
        identity,
        committed,
    })
}

pub(crate) fn finish_pending_codex_native_retirement(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodexCatalogSource,
    options: &CodexNativeStoreOptions,
) -> VerticalResult<bool> {
    let identity = source_projection_identity(source)?;
    let Some(mut committed) = load_committed_source(store, source, options, &identity)? else {
        return Ok(false);
    };
    let CodexNativeCursorPhase::Retiring { mut after } = committed.phase.clone() else {
        return Ok(false);
    };
    let certified_observation =
        committed
            .certified_observation
            .clone()
            .ok_or(CodexNativeVerticalError::CorruptCursor(
                "pending retirement has no certified source observation",
            ))?;
    let locator = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id.clone(),
        locator_identity: source_locator_identity(
            &identity.cursor_stream,
            &identity.proposed_source_namespace,
        ),
        cursor_stream: identity.cursor_stream.clone(),
        proposed_source_identity: identity.proposed_source_namespace.clone(),
        raw_source_path: Some(source.source_path.display().to_string()),
        source_revision: committed.source_revision.clone(),
        observed_at_ms: options.imported_at.timestamp_millis(),
    };
    let resolution = store.plan_provider_source_locator(&locator)?;
    let key = NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id.clone(),
        canonical_source_identity: resolution.canonical_source_identity,
        locator_identity: locator.locator_identity.clone(),
        cursor_stream: identity.cursor_stream.clone(),
        source_revision: committed.source_revision.clone(),
        generation_id: format!("codex-nativepath-generation-v1:{}", committed.generation),
    };

    loop {
        let store_after = after
            .as_ref()
            .map(CodexRetirementFrontierWire::to_store)
            .transpose()?;
        let accounting = NativePathGroupAccounting::new(1, 1, CODEX_RETIREMENT_PAGE_BYTES)?;
        let admission = store.admit_event_search_bulk_group(bulk_guard)?;
        let mut publication = store.begin_native_path_publication_group(admission, accounting)?;
        let preview = publication.preview_source_generation_retirement_page(
            &key,
            store_after.as_ref(),
            CODEX_RETIREMENT_PAGE_UNITS,
        )?;
        let next_after = preview
            .next_after
            .clone()
            .map(CodexRetirementFrontierWire::from_store);
        let next_phase = if preview.done {
            CodexNativeCursorPhase::Complete
        } else {
            CodexNativeCursorPhase::Retiring {
                after: next_after.clone(),
            }
        };
        let next = build_next_store_cursor(
            options,
            &identity,
            committed.generation,
            &committed.source_revision,
            committed
                .proof
                .as_ref()
                .map(|proof| &proof.checkpoint)
                .ok_or(CodexNativeVerticalError::CorruptCursor(
                    "pending retirement lost its exact provider checkpoint",
                ))?,
            &certified_observation,
            next_phase.clone(),
            committed.rejected_records,
            committed.retained_events,
            committed.skipped_events,
        )?;
        let transition = NativePathCursorTransition::new(
            Some(committed.expected_store_cursor.cursor.clone()),
            next,
        );
        let publication_id = generation_retirement_publication_id(
            &key,
            after.as_ref(),
            next_after.as_ref(),
            preview.done,
        );
        match publication.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let exact_route = publication.reconcile_provider_source_locator(&locator)?;
                if exact_route.canonical_source_identity != key.canonical_source_identity {
                    return Err(CodexNativeVerticalError::CorruptCursor(
                        "Codex retirement locator plan changed before publication",
                    ));
                }
                let actual = publication.retire_source_generation_page(
                    &key,
                    store_after.as_ref(),
                    CODEX_RETIREMENT_PAGE_UNITS,
                    options.imported_at.timestamp_millis(),
                )?;
                if actual != preview {
                    return Err(CodexNativeVerticalError::CorruptFrontier(
                        "Codex retirement preview diverged from Store authority",
                    ));
                }
                publication.prepare_journal_checkpoint()?;
                revalidate_codex_source_observation(source, &certified_observation)?;
                publication.publish_cursor_set()?;
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                revalidate_codex_source_observation(source, &certified_observation)?;
            }
        }
        let receipt = publication.commit()?;
        #[cfg(codex_nativepath_qualification)]
        super::super::qualification::observe_store_receipt(&receipt);
        let current = receipt
            .published_cursors()
            .first()
            .filter(|_| receipt.published_cursors().len() == 1)
            .cloned()
            .ok_or(CodexNativeVerticalError::CorruptCursor(
                "Codex retirement did not publish one exact cursor",
            ))?;
        committed.expected_store_cursor = current;
        committed.phase = next_phase;
        if preview.done {
            return Ok(true);
        }
        after = next_after;
    }
}

impl CodexNativeProducerTask {
    pub(crate) fn source(&self) -> &CodexCatalogSource {
        &self.source
    }

    pub(crate) fn open(self) -> VerticalResult<CodexNativeWindowProducer> {
        if self
            .committed
            .as_ref()
            .is_some_and(|state| matches!(state.phase, CodexNativeCursorPhase::Retiring { .. }))
        {
            return Err(
                CodexNativeLifecycleGate::SourceMutationRequiresReconciliation {
                    lifecycle: "pending_retirement",
                }
                .into(),
            );
        }
        if let Some(noop) = exact_metadata_noop(&self.source, self.committed.as_ref()) {
            if let Some(observation) =
                observe_ordinary_file_strong_metadata(&self.source.source_path)?
            {
                let observed = CodexFileObservation::from_parts(
                    observation.len(),
                    observation.modified_at(),
                    *observation.token(),
                );
                if observed != self.source.catalog_observation {
                    return Err(CaptureError::InvalidPayload(
                        "Codex catalog observation changed before NativePath admission".to_owned(),
                    )
                    .into());
                }
                return Ok(self.open_prevalidated_noop(noop));
            }
        }
        let resume_proof = self
            .committed
            .as_ref()
            .and_then(|state| state.proof.as_ref());
        let mut scanner = match CodexNativeScanner::new(
            self.source.clone(),
            resume_proof,
            CodexNativeProfile::CoreOnly,
        ) {
            Ok(scanner) => scanner,
            Err(CaptureError::InvalidPayload(message))
                if resume_proof.is_some() && message.starts_with("invalid Codex append proof:") =>
            {
                CodexNativeScanner::new(self.source.clone(), None, CodexNativeProfile::CoreOnly)?
            }
            Err(error) => {
                return Err(map_scan_error(error, self.committed.is_some()));
            }
        };
        let disposition = scanner.disposition();
        let resumed = disposition == super::super::reader::CodexParseDisposition::AppendDelta;
        let source_revision = source_observation_revision(&self.source.catalog_observation);
        let continuing_generation = resumed
            && self.committed.as_ref().is_some_and(|state| {
                state.source_revision == source_revision
                    && state.proof.as_ref().is_some_and(|proof| {
                        proof.checkpoint.observation.len < self.source.catalog_observation.len
                    })
            });
        let generation = match self.committed.as_ref() {
            Some(state) if continuing_generation => state.generation,
            Some(state) => state
                .generation
                .checked_add(1)
                .ok_or(CodexNativeVerticalError::CheckpointGenerationExhausted)?,
            None => 0,
        };
        let expected_frontier = if resumed {
            self.committed
                .as_ref()
                .map(|state| state.frontier.clone())
                .unwrap_or_else(initial_codex_frontier)
        } else {
            initial_codex_frontier()
        };
        let base_retained_events = if resumed {
            self.committed
                .as_ref()
                .map_or(0, |state| state.retained_events)
        } else {
            0
        };
        let base_skipped_events = if resumed {
            self.committed
                .as_ref()
                .map_or(0, |state| state.skipped_events)
        } else {
            0
        };
        let base_rejected_records = if resumed {
            self.committed
                .as_ref()
                .map_or(0, |state| state.rejected_records)
        } else {
            0
        };
        let stage_generation = self
            .committed
            .as_ref()
            .is_some_and(|state| state.phase == CodexNativeCursorPhase::Rebuilding)
            || (self.committed.is_some() && !resumed);
        // Opening a checkpoint may hydrate bounded pending tool authority.
        // Keep no physical-record scratch allocation between preparation
        // windows.
        scanner.release_transient_record_buffer();
        Ok(CodexNativeWindowProducer {
            expected_store_cursor: self
                .committed
                .as_ref()
                .map(|state| state.expected_store_cursor.clone()),
            source: self.source,
            options: self.options,
            identity: self.identity,
            committed: self.committed,
            scanner: Some(scanner),
            generation,
            source_revision,
            expected_frontier,
            base_retained_events,
            base_skipped_events,
            base_rejected_records,
            imported_events: 0,
            scanned_sparse_results: 0,
            published_window: false,
            published_core: false,
            stage_generation,
            pending_step: None,
            prevalidated_noop: None,
        })
    }

    fn open_prevalidated_noop(self, noop: CodexNativeNoop) -> CodexNativeWindowProducer {
        let generation = self.committed.as_ref().map_or(0, |state| state.generation);
        let expected_frontier = self
            .committed
            .as_ref()
            .map(|state| state.frontier.clone())
            .unwrap_or_else(initial_codex_frontier);
        let source_revision = source_observation_revision(&self.source.catalog_observation);
        CodexNativeWindowProducer {
            expected_store_cursor: self
                .committed
                .as_ref()
                .map(|state| state.expected_store_cursor.clone()),
            source: self.source,
            options: self.options,
            identity: self.identity,
            committed: self.committed,
            scanner: None,
            generation,
            source_revision,
            expected_frontier,
            base_retained_events: 0,
            base_skipped_events: 0,
            base_rejected_records: 0,
            imported_events: 0,
            scanned_sparse_results: 0,
            published_window: false,
            published_core: false,
            stage_generation: false,
            pending_step: None,
            prevalidated_noop: Some(noop),
        }
    }
}

impl CodexNativeWindowProducer {
    pub(crate) fn next_window(&mut self) -> VerticalResult<CodexNativeProducerStep> {
        if let Some(noop) = self.prevalidated_noop.take() {
            return Ok(CodexNativeProducerStep::Noop(noop));
        }
        let mut current = match self.pending_step.take() {
            Some(step) => step,
            None => self.next_page_window()?,
        };
        while !current.source_done() {
            let later = self.next_page_window()?;
            match current.try_merge_window(later)? {
                Ok(merged) => current = merged,
                Err((full, later)) => {
                    self.pending_step = Some(later);
                    return Ok(full);
                }
            }
        }
        Ok(current)
    }

    fn next_page_window(&mut self) -> VerticalResult<CodexNativeProducerStep> {
        let mut folded_page = None::<CodexNativePage>;
        loop {
            let mut page = match self
                .scanner
                .as_mut()
                .ok_or(CodexNativeVerticalError::CorruptFrontier(
                    "Codex bounded producer lost its scanner",
                ))?
                .next_page()
                .map_err(|error| map_scan_error(error, self.committed.is_some()))?
            {
                Some(CodexNativeOwnedPage::Core(page)) => Some(*page),
                Some(CodexNativeOwnedPage::Pro(_)) => {
                    return Err(CodexNativeVerticalError::CorruptFrontier(
                        "Core-only bounded producer emitted a Pro page",
                    ));
                }
                None => None,
            };
            let mut exhausted = self
                .scanner
                .as_ref()
                .ok_or(CodexNativeVerticalError::CorruptFrontier(
                    "Codex bounded producer lost its scanner state",
                ))?
                .is_exhausted();
            if !exhausted
                && page.as_ref().is_some_and(|candidate| {
                    candidate.next_safe_frontier.complete_prefix_end
                        == self.source.catalog_observation.len
                })
            {
                let lookahead = self
                    .scanner
                    .as_mut()
                    .ok_or(CodexNativeVerticalError::CorruptFrontier(
                        "Codex bounded producer lost its EOF lookahead scanner",
                    ))?
                    .next_page()
                    .map_err(|error| map_scan_error(error, self.committed.is_some()))?;
                if lookahead.is_some()
                    || !self
                        .scanner
                        .as_ref()
                        .is_some_and(CodexNativeScanner::is_exhausted)
                {
                    return Err(CodexNativeVerticalError::CorruptFrontier(
                        "Codex exact-boundary EOF lookahead produced unexpected work",
                    ));
                }
                exhausted = true;
            }

            if let Some(candidate) = page.as_ref() {
                if candidate.mutation_units() == 0 && !exhausted {
                    if let Some(folded) = folded_page.as_mut() {
                        folded.next_safe_frontier = candidate.next_safe_frontier.clone();
                        folded.physical_records = folded
                            .physical_records
                            .saturating_add(candidate.physical_records);
                    } else {
                        folded_page = page.take();
                    }
                    self.scanner
                        .as_mut()
                        .ok_or(CodexNativeVerticalError::CorruptFrontier(
                            "Codex bounded producer lost its scanner",
                        ))?
                        .release_transient_record_buffer();
                    continue;
                }
            }

            let terminal_scan = if exhausted {
                let scanner =
                    self.scanner
                        .take()
                        .ok_or(CodexNativeVerticalError::CorruptFrontier(
                            "Codex bounded producer lost its terminal scanner",
                        ))?;
                let scan = scanner
                    .finish()
                    .map_err(|error| map_scan_error(error, self.committed.is_some()))?;
                revalidate_codex_source_observation(&scan.source, &scan.after_observation)?;
                validate_lifecycle(&scan, self.committed.as_ref())?;
                Some(scan)
            } else {
                revalidate_codex_source_observation(
                    &self.source,
                    &self.source.catalog_observation,
                )?;
                None
            };

            if page.is_none() {
                page = folded_page.take();
            } else if let Some(folded) = folded_page.take() {
                if let Some(page) = page.as_mut() {
                    page.expected_frontier = folded.expected_frontier;
                    page.physical_records = page
                        .physical_records
                        .saturating_add(folded.physical_records);
                    page.recompute_identity()?;
                }
            }
            let Some(page) = page.take() else {
                let scan = terminal_scan.ok_or(CodexNativeVerticalError::CorruptFrontier(
                    "Codex bounded producer paused without a page",
                ))?;
                if scan.is_observation_replay() {
                    let committed =
                        self.committed
                            .as_ref()
                            .ok_or(CodexNativeVerticalError::CorruptCursor(
                                "observation replay has no committed cursor",
                            ))?;
                    if committed.canonical_journal_frontier.is_none() {
                        return Err(CodexNativeVerticalError::CorruptCursor(
                            "NativePath observation replay has no journal checkpoint",
                        ));
                    }
                    return Ok(CodexNativeProducerStep::Noop(CodexNativeNoop {
                        terminal: scan.terminal(),
                        skipped_events: usize::try_from(
                            committed
                                .retained_events
                                .saturating_add(committed.skipped_events)
                                .saturating_add(committed.rejected_records),
                        )
                        .unwrap_or(usize::MAX),
                        rejected_records: usize::try_from(committed.rejected_records)
                            .unwrap_or(usize::MAX),
                        rejections: Vec::new(),
                        retained_events: committed.retained_events,
                        committed_authority: true,
                    }));
                }
                let owner = scan
                    .owner
                    .clone()
                    .ok_or(CodexNativeVerticalError::MissingOwner)?;
                let page = CodexNativePage::cursor_only(
                    owner,
                    self.expected_frontier.clone(),
                    scan.terminal(),
                )?;
                return self.finish_window(page, Some(scan));
            };

            return self.finish_window(page, terminal_scan);
        }
    }

    fn finish_window(
        &mut self,
        mut page: CodexNativePage,
        terminal_scan: Option<super::super::CodexSourceScan>,
    ) -> VerticalResult<CodexNativeProducerStep> {
        let owner = terminal_scan
            .as_ref()
            .and_then(|scan| scan.owner.clone())
            .or_else(|| {
                self.scanner
                    .as_ref()
                    .and_then(CodexNativeScanner::owner)
                    .cloned()
            })
            .ok_or(CodexNativeVerticalError::MissingOwner)?;
        self.imported_events = self.imported_events.saturating_add(page.core_rows.len());
        self.scanned_sparse_results = self.scanned_sparse_results.saturating_add(
            u64::try_from(
                page.core_rows
                    .iter()
                    .filter(|row| {
                        matches!(
                            row.provider_event.event_type,
                            ctx_history_core::EventType::ToolOutput
                                | ctx_history_core::EventType::CommandOutput
                        )
                    })
                    .count(),
            )
            .unwrap_or(u64::MAX),
        );

        let (checkpoint, scan_rejected_records, retained_events, authority_skipped_events) =
            if let Some(scan) = terminal_scan.as_ref() {
                let checkpoint = scan
                    .checkpoint()
                    .ok_or(CodexNativeVerticalError::MissingOwner)?;
                let scan_rejected_records = scan
                    .counters
                    .malformed_records
                    .saturating_add(scan.counters.oversized_records);
                let retained_events = scan.counters.retained_records.saturating_add(
                    if scan.resume_proof().is_some() {
                        self.base_retained_events
                    } else {
                        0
                    },
                );
                let scanned_skipped = scan
                    .counters
                    .native_result_records
                    .saturating_sub(self.scanned_sparse_results);
                let authority_skipped_events =
                    scanned_skipped.saturating_add(if scan.resume_proof().is_some() {
                        self.base_skipped_events
                    } else {
                        0
                    });
                (
                    checkpoint,
                    scan_rejected_records,
                    retained_events,
                    authority_skipped_events,
                )
            } else {
                let scanner =
                    self.scanner
                        .as_ref()
                        .ok_or(CodexNativeVerticalError::CorruptFrontier(
                            "Codex bounded producer lost its continuation scanner",
                        ))?;
                let checkpoint = scanner.checkpoint_at_frontier(&page.next_safe_frontier)?;
                let counters = scanner.counters();
                let scanned_rejected_records = counters
                    .malformed_records
                    .saturating_add(counters.oversized_records);
                let retained_events = counters
                    .retained_records
                    .saturating_add(self.base_retained_events);
                let authority_skipped_events = counters
                    .native_result_records
                    .saturating_sub(self.scanned_sparse_results)
                    .saturating_add(self.base_skipped_events);
                (
                    checkpoint,
                    scanned_rejected_records,
                    retained_events,
                    authority_skipped_events,
                )
            };
        let rejected_records = self
            .base_rejected_records
            .saturating_add(scan_rejected_records);
        let source_done = terminal_scan.is_some();
        let source_terminal = terminal_scan.as_ref().is_some_and(|scan| scan.terminal());
        if source_terminal
            && self.committed.is_none()
            && !self.published_core
            && page.core_rows.is_empty()
        {
            let scan = terminal_scan.ok_or(CodexNativeVerticalError::CorruptFrontier(
                "fresh empty Codex source lost its terminal scan",
            ))?;
            return Ok(CodexNativeProducerStep::Noop(CodexNativeNoop {
                terminal: true,
                skipped_events: usize::try_from(
                    authority_skipped_events.saturating_add(rejected_records),
                )
                .unwrap_or(usize::MAX),
                rejected_records: usize::try_from(rejected_records).unwrap_or(usize::MAX),
                rejections: scan.rejections,
                retained_events: 0,
                committed_authority: false,
            }));
        }
        page.terminal = source_terminal;
        page.recompute_identity()?;
        let certified_observation = terminal_scan
            .as_ref()
            .map(|scan| scan.after_observation.clone())
            .unwrap_or_else(|| self.source.catalog_observation.clone());
        let cursor_phase = if self.stage_generation {
            if source_done && source_terminal {
                CodexNativeCursorPhase::Retiring { after: None }
            } else {
                CodexNativeCursorPhase::Rebuilding
            }
        } else if source_done && source_terminal {
            CodexNativeCursorPhase::Complete
        } else {
            CodexNativeCursorPhase::Core
        };
        let next_store_cursor = build_next_store_cursor(
            &self.options,
            &self.identity,
            self.generation,
            &self.source_revision,
            &checkpoint,
            &certified_observation,
            cursor_phase.clone(),
            rejected_records,
            retained_events,
            authority_skipped_events,
        )?;
        let expected_store_cursor = self.expected_store_cursor.take();
        let expected_frontier = self.expected_frontier.clone();
        let next_frontier = page.next_safe_frontier.clone();
        let context = CodexPublicationContext {
            source: self.source.clone(),
            certified_observation,
            options: self.options.clone(),
            canonical_source_key: self.identity.canonical_source_key.clone(),
            proposed_source_namespace: self.identity.proposed_source_namespace.clone(),
            root_namespace: self.identity.root_namespace.clone(),
            parent_native_session_id: self.source.catalog_parent_native_session_id.clone(),
            root_native_session_id: self.source.catalog_root_native_session_id.clone(),
            cursor_stream: self.identity.cursor_stream.clone(),
            source_revision: self.source_revision.clone(),
            owner,
            generation: self.generation,
            checkpoint,
            rejected_records,
            retained_events,
            skipped_events: authority_skipped_events,
            stage_generation: self.stage_generation,
        };
        let has_core_rows = !page.core_rows.is_empty();
        let delta = CodexNativeCommittedDelta {
            imported_sessions: usize::from(!self.published_core && has_core_rows),
            imported_events: page.core_rows.len(),
            imported_edges: usize::from(
                !self.published_core
                    && has_core_rows
                    && self.source.catalog_parent_native_session_id.is_some(),
            ),
        };
        let chunk = CodexNativeRootChunk::new(
            context,
            vec![page],
            expected_store_cursor,
            next_store_cursor.clone(),
            expected_frontier,
            next_frontier.clone(),
            source_terminal,
        )?;
        self.expected_store_cursor = Some(next_store_cursor);
        self.expected_frontier = next_frontier;
        self.published_window = true;
        self.published_core |= has_core_rows;
        if let Some(scanner) = self.scanner.as_mut() {
            scanner.release_transient_record_buffer();
        }
        let report = terminal_scan.map(|scan| {
            let skipped_events = scan
                .counters
                .native_result_records
                .saturating_sub(self.scanned_sparse_results);
            let terminal = scan.terminal();
            CodexNativeTerminalReport {
                skipped_events: usize::try_from(skipped_events).unwrap_or(usize::MAX),
                rejected_records: usize::try_from(scan_rejected_records).unwrap_or(usize::MAX),
                rejections: scan.rejections,
                retained_events,
                terminal,
            }
        });
        Ok(CodexNativeProducerStep::Window {
            chunk,
            source_done,
            delta,
            report,
        })
    }
}

fn exact_metadata_noop(
    source: &CodexCatalogSource,
    committed: Option<&CodexCommittedSource>,
) -> Option<CodexNativeNoop> {
    let committed = committed?;
    let proof = committed.proof.as_ref()?;
    if committed.phase != CodexNativeCursorPhase::Complete
        || committed.canonical_journal_frontier.is_none()
        || committed.certified_observation.as_ref() != Some(&source.catalog_observation)
        || proof.checkpoint.observation != source.catalog_observation
        || committed.source_revision != source_observation_revision(&source.catalog_observation)
    {
        return None;
    }
    Some(CodexNativeNoop {
        terminal: proof.checkpoint.terminal(),
        skipped_events: usize::try_from(
            committed
                .retained_events
                .saturating_add(committed.skipped_events)
                .saturating_add(committed.rejected_records),
        )
        .unwrap_or(usize::MAX),
        rejected_records: usize::try_from(committed.rejected_records).unwrap_or(usize::MAX),
        rejections: Vec::new(),
        retained_events: committed.retained_events,
        committed_authority: true,
    })
}

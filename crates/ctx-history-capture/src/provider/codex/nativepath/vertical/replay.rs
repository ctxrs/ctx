use super::*;

#[derive(Debug)]
pub(crate) struct CodexNativeOutputReplay {
    source: CodexCatalogSource,
    certified_observation: CodexFileObservation,
    scanner: Option<CodexNativeScanner>,
    committed_checkpoint: CodexNativeCheckpoint,
    output_source: OutputSourceIdentity,
    native_source_identity: NativeSourceIdentity,
    plan: OutputPlan,
    final_frontier: CodexNativeFrontier,
    current_revision: String,
    parser_revision: String,
    materializer_revision: String,
    last_output_frontier: Option<CodexNativeFrontier>,
    finished: bool,
}

impl CodexNativeOutputReplay {
    pub(crate) fn next_page(
        &mut self,
        output_sink: &dyn ProOutputSink,
    ) -> VerticalResult<
        Option<
            std::result::Result<
                crate::provider::native_ingestion::NativeOutputPageReceipt,
                Box<NativeProReplayFailure>,
            >,
        >,
    > {
        if self.finished || self.plan.noop {
            self.finished = true;
            return Ok(None);
        }
        loop {
            let next = self
                .scanner
                .as_mut()
                .ok_or(CodexNativeVerticalError::CorruptOutputProgress(
                    "output replay lost its bounded scanner",
                ))?
                .next_page()?;
            match next {
                Some(CodexNativeOwnedPage::Core(_)) => continue,
                Some(CodexNativeOwnedPage::Pro(page)) => {
                    self.last_output_frontier = Some(page.next_safe_frontier.clone());
                    if self.plan.skip_page(&page)? {
                        continue;
                    }
                    revalidate_codex_source_observation(&self.source, &self.certified_observation)?;
                    let page = adapt_output_page(
                        *page,
                        &mut self.plan,
                        &self.output_source,
                        &self.native_source_identity,
                        &self.final_frontier,
                        &self.current_revision,
                        &self.parser_revision,
                        &self.materializer_revision,
                        output_sink,
                    )?;
                    return Ok(Some(process_pro_replay_only(page, output_sink)));
                }
                None => {
                    let scanner = self.scanner.take().ok_or(
                        CodexNativeVerticalError::CorruptOutputProgress(
                            "output replay lost its terminal scanner",
                        ),
                    )?;
                    let scan = scanner.finish()?;
                    let checkpoint = scan
                        .checkpoint()
                        .ok_or(CodexNativeVerticalError::MissingOwner)?;
                    if checkpoint != self.committed_checkpoint
                        || source_revision(&scan.full_revision_sha256) != self.current_revision
                    {
                        return Err(CodexNativeLifecycleGate::OutputReplaySourceChanged.into());
                    }
                    self.plan.finish_skip()?;
                    revalidate_codex_source_observation(&self.source, &self.certified_observation)?;
                    self.finished = true;
                    if self.last_output_frontier.as_ref() == Some(&self.final_frontier) {
                        return Ok(None);
                    }
                    let expected_frontier = self
                        .last_output_frontier
                        .clone()
                        .unwrap_or_else(initial_codex_frontier);
                    let page = CodexNativeProOutputPage {
                        identity: Default::default(),
                        expected_frontier: expected_frontier.clone(),
                        next_safe_frontier: self.final_frontier.clone(),
                        outputs: Vec::new(),
                        serialized_bytes: 4 * 1024,
                    };
                    let page = adapt_output_page(
                        page,
                        &mut self.plan,
                        &self.output_source,
                        &self.native_source_identity,
                        &self.final_frontier,
                        &self.current_revision,
                        &self.parser_revision,
                        &self.materializer_revision,
                        output_sink,
                    )?;
                    return Ok(Some(process_pro_replay_only(page, output_sink)));
                }
            }
        }
    }
}

pub(crate) fn prepare_codex_native_output_replay(
    store: &Store,
    source: CodexCatalogSource,
    options: CodexNativeStoreOptions,
    output_sink: &dyn ProOutputSink,
) -> VerticalResult<CodexNativeOutputReplay> {
    let identity = source_projection_identity(&source)?;
    let committed = load_committed_source(store, &source, &options, &identity)?
        .ok_or(CodexNativeLifecycleGate::OutputReplayRequiresCommittedCore)?;
    if committed.phase != CodexNativeCursorPhase::Complete {
        return Err(CodexNativeLifecycleGate::OutputReplayRequiresCommittedCore.into());
    }
    let committed_proof = committed
        .proof
        .as_ref()
        .ok_or(CodexNativeLifecycleGate::OutputReplayRequiresCommittedCore)?;
    let certified_observation =
        committed
            .certified_observation
            .clone()
            .ok_or(CodexNativeVerticalError::CorruptCursor(
                "output replay has no certified source observation",
            ))?;
    if source.catalog_observation != certified_observation
        || committed_proof.checkpoint.observation != certified_observation
        || !committed_proof.checkpoint.terminal()
    {
        return Err(CodexNativeLifecycleGate::OutputReplaySourceChanged.into());
    }
    revalidate_codex_source_observation(&source, &certified_observation)?;
    let locator = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id,
        locator_identity: source_locator_identity(
            &identity.cursor_stream,
            &identity.proposed_source_namespace,
        ),
        cursor_stream: identity.cursor_stream.clone(),
        proposed_source_identity: identity.proposed_source_namespace,
        raw_source_path: Some(source.source_path.display().to_string()),
        source_revision: committed.source_revision.clone(),
        observed_at_ms: options.imported_at.timestamp_millis(),
    };
    let resolution = store.plan_provider_source_locator(&locator)?;
    let output_source = OutputSourceIdentity {
        provider: CODEX_PROVIDER.to_owned(),
        namespace_id: identity.cursor_stream,
        source_id: resolution.canonical_source_identity.clone(),
    };
    let final_frontier = frontier_from_checkpoint(&committed_proof.checkpoint);
    let current_revision = source_revision(&committed_proof.checkpoint.full_revision_sha256);
    let parser_revision = output_parser_revision();
    let materializer_revision = output_sink.materializer_revision().to_owned();
    let progress = output_sink.observe_source(&output_source)?;
    let plan = OutputPlan::new(
        progress,
        &initial_codex_frontier(),
        &final_frontier,
        &current_revision,
        &parser_revision,
        &materializer_revision,
        Some(&committed),
        true,
    )?;
    let scanner = CodexNativeScanner::new(source.clone(), None, CodexNativeProfile::CoreAndPro)?;
    Ok(CodexNativeOutputReplay {
        source,
        certified_observation,
        scanner: Some(scanner),
        committed_checkpoint: committed_proof.checkpoint.clone(),
        output_source,
        native_source_identity: NativeSourceIdentity::new(
            CODEX_PROVIDER,
            resolution.canonical_source_identity,
        ),
        plan,
        final_frontier,
        current_revision,
        parser_revision,
        materializer_revision,
        last_output_frontier: None,
        finished: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn adapt_output_page(
    page: CodexNativeProOutputPage,
    plan: &mut OutputPlan,
    output_source: &OutputSourceIdentity,
    native_source_identity: &NativeSourceIdentity,
    final_frontier: &CodexNativeFrontier,
    current_revision: &str,
    parser_revision: &str,
    materializer_revision: &str,
    sink: &dyn ProOutputSink,
) -> VerticalResult<NativeProReplayPage> {
    let expected_frontier = safe_frontier(&page.expected_frontier)?;
    let next_safe_frontier = safe_frontier(&page.next_safe_frontier)?;
    let terminal = page.next_safe_frontier == *final_frontier;
    let output = crate::provider::native_ingestion::NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: output_source.clone(),
        source_epoch: plan.source_epoch,
        observed_revision: current_revision.to_owned(),
        parser_revision: parser_revision.to_owned(),
        materializer_revision: materializer_revision.to_owned(),
        disposition: plan.disposition,
        expected_prior_source_epoch: plan.expected_source_epoch,
        expected_prior_frontier: plan.expected_sink_frontier.clone(),
        observations: page.outputs,
    };
    let accounting = NativePageAccounting {
        logical_units: output.observations.len(),
        conservative_serialized_bytes: page.serialized_bytes,
    };
    let adapted = NativeProReplayPage::new_with_source_identity(
        native_source_identity.clone(),
        expected_frontier,
        next_safe_frontier.clone(),
        terminal,
        accounting,
        output,
    )?;
    plan.expected_source_epoch = Some(plan.source_epoch);
    plan.expected_sink_frontier = Some(next_safe_frontier);
    plan.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(adapted)
}

#[derive(Debug)]
struct OutputPlan {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    skip_frontier: Option<CodexNativeFrontier>,
    noop: bool,
}

impl OutputPlan {
    #[allow(clippy::too_many_arguments)]
    fn new(
        progress: Option<ProOutputProgress>,
        acquisition_frontier: &CodexNativeFrontier,
        final_frontier: &CodexNativeFrontier,
        current_revision: &str,
        parser_revision: &str,
        materializer_revision: &str,
        committed: Option<&CodexCommittedSource>,
        full_replay: bool,
    ) -> VerticalResult<Self> {
        let Some(progress) = progress else {
            if committed.is_some() && *acquisition_frontier != initial_codex_frontier() {
                return Err(CodexNativeVerticalError::CorruptOutputProgress(
                    "output source is absent behind a nonzero Core frontier",
                ));
            }
            return Ok(Self {
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                skip_frontier: None,
                noop: false,
            });
        };
        let progress_cursor = progress
            .cursor
            .as_ref()
            .map(output_cursor_frontier)
            .transpose()?;
        let revision_rewrite = progress.observed_revision != current_revision
            || progress.parser_revision != parser_revision
            || progress.materializer_revision != materializer_revision;
        if revision_rewrite {
            if !full_replay && *acquisition_frontier != initial_codex_frontier() {
                return Err(CodexNativeVerticalError::CorruptOutputProgress(
                    "output revision rewrite requires a full output-only replay",
                ));
            }
            let source_epoch = progress
                .source_epoch
                .checked_add(1)
                .ok_or(CodexNativeVerticalError::OutputSourceEpochExhausted)?;
            return Ok(Self {
                source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: progress
                    .cursor
                    .as_ref()
                    .map(output_cursor_safe_frontier)
                    .transpose()?,
                disposition: ProOutputSourceDisposition::Rewrite,
                skip_frontier: None,
                noop: false,
            });
        }
        let Some(progress_frontier) = progress_cursor else {
            return Err(CodexNativeVerticalError::CorruptOutputProgress(
                "existing output source has no cursor",
            ));
        };
        if progress.observed_revision == current_revision
            && progress_frontier == *final_frontier
            && progress.terminal
        {
            return Ok(Self {
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: progress
                    .cursor
                    .as_ref()
                    .map(output_cursor_safe_frontier)
                    .transpose()?,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                skip_frontier: None,
                noop: true,
            });
        }
        if !full_replay && progress_frontier != *acquisition_frontier {
            return Err(CodexNativeVerticalError::CorruptOutputProgress(
                "output cursor does not match the Core acquisition frontier",
            ));
        }
        Ok(Self {
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: progress
                .cursor
                .as_ref()
                .map(output_cursor_safe_frontier)
                .transpose()?,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            skip_frontier: (full_replay && progress_frontier != initial_codex_frontier())
                .then_some(progress_frontier),
            noop: false,
        })
    }

    fn skip_page(&mut self, page: &CodexNativeProOutputPage) -> VerticalResult<bool> {
        let Some(frontier) = self.skip_frontier.as_ref() else {
            return Ok(false);
        };
        if page.next_safe_frontier == *frontier {
            self.skip_frontier = None;
            return Ok(true);
        }
        if page.next_safe_frontier.complete_prefix_end > frontier.complete_prefix_end
            || page.next_safe_frontier.next_raw_ordinal > frontier.next_raw_ordinal
        {
            return Err(CodexNativeVerticalError::CorruptOutputProgress(
                "output cursor is not a certified source page boundary",
            ));
        }
        Ok(true)
    }

    fn finish_skip(&self) -> VerticalResult<()> {
        if self.skip_frontier.is_some() {
            return Err(CodexNativeVerticalError::CorruptOutputProgress(
                "output cursor is not a certified source page boundary",
            ));
        }
        Ok(())
    }
}

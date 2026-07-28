use super::*;

impl ClineNativeReader {
    pub(super) fn advance_active_task(
        &mut self,
    ) -> Result<Option<ClineCertifiedPage>, ClineNativePathError> {
        let mut task = self
            .active_task
            .take()
            .ok_or_else(|| ClineNativePathError::Invariant {
                message: "Cline active task disappeared".to_owned(),
            })?;
        if task.next_component >= task.event_components.len() {
            if task.component_failed
                && !task.component_page_certified
                && task.discard_deferred_metadata_on_failure
            {
                task.deferred_metadata_page = None;
                return Ok(None);
            }
            let has_retained_rows = [
                task.api_history.as_ref(),
                task.ui_messages.as_ref(),
                task.fallback_history.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|checkpoint| checkpoint.retained_rows != 0);
            if !has_retained_rows && task.discard_deferred_metadata_on_failure {
                task.deferred_metadata_page = None;
            }
            if let Some(page) = task.deferred_metadata_page.take() {
                if let Some(failure) = Self::certify_metadata_boundary(
                    &task.metadata,
                    &mut task.metadata_content_authority,
                )? {
                    self.outcomes.push(component_failure_outcome(failure));
                    if let Some(previous) =
                        self.previous_by_path.get(&task.task.canonical_task_path)
                    {
                        self.live_checkpoints.push(previous.clone());
                    }
                    return Ok(None);
                }
                self.stats.pages_certified = self.stats.pages_certified.saturating_add(1);
                self.stats.max_pages_buffered = self.stats.max_pages_buffered.max(1);
                self.active_task = Some(task);
                return Ok(Some(page));
            }
            self.live_checkpoints.push(ClineTaskCheckpoint {
                identity: task.metadata.session.identity.clone(),
                canonical_task_path: task.task.canonical_task_path.clone(),
                api_history: task.api_history,
                ui_messages: task.ui_messages,
                fallback_history: task.fallback_history,
                task_metadata: task.metadata,
            });
            return Ok(None);
        }
        let component = task.event_components[task.next_component];
        task.next_component = task.next_component.saturating_add(1);
        let prior = match component {
            ClineEventComponent::ApiHistory => task.api_history.clone(),
            ClineEventComponent::UiMessages => task.ui_messages.clone(),
            ClineEventComponent::FallbackHistory => task.fallback_history.clone(),
        };
        let observation = task.task.component(component.source_component()).clone();
        if !task.identity_changed
            && prior
                .as_ref()
                .is_some_and(|prior| prior.observation == observation)
        {
            if let Some(failure) = component_authority_failure(&observation, false)?
                .or(directory_authority_failure(&task.task, &observation)?)
            {
                task.component_failed = true;
                self.outcomes.push(component_failure_outcome(failure));
            } else {
                self.outcomes.push(ClineComponentReadOutcome {
                    component: observation.component,
                    path: observation.path.clone(),
                    transition: Some(ClineComponentTransition::Unchanged),
                    pages: 0,
                    failure: None,
                });
            }
            self.active_task = Some(task);
            return Ok(None);
        }
        match &observation.state {
            ClineObservedFileState::Unavailable(message) => {
                task.component_failed = true;
                self.outcomes
                    .push(component_failure_outcome(ClineComponentFailure {
                        component: observation.component,
                        path: observation.path.clone(),
                        kind: ClineComponentFailureKind::LocalIo,
                        message: message.clone(),
                        retryable: true,
                    }));
                self.active_task = Some(task);
                Ok(None)
            }
            ClineObservedFileState::Missing => {
                self.run_before_exposure(&observation.path, observation.component);
                if let Some(failure) = component_authority_failure(&observation, false)?
                    .or(directory_authority_failure(&task.task, &observation)?)
                {
                    task.component_failed = true;
                    self.outcomes.push(component_failure_outcome(failure));
                    self.active_task = Some(task);
                    return Ok(None);
                }
                if prior.is_none() {
                    self.outcomes.push(ClineComponentReadOutcome {
                        component: observation.component,
                        path: observation.path.clone(),
                        transition: Some(ClineComponentTransition::MissingPhysical),
                        pages: 0,
                        failure: None,
                    });
                    Self::set_task_component(&mut task, component, None);
                    self.active_task = Some(task);
                    return Ok(None);
                }
                let metadata_failure = Self::certify_metadata_boundary(
                    &task.metadata,
                    &mut task.metadata_content_authority,
                )?;
                let deletion_authority_failure = metadata_failure
                    .or_else(|| deletion_metadata_authority_refusal(&task.metadata, &observation));
                if let Some(failure) = deletion_authority_failure {
                    task.component_failed = true;
                    self.outcomes.push(ClineComponentReadOutcome {
                        component: observation.component,
                        path: observation.path.clone(),
                        transition: None,
                        pages: 0,
                        failure: Some(failure),
                    });
                    self.active_task = Some(task);
                    return Ok(None);
                }
                let page = self.build_deleted_array_page(
                    &task.metadata,
                    component,
                    &observation,
                    prior.as_ref(),
                )?;
                Self::set_task_component(&mut task, component, None);
                task.needs_session = false;
                self.outcomes.push(ClineComponentReadOutcome {
                    component: observation.component,
                    path: observation.path.clone(),
                    transition: Some(ClineComponentTransition::MissingPhysical),
                    pages: 1,
                    failure: None,
                });
                self.stats.pages_certified = self.stats.pages_certified.saturating_add(1);
                self.active_task = Some(task);
                Ok(Some(self.expose_component_page_after_metadata(page)?))
            }
            ClineObservedFileState::Present(_) => {
                let scanner = match ClineArrayScanner::open(
                    &observation,
                    &mut self.stats,
                    self.profile.wants_record_evidence(),
                ) {
                    Ok(scanner) => scanner,
                    Err(ClineLocalReadError::Local(failure)) => {
                        task.component_failed = true;
                        self.outcomes.push(component_failure_outcome(failure));
                        self.active_task = Some(task);
                        return Ok(None);
                    }
                    Err(ClineLocalReadError::Fatal(error)) => return Err(error),
                };
                let source = file_source(
                    task.task.dialect,
                    &task.metadata.session,
                    observation.component,
                    &observation.path,
                    released_ordinal_offset(&task, component),
                );
                let revision = certified_revision(&observation, scanner.revision_sha256());
                let frontier = ClinePageFrontier::zero(component);
                let prior_prefix_matches = prior.as_ref().is_some_and(|prior| {
                    prior.observed_items == 0 && prior.final_frontier == frontier
                });
                let page_transition = if prior.is_some() {
                    ClineComponentTransition::Rewrite
                } else {
                    ClineComponentTransition::Cold
                };
                self.active_array = Some(ActiveArray {
                    task: task.task.clone(),
                    metadata: task.metadata.clone(),
                    component,
                    prior,
                    scanner,
                    source,
                    revision,
                    frontier,
                    observed_items: 0,
                    retained_rows: 0,
                    native_id_occurrences: BTreeMap::new(),
                    prior_prefix_matches,
                    attach_session: task.needs_session,
                    page_transition,
                    pages: 0,
                });
                self.active_task = Some(task);
                Ok(None)
            }
        }
    }

    pub(super) fn advance_active_array(
        &mut self,
    ) -> Result<Option<ClineCertifiedPage>, ClineNativePathError> {
        let mut active =
            self.active_array
                .take()
                .ok_or_else(|| ClineNativePathError::Invariant {
                    message: "Cline active array disappeared".to_owned(),
                })?;
        let step = match active.scanner.next_step() {
            Ok(step) => step,
            Err(ClineLocalReadError::Local(failure)) => {
                self.finish_array_failure(&active, failure);
                return Ok(None);
            }
            Err(ClineLocalReadError::Fatal(error)) => return Err(error),
        };
        match step {
            ClineArrayScanStep::EmptyTerminal { complete_bytes } => {
                let checkpoint = ClineArrayCheckpoint::new(
                    active.component,
                    active
                        .task
                        .component(active.component.source_component())
                        .clone(),
                    active.revision.revision_sha256,
                    complete_bytes,
                    0,
                    0,
                    active.frontier.clone(),
                );
                let transition = classify_transition(
                    active.prior.as_ref(),
                    &checkpoint,
                    active.prior_prefix_matches,
                );
                if let Some(failure) = self.certify_array_boundary(&active)? {
                    self.finish_array_failure(&active, failure);
                    return Ok(None);
                }
                let evidence = if transition == ClineComponentTransition::ControlOnlyRewrite {
                    ClineTerminalEvidence::ControlOnly {
                        certified_revision_sha256: active.revision.revision_sha256,
                    }
                } else {
                    ClineTerminalEvidence::CompleteArray {
                        observed_items: 0,
                        complete_bytes,
                        certified_revision_sha256: active.revision.revision_sha256,
                    }
                };
                let page = match self.make_array_page(
                    active.source.clone(),
                    active.revision.clone(),
                    active.frontier.clone(),
                    active.frontier.clone(),
                    active.page_transition,
                    None,
                    Some(checkpoint.clone()),
                    true,
                    Some(evidence),
                    active
                        .attach_session
                        .then(|| active.metadata.session.clone()),
                ) {
                    Ok(page) => page,
                    Err(failure) => {
                        self.finish_array_failure(&active, failure);
                        return Ok(None);
                    }
                };
                active.pages = active.pages.saturating_add(1);
                self.finish_array_success(&active, checkpoint, transition);
                self.stats.pages_certified = self.stats.pages_certified.saturating_add(1);
                Ok(Some(self.expose_component_page_after_metadata(page)?))
            }
            ClineArrayScanStep::Item(scanned) => {
                let terminal = scanned.terminal;
                let complete_bytes = scanned.complete_bytes;
                let item = parse_scanned_item(
                    scanned,
                    &active.source,
                    &active.metadata.session.identity,
                    active.component,
                    self.profile,
                    super::super::normalize::CLINE_NATIVE_PAGE_MAX_UNITS
                        .saturating_sub(CLINE_NATIVE_FIXED_PAGE_UNITS)
                        .saturating_sub(
                            usize::from(active.attach_session)
                                .saturating_mul(CLINE_NATIVE_SESSION_PAGE_UNITS),
                        ),
                    &mut active.native_id_occurrences,
                    &mut self.stats,
                );
                active.retained_rows = active
                    .retained_rows
                    .saturating_add(u64::try_from(item.rows.len()).unwrap_or(u64::MAX));
                active.observed_items = active.observed_items.saturating_add(1);
                let expected = active.frontier.clone();
                let next = expected.advance(&item.checkpoint);
                active.frontier = next.clone();
                if active
                    .prior
                    .as_ref()
                    .is_some_and(|prior| prior.observed_items == active.observed_items)
                {
                    active.prior_prefix_matches = active
                        .prior
                        .as_ref()
                        .is_some_and(|prior| prior.final_frontier == active.frontier);
                }
                let terminal_checkpoint = if terminal {
                    Some(ClineArrayCheckpoint::new(
                        active.component,
                        active
                            .task
                            .component(active.component.source_component())
                            .clone(),
                        active.revision.revision_sha256,
                        complete_bytes.ok_or_else(|| ClineNativePathError::Invariant {
                            message: "terminal Cline array item lacks an EOF boundary".to_owned(),
                        })?,
                        active.observed_items,
                        active.retained_rows,
                        active.frontier.clone(),
                    ))
                } else {
                    None
                };
                let transition = terminal_checkpoint.as_ref().map(|checkpoint| {
                    classify_transition(
                        active.prior.as_ref(),
                        checkpoint,
                        active.prior_prefix_matches,
                    )
                });
                if let Some(failure) = self.certify_array_boundary(&active)? {
                    self.finish_array_failure(&active, failure);
                    return Ok(None);
                }
                let evidence = terminal_checkpoint.as_ref().map(|checkpoint| {
                    if transition == Some(ClineComponentTransition::ControlOnlyRewrite) {
                        ClineTerminalEvidence::ControlOnly {
                            certified_revision_sha256: checkpoint.certified_revision_sha256,
                        }
                    } else {
                        ClineTerminalEvidence::CompleteArray {
                            observed_items: checkpoint.observed_items,
                            complete_bytes: checkpoint.complete_bytes,
                            certified_revision_sha256: checkpoint.certified_revision_sha256,
                        }
                    }
                });
                let page = match self.make_array_page(
                    active.source.clone(),
                    active.revision.clone(),
                    expected,
                    next,
                    active.page_transition,
                    Some(item),
                    terminal_checkpoint.clone(),
                    terminal,
                    evidence,
                    active
                        .attach_session
                        .then(|| active.metadata.session.clone()),
                ) {
                    Ok(page) => page,
                    Err(failure) => {
                        self.finish_array_failure(&active, failure);
                        return Ok(None);
                    }
                };
                active.pages = active.pages.saturating_add(1);
                if active.attach_session {
                    if let Some(task) = self.active_task.as_mut() {
                        task.needs_session = false;
                    }
                }
                active.attach_session = false;
                self.stats.pages_certified = self.stats.pages_certified.saturating_add(1);
                if let (Some(checkpoint), Some(transition)) = (terminal_checkpoint, transition) {
                    self.finish_array_success(&active, checkpoint, transition);
                } else {
                    self.active_array = Some(active);
                }
                Ok(Some(self.expose_component_page_after_metadata(page)?))
            }
        }
    }
}

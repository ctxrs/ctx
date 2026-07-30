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
            if task.component_failed && !task.component_page_certified {
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
            if !has_retained_rows {
                task.deferred_metadata_page = None;
            }
            if let Some(page) = task.deferred_metadata_page.take() {
                if let Some(failure) = Self::certify_metadata_boundary(
                    &task.metadata,
                    &mut task.metadata_content_authority,
                )? {
                    self.outcomes.push(component_failure_outcome(failure));
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
        let observation = task.task.component(component.source_component()).clone();
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
                if let Some(failure) = component_authority_failure(&observation, false)?
                    .or(directory_authority_failure(&task.task, &observation)?)
                {
                    task.component_failed = true;
                    self.outcomes.push(component_failure_outcome(failure));
                    self.active_task = Some(task);
                    return Ok(None);
                }
                Self::set_task_component(&mut task, component, None);
                self.outcomes.push(ClineComponentReadOutcome {
                    component: observation.component,
                    path: observation.path.clone(),
                    transition: Some(ClineComponentTransition::MissingPhysical),
                    pages: 0,
                    failure: None,
                });
                self.active_task = Some(task);
                Ok(None)
            }
            ClineObservedFileState::Present(_) => {
                let scanner = match ClineArrayScanner::open(&observation, &mut self.stats, true) {
                    Ok(scanner) => scanner,
                    Err(ClineLocalReadError::Local(failure)) => {
                        task.component_failed = true;
                        self.outcomes.push(component_failure_outcome(failure));
                        self.active_task = Some(task);
                        return Ok(None);
                    }
                    Err(ClineLocalReadError::Fatal(error)) => return Err(error),
                };
                let source = file_source(observation.component, &observation.path);
                let revision_sha256 = scanner.revision_sha256();
                let frontier = ClinePageFrontier::zero(component);
                self.active_array = Some(ActiveArray {
                    task: task.task.clone(),
                    metadata: task.metadata.clone(),
                    component,
                    scanner,
                    source,
                    revision_sha256,
                    frontier,
                    observed_items: 0,
                    retained_rows: 0,
                    native_id_occurrences: BTreeMap::new(),
                    attach_session: task.needs_session,
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
                    active.revision_sha256,
                    complete_bytes,
                    0,
                    0,
                    active.frontier.clone(),
                );
                let transition = cold_transition(&checkpoint);
                if let Some(failure) = self.certify_array_boundary(&active)? {
                    self.finish_array_failure(&active, failure);
                    return Ok(None);
                }
                let page = match self.make_array_page(
                    active.source.clone(),
                    None,
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
                    &active.metadata.session.identity,
                    active.component,
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
                let terminal_checkpoint = if terminal {
                    Some(ClineArrayCheckpoint::new(
                        active.component,
                        active
                            .task
                            .component(active.component.source_component())
                            .clone(),
                        active.revision_sha256,
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
                let transition = terminal_checkpoint.as_ref().map(cold_transition);
                if let Some(failure) = self.certify_array_boundary(&active)? {
                    self.finish_array_failure(&active, failure);
                    return Ok(None);
                }
                let page = match self.make_array_page(
                    active.source.clone(),
                    Some(item),
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

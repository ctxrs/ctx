use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::{
    normalize::{
        core_payload_fingerprint, estimated_frontier_bytes, estimated_metadata_checkpoint_bytes,
        estimated_output_bytes, estimated_rejection_bytes, estimated_revision_bytes,
        estimated_session_bytes, estimated_source_bytes, page_identity, ClineArrayCheckpoint,
        ClineCatalogCompletion, ClineCatalogIndex, ClineCatalogRejection, ClineCertifiedPage,
        ClineCertifiedRevision, ClineComponentFailure, ClineComponentFailureKind,
        ClineComponentReadOutcome, ClineComponentTransition, ClineCorePayload, ClineEventComponent,
        ClineFileSourceIdentity, ClineItemRejection, ClineItemRejectionKind,
        ClineMetadataCheckpoint, ClineNativeProfile, ClinePageAccounting, ClinePageFrontier,
        ClinePublicationStats, ClineSessionRow, ClineTaskCheckpoint, ClineTaskIdentity,
        ClineTaskIdentityOrigin, ClineTerminalEvidence, ClineTransientOutputPayload,
        CLINE_NATIVE_CORE_PAGE_MAX_BYTES, CLINE_NATIVE_MAX_REJECTIONS, CLINE_NATIVE_PAGE_MAX_BYTES,
        CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES,
    },
    parse::{
        hydrate_component, parse_metadata, parse_root_index, parse_scanned_item,
        pin_component_content, ClineArrayScanStep, ClineArrayScanner, ClineLocalReadError,
        ClinePinnedContentAuthority, ParsedItem,
    },
    source::{
        is_component_local_error, ClineComponent, ClineComponentObservation, ClineDiscovery,
        ClineLiveTaskObservation, ClineObservedFileState, TaskJsonNativeDialect,
    },
    ClineNativePathError,
};

pub(crate) struct ClineNativeReader {
    discovery: ClineDiscovery,
    dialect: TaskJsonNativeDialect,
    profile: ClineNativeProfile,
    previous_by_path: BTreeMap<PathBuf, ClineTaskCheckpoint>,
    route_index: usize,
    pending_page: Option<ClineCertifiedPage>,
    active_task: Option<ActiveTask>,
    active_array: Option<ActiveArray>,
    outcomes: Vec<ClineComponentReadOutcome>,
    live_checkpoints: Vec<ClineTaskCheckpoint>,
    stats: ClinePublicationStats,
    catalog_finished: bool,
    #[cfg(test)]
    before_exposure: Option<Box<dyn FnMut(&Path, ClineComponent)>>,
}

struct ActiveTask {
    task: Box<ClineLiveTaskObservation>,
    metadata: ClineMetadataCheckpoint,
    metadata_content_authority: Option<ClinePinnedContentAuthority>,
    identity_changed: bool,
    api_history: Option<ClineArrayCheckpoint>,
    ui_messages: Option<ClineArrayCheckpoint>,
    fallback_history: Option<ClineArrayCheckpoint>,
    event_components: Box<[ClineEventComponent]>,
    next_component: usize,
    needs_session: bool,
}

struct ActiveArray {
    task: Box<ClineLiveTaskObservation>,
    metadata: ClineMetadataCheckpoint,
    component: ClineEventComponent,
    prior: Option<ClineArrayCheckpoint>,
    scanner: ClineArrayScanner,
    source: ClineFileSourceIdentity,
    revision: ClineCertifiedRevision,
    frontier: ClinePageFrontier,
    observed_items: u64,
    retained_rows: u64,
    prior_prefix_matches: bool,
    attach_session: bool,
    page_transition: ClineComponentTransition,
    pages: usize,
}

impl ClineNativeReader {
    /// Metadata-only construction. No provider component is opened or parsed.
    pub(crate) fn new(
        discovery: ClineDiscovery,
        previous: &[ClineTaskCheckpoint],
        profile: ClineNativeProfile,
    ) -> Self {
        let dialect = discovery.dialect();
        Self {
            discovery,
            dialect,
            profile,
            previous_by_path: previous
                .iter()
                .cloned()
                .map(|checkpoint| (checkpoint.canonical_task_path.clone(), checkpoint))
                .collect(),
            route_index: 0,
            pending_page: None,
            active_task: None,
            active_array: None,
            outcomes: Vec::new(),
            live_checkpoints: Vec::new(),
            stats: ClinePublicationStats::default(),
            catalog_finished: false,
            #[cfg(test)]
            before_exposure: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_exposure_hook(
        &mut self,
        hook: impl FnMut(&Path, ClineComponent) + 'static,
    ) {
        self.before_exposure = Some(Box::new(hook));
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ClinePublicationStats {
        self.stats
    }

    #[cfg(test)]
    pub(crate) fn outcomes(&self) -> &[ClineComponentReadOutcome] {
        &self.outcomes
    }

    /// Advances at most one bounded native item into one certified page.
    ///
    /// Changed arrays remain pinned open across calls. The reader retains no
    /// source-wide item or page collection.
    pub(crate) fn next_page(&mut self) -> Result<Option<ClineCertifiedPage>, ClineNativePathError> {
        loop {
            if let Some(page) = self.pending_page.take() {
                return Ok(Some(page));
            }
            if self.active_array.is_some() {
                if let Some(page) = self.advance_active_array()? {
                    return Ok(Some(page));
                }
                continue;
            }
            if self.active_task.is_some() {
                if let Some(page) = self.advance_active_task()? {
                    return Ok(Some(page));
                }
                continue;
            }
            if self.route_index == self.discovery.task_routes().len() {
                return Ok(None);
            }
            let route = self.discovery.task_routes()[self.route_index].clone();
            self.route_index = self.route_index.saturating_add(1);
            self.begin_route(route)?;
        }
    }

    /// Reconciles directory inventory and `taskHistory.json` only after all
    /// independently certified file pages have drained.
    pub(crate) fn finish_catalog(
        &mut self,
    ) -> Result<ClineCatalogCompletion, ClineNativePathError> {
        if self.route_index != self.discovery.task_routes().len()
            || self.active_task.is_some()
            || self.active_array.is_some()
            || self.pending_page.is_some()
        {
            return Err(ClineNativePathError::Invariant {
                message: "Cline catalog completion requires a fully drained page reader".to_owned(),
            });
        }
        if self.catalog_finished {
            return Err(ClineNativePathError::Invariant {
                message: "Cline catalog completion may run only once".to_owned(),
            });
        }
        self.catalog_finished = true;
        let inventory_revalidated = self.discovery.root_authority().revalidate_catalog()?;
        if !inventory_revalidated {
            return Err(ClineNativePathError::SourceChanged {
                path: self.discovery.root_authority().tasks_root().to_path_buf(),
            });
        }
        let root_observation = self.discovery.root_index().clone();
        let root_index = match &root_observation.state {
            ClineObservedFileState::Missing => ClineCatalogIndex::Missing,
            ClineObservedFileState::Unavailable(message) => {
                ClineCatalogIndex::Unavailable(ClineCatalogRejection {
                    path: root_observation.path.clone(),
                    retryable: true,
                    message: message.clone(),
                })
            }
            ClineObservedFileState::Present(_) => {
                match hydrate_component(&root_observation, &mut self.stats) {
                    Ok(hydrated) => {
                        let parsed =
                            parse_root_index(&hydrated, &root_observation, &mut self.stats);
                        self.run_before_exposure(&root_observation.path, ClineComponent::RootIndex);
                        match root_observation.post_parse_revalidate() {
                            Ok(true) => match parsed {
                                Ok(entries) => ClineCatalogIndex::Parsed {
                                    content_sha256: hydrated.content_sha256,
                                    entries: entries.into_boxed_slice(),
                                },
                                Err(failure)
                                    if failure.kind
                                        == ClineComponentFailureKind::IncompleteJson =>
                                {
                                    ClineCatalogIndex::Incomplete(catalog_rejection(failure))
                                }
                                Err(failure) => {
                                    ClineCatalogIndex::Malformed(catalog_rejection(failure))
                                }
                            },
                            Ok(false) | Err(ClineNativePathError::SourceChanged { .. }) => {
                                return Err(ClineNativePathError::SourceChanged {
                                    path: root_observation.path.clone(),
                                });
                            }
                            Err(error) if is_component_local_error(&error) => {
                                ClineCatalogIndex::Unavailable(catalog_rejection(
                                    local_authority_failure(&root_observation, &error),
                                ))
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(ClineLocalReadError::Local(failure)) => {
                        ClineCatalogIndex::Unavailable(catalog_rejection(failure))
                    }
                    Err(ClineLocalReadError::Fatal(error)) => return Err(error),
                }
            }
        };
        let live_paths = self
            .live_checkpoints
            .iter()
            .map(|checkpoint| checkpoint.canonical_task_path.clone())
            .collect::<BTreeSet<_>>();
        let mut missing_task_paths = if self.discovery.root_authority().is_complete() {
            self.previous_by_path
                .keys()
                .filter(|path| !live_paths.contains(*path))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        missing_task_paths.sort();
        self.live_checkpoints
            .sort_by(|left, right| left.canonical_task_path.cmp(&right.canonical_task_path));
        Ok(ClineCatalogCompletion {
            inventory_complete: self.discovery.root_authority().is_complete(),
            inventory_revalidated,
            root_index,
            live_checkpoints: self.live_checkpoints.clone().into_boxed_slice(),
            missing_task_paths: missing_task_paths.into_boxed_slice(),
        })
    }

    fn begin_route(&mut self, task: ClineLiveTaskObservation) -> Result<(), ClineNativePathError> {
        let previous = self
            .previous_by_path
            .get(&task.canonical_task_path)
            .cloned();
        let metadata = match self.resolve_metadata(&task, previous.as_ref())? {
            MetadataResolution::Ready(ready) => *ready,
            MetadataResolution::Unsafe(failure) => {
                self.outcomes.push(component_failure_outcome(failure));
                if let Some(previous) = previous {
                    self.live_checkpoints.push(previous);
                }
                return Ok(());
            }
        };
        let identity_changed = previous
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.identity != metadata.checkpoint.session.identity);
        let api_history = previous
            .as_ref()
            .and_then(|checkpoint| checkpoint.api_history.clone());
        let ui_messages = previous
            .as_ref()
            .and_then(|checkpoint| checkpoint.ui_messages.clone());
        let fallback_history = previous
            .as_ref()
            .and_then(|checkpoint| checkpoint.fallback_history.clone());
        let event_components = task
            .event_components()
            .filter_map(event_component)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.active_task = Some(ActiveTask {
            task: Box::new(task),
            metadata: metadata.checkpoint,
            metadata_content_authority: metadata.content_authority,
            identity_changed,
            api_history,
            ui_messages,
            fallback_history,
            event_components,
            next_component: 0,
            needs_session: !metadata.page_emitted,
        });
        Ok(())
    }

    fn advance_active_task(&mut self) -> Result<Option<ClineCertifiedPage>, ClineNativePathError> {
        let mut task = self
            .active_task
            .take()
            .ok_or_else(|| ClineNativePathError::Invariant {
                message: "Cline active task disappeared".to_owned(),
            })?;
        if task.next_component >= task.event_components.len() {
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
                Ok(Some(page))
            }
            ClineObservedFileState::Present(_) => {
                let scanner = match ClineArrayScanner::open(&observation, &mut self.stats) {
                    Ok(scanner) => scanner,
                    Err(ClineLocalReadError::Local(failure)) => {
                        self.outcomes.push(component_failure_outcome(failure));
                        self.active_task = Some(task);
                        return Ok(None);
                    }
                    Err(ClineLocalReadError::Fatal(error)) => return Err(error),
                };
                let source = file_source(
                    task.task.dialect,
                    task.metadata.session.identity.clone(),
                    observation.component,
                    &observation.path,
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

    fn advance_active_array(&mut self) -> Result<Option<ClineCertifiedPage>, ClineNativePathError> {
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
                Ok(Some(page))
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
                    super::normalize::CLINE_NATIVE_PAGE_MAX_UNITS
                        .saturating_sub(usize::from(active.attach_session)),
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
                Ok(Some(page))
            }
        }
    }

    fn certify_array_boundary(
        &mut self,
        active: &ActiveArray,
    ) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
        let observation = active.task.component(active.component.source_component());
        self.run_before_exposure(&observation.path, observation.component);
        if !match active.scanner.descriptor_matches_observation() {
            Ok(matches) => matches,
            Err(ClineLocalReadError::Local(failure)) => return Ok(Some(failure)),
            Err(ClineLocalReadError::Fatal(error)) => return Err(error),
        } {
            return Ok(Some(source_changed(observation)));
        }
        let metadata_failure = {
            let task =
                self.active_task
                    .as_mut()
                    .ok_or_else(|| ClineNativePathError::Invariant {
                        message: "active Cline array lacks its task metadata authority".to_owned(),
                    })?;
            Self::certify_metadata_boundary(&active.metadata, &mut task.metadata_content_authority)?
        };
        let component_failure = component_authority_failure(observation, true)?;
        let directory_failure = directory_authority_failure(&active.task, observation)?;
        Ok(component_failure.or(metadata_failure).or(directory_failure))
    }

    fn certify_metadata_boundary(
        metadata: &ClineMetadataCheckpoint,
        content_authority: &mut Option<ClinePinnedContentAuthority>,
    ) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
        // Metadata is source authority even when its parsed taskId was absent
        // or invalid and the directory identity was selected.
        let metadata_failure = component_authority_failure(&metadata.observation, true)?;
        let metadata_content_failure = if metadata_failure.is_none()
            && metadata.observation.stamp().is_some()
        {
            let authority =
                content_authority
                    .as_mut()
                    .ok_or_else(|| ClineNativePathError::Invariant {
                        message:
                            "present Cline metadata lacks pinned content authority at page boundary"
                                .to_owned(),
                    })?;
            match authority.verify_content() {
                Ok(true) => None,
                Ok(false) => Some(source_changed(&metadata.observation)),
                Err(ClineLocalReadError::Local(failure)) => Some(failure),
                Err(ClineLocalReadError::Fatal(error)) => return Err(error),
            }
        } else {
            None
        };
        // Bracket the pinned content check so an atomic metadata replacement
        // during certification cannot leave the old descriptor authoritative
        // for a newly selected path.
        let metadata_post_failure = component_authority_failure(&metadata.observation, true)?;
        Ok(metadata_failure
            .or(metadata_content_failure)
            .or(metadata_post_failure))
    }

    fn finish_array_success(
        &mut self,
        active: &ActiveArray,
        checkpoint: ClineArrayCheckpoint,
        transition: ClineComponentTransition,
    ) {
        if let Some(task) = self.active_task.as_mut() {
            Self::set_task_component(task, active.component, Some(checkpoint));
            task.needs_session = false;
        }
        self.outcomes.push(ClineComponentReadOutcome {
            component: active.component.source_component(),
            path: active.source.canonical_path.clone(),
            transition: Some(transition),
            pages: active.pages,
            failure: None,
        });
    }

    fn finish_array_failure(&mut self, active: &ActiveArray, failure: ClineComponentFailure) {
        self.outcomes.push(ClineComponentReadOutcome {
            component: active.component.source_component(),
            path: active.source.canonical_path.clone(),
            transition: None,
            pages: active.pages,
            failure: Some(failure),
        });
    }

    fn set_task_component(
        task: &mut ActiveTask,
        component: ClineEventComponent,
        checkpoint: Option<ClineArrayCheckpoint>,
    ) {
        match component {
            ClineEventComponent::ApiHistory => task.api_history = checkpoint,
            ClineEventComponent::UiMessages => task.ui_messages = checkpoint,
            ClineEventComponent::FallbackHistory => task.fallback_history = checkpoint,
        }
    }

    fn resolve_metadata(
        &mut self,
        task: &ClineLiveTaskObservation,
        previous: Option<&ClineTaskCheckpoint>,
    ) -> Result<MetadataResolution, ClineNativePathError> {
        let observation = task.metadata_authority();
        let prior = previous.map(|checkpoint| &checkpoint.task_metadata);
        if let Some(prior) = prior.filter(|prior| prior.observation == *observation) {
            if let Some(failure) = component_authority_failure(observation, false)?
                .or(directory_authority_failure(task, observation)?)
            {
                return Ok(MetadataResolution::Unsafe(failure));
            }
            self.outcomes.push(ClineComponentReadOutcome {
                component: observation.component,
                path: observation.path.clone(),
                transition: Some(ClineComponentTransition::Unchanged),
                pages: 0,
                failure: None,
            });
            return Ok(MetadataResolution::Ready(Box::new(MetadataReady {
                checkpoint: prior.clone(),
                page_emitted: false,
                content_authority: match prior.content_sha256 {
                    Some(content_sha256) => {
                        match pin_component_content(observation, content_sha256) {
                            Ok(authority) => Some(authority),
                            Err(ClineLocalReadError::Local(failure)) => {
                                return Ok(MetadataResolution::Unsafe(failure));
                            }
                            Err(ClineLocalReadError::Fatal(error)) => return Err(error),
                        }
                    }
                    None => None,
                },
            })));
        }
        match &observation.state {
            ClineObservedFileState::Unavailable(message) => {
                let failure = ClineComponentFailure {
                    component: observation.component,
                    path: observation.path.clone(),
                    kind: ClineComponentFailureKind::LocalIo,
                    message: message.clone(),
                    retryable: true,
                };
                if previous.is_some_and(|checkpoint| {
                    checkpoint.task_metadata.session.identity_origin
                        == ClineTaskIdentityOrigin::TaskMetadata
                }) {
                    return Ok(MetadataResolution::Unsafe(failure));
                }
                return Ok(MetadataResolution::Ready(Box::new(MetadataReady {
                    checkpoint: fallback_metadata(task, observation.clone()),
                    page_emitted: false,
                    content_authority: None,
                })));
            }
            ClineObservedFileState::Missing => {
                self.run_before_exposure(&observation.path, observation.component);
                if let Some(failure) = component_authority_failure(observation, false)?
                    .or(directory_authority_failure(task, observation)?)
                {
                    return Ok(MetadataResolution::Unsafe(failure));
                }
                let checkpoint = prior.map_or_else(
                    || fallback_metadata(task, observation.clone()),
                    |prior| ClineMetadataCheckpoint {
                        observation: observation.clone(),
                        content_sha256: None,
                        session: prior.session.clone(),
                    },
                );
                let transition = if prior.is_some() {
                    ClineComponentTransition::MissingPhysical
                } else {
                    ClineComponentTransition::Cold
                };
                let page = self.build_metadata_page(checkpoint.clone(), prior, transition)?;
                self.certify_page(page)?;
                self.outcomes.push(ClineComponentReadOutcome {
                    component: observation.component,
                    path: observation.path.clone(),
                    transition: Some(transition),
                    pages: 1,
                    failure: None,
                });
                return Ok(MetadataResolution::Ready(Box::new(MetadataReady {
                    checkpoint,
                    page_emitted: true,
                    content_authority: None,
                })));
            }
            ClineObservedFileState::Present(_) => {}
        }
        let hydrated = match hydrate_component(observation, &mut self.stats) {
            Ok(hydrated) => hydrated,
            Err(ClineLocalReadError::Local(failure)) => {
                return Ok(MetadataResolution::Unsafe(failure));
            }
            Err(ClineLocalReadError::Fatal(error)) => return Err(error),
        };
        let checkpoint = match parse_metadata(
            &hydrated,
            observation,
            &task.directory_task_id,
            &mut self.stats,
        ) {
            Ok(checkpoint) => checkpoint,
            Err(failure) => return Ok(MetadataResolution::Unsafe(failure)),
        };
        let mut content_authority = Some(hydrated.into_pinned_authority(observation));
        self.run_before_exposure(&observation.path, observation.component);
        if let Some(failure) = component_authority_failure(observation, true)?
            .or(directory_authority_failure(task, observation)?)
        {
            return Ok(MetadataResolution::Unsafe(failure));
        }
        if !match content_authority
            .as_mut()
            .expect("present Cline metadata has pinned content authority")
            .verify_content()
        {
            Ok(matches) => matches,
            Err(ClineLocalReadError::Local(failure)) => {
                return Ok(MetadataResolution::Unsafe(failure));
            }
            Err(ClineLocalReadError::Fatal(error)) => return Err(error),
        } {
            return Ok(MetadataResolution::Unsafe(source_changed(observation)));
        }
        let transition = match prior {
            None => ClineComponentTransition::Cold,
            Some(prior) if prior.session.metadata_hash == checkpoint.session.metadata_hash => {
                ClineComponentTransition::ControlOnlyRewrite
            }
            Some(_) => ClineComponentTransition::Rewrite,
        };
        let page = self.build_metadata_page(checkpoint.clone(), prior, transition)?;
        self.certify_page(page)?;
        self.outcomes.push(ClineComponentReadOutcome {
            component: observation.component,
            path: observation.path.clone(),
            transition: Some(transition),
            pages: 1,
            failure: None,
        });
        Ok(MetadataResolution::Ready(Box::new(MetadataReady {
            checkpoint,
            page_emitted: true,
            content_authority,
        })))
    }

    #[allow(clippy::too_many_arguments)]
    fn make_array_page(
        &self,
        source: ClineFileSourceIdentity,
        revision: ClineCertifiedRevision,
        expected: ClinePageFrontier,
        next: ClinePageFrontier,
        transition: ClineComponentTransition,
        item: Option<ParsedItem>,
        terminal_checkpoint: Option<ClineArrayCheckpoint>,
        terminal: bool,
        evidence: Option<ClineTerminalEvidence>,
        session: Option<ClineSessionRow>,
    ) -> Result<ClineCertifiedPage, ClineComponentFailure> {
        let mut events = Vec::new();
        let mut rejections = Vec::new();
        let mut outputs = Vec::new();
        let mut transient_rejections = Vec::new();
        let mut core_bytes =
            estimated_page_envelope_bytes(&source, &revision, &expected, &next, evidence.as_ref());
        let mut potential_output_units = 0_usize;
        let mut fingerprint_items = Vec::new();
        if let Some(item) = item {
            potential_output_units =
                usize::try_from(item.checkpoint.output_outcomes).unwrap_or(usize::MAX);
            core_bytes = core_bytes.saturating_add(item.core_bytes);
            fingerprint_items.push(item.checkpoint);
            events = item.rows;
            outputs = item.outputs;
            transient_rejections = item.transient_rejections;
            if let Some(rejection) = item.rejection {
                core_bytes = core_bytes.saturating_add(estimated_rejection_bytes(&rejection));
                rejections.push(rejection);
            }
        }
        if let Some(session) = session.as_ref() {
            core_bytes = core_bytes.saturating_add(estimated_session_bytes(session));
        }
        if let Some(checkpoint) = terminal_checkpoint.as_ref() {
            core_bytes = core_bytes.saturating_add(checkpoint.estimated_bytes());
        }
        let core_units = events
            .len()
            .saturating_add(rejections.len())
            .saturating_add(usize::from(session.is_some()));
        let logical_units = core_units.saturating_add(potential_output_units);
        let mut transient_bytes = usize::from(self.profile.wants_outputs()) * 16;
        transient_bytes = outputs
            .iter()
            .map(estimated_output_bytes)
            .chain(transient_rejections.iter().map(estimated_rejection_bytes))
            .fold(transient_bytes, usize::saturating_add);
        while transient_bytes > CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES
            || core_bytes.saturating_add(transient_bytes) > CLINE_NATIVE_PAGE_MAX_BYTES
        {
            if let Some(output) = outputs.pop() {
                transient_bytes = transient_bytes.saturating_sub(estimated_output_bytes(&output));
                if let Some(rejection) = output_pressure_rejection(source.component, &output) {
                    let rejection_bytes = estimated_rejection_bytes(&rejection);
                    if transient_bytes.saturating_add(rejection_bytes)
                        <= CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES
                        && core_bytes
                            .saturating_add(transient_bytes)
                            .saturating_add(rejection_bytes)
                            <= CLINE_NATIVE_PAGE_MAX_BYTES
                        && transient_rejections.len() < CLINE_NATIVE_MAX_REJECTIONS
                    {
                        transient_bytes = transient_bytes.saturating_add(rejection_bytes);
                        transient_rejections.push(rejection);
                    }
                }
                continue;
            }
            let Some(rejection) = transient_rejections.pop() else {
                break;
            };
            transient_bytes = transient_bytes.saturating_sub(estimated_rejection_bytes(&rejection));
        }
        let total = core_bytes.saturating_add(transient_bytes);
        if !owned_page_bounds_are_valid(core_bytes, transient_bytes, logical_units) {
            return Err(ClineComponentFailure {
                component: source.component,
                path: source.canonical_path.clone(),
                kind: ClineComponentFailureKind::AuthorityBound,
                message: "Cline certified page exceeds the 64-unit/4 MiB Core/8 MiB total contract"
                    .into(),
                retryable: false,
            });
        }
        let fingerprint = core_payload_fingerprint(
            source.component,
            transition,
            session.as_ref(),
            &fingerprint_items,
            &rejections,
            terminal,
        );
        let identity = page_identity(&source, &revision, &expected, &next, terminal, &fingerprint);
        Ok(ClineCertifiedPage {
            identity,
            source,
            source_revision: revision,
            expected_frontier: expected,
            next_safe_frontier: next,
            terminal,
            #[cfg(test)]
            terminal_evidence: evidence,
            accounting: ClinePageAccounting {
                core_units,
                potential_output_units,
                logical_units,
                conservative_core_bytes: core_bytes,
                transient_output_bytes: transient_bytes,
                conservative_serialized_bytes: total,
            },
            core: ClineCorePayload {
                #[cfg(test)]
                transition,
                session,
                events: events.into_boxed_slice(),
                rejections: rejections.into_boxed_slice(),
                terminal_metadata_checkpoint: None,
            },
            transient: self
                .profile
                .wants_outputs()
                .then_some(ClineTransientOutputPayload {
                    observations: outputs,
                    rejected_outputs: transient_rejections.into_boxed_slice(),
                }),
        })
    }

    fn build_metadata_page(
        &self,
        checkpoint: ClineMetadataCheckpoint,
        prior: Option<&ClineMetadataCheckpoint>,
        transition: ClineComponentTransition,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let source = file_source(
            self.dialect,
            checkpoint.session.identity.clone(),
            checkpoint.observation.component,
            &checkpoint.observation.path,
        );
        let revision_hash = checkpoint
            .content_sha256
            .unwrap_or_else(|| missing_revision(checkpoint.observation.component));
        let revision = certified_revision(&checkpoint.observation, revision_hash);
        let expected = prior.map_or_else(
            || ClinePageFrontier::zero_component(checkpoint.observation.component),
            metadata_frontier,
        );
        let next = metadata_frontier(&checkpoint);
        let evidence = if checkpoint.content_sha256.is_some() {
            ClineTerminalEvidence::CompleteMetadata {
                content_sha256: checkpoint.content_sha256,
            }
        } else {
            ClineTerminalEvidence::Deleted
        };
        let fingerprint = core_payload_fingerprint(
            checkpoint.observation.component,
            transition,
            Some(&checkpoint.session),
            &[],
            &[],
            true,
        );
        let identity = page_identity(&source, &revision, &expected, &next, true, &fingerprint);
        let core_bytes =
            estimated_page_envelope_bytes(&source, &revision, &expected, &next, Some(&evidence))
                .saturating_add(estimated_metadata_checkpoint_bytes(&checkpoint))
                .saturating_add(estimated_session_bytes(&checkpoint.session));
        if !owned_page_bounds_are_valid(core_bytes, 0, 1) {
            return Err(ClineNativePathError::Invariant {
                message: "Cline metadata page exceeded the 4 MiB Core/8 MiB total page bounds"
                    .to_owned(),
            });
        }
        Ok(ClineCertifiedPage {
            identity,
            source,
            source_revision: revision,
            expected_frontier: expected,
            next_safe_frontier: next,
            terminal: true,
            #[cfg(test)]
            terminal_evidence: Some(evidence),
            accounting: ClinePageAccounting {
                core_units: 1,
                potential_output_units: 0,
                logical_units: 1,
                conservative_core_bytes: core_bytes,
                transient_output_bytes: 0,
                conservative_serialized_bytes: core_bytes,
            },
            core: ClineCorePayload {
                #[cfg(test)]
                transition,
                session: Some(checkpoint.session.clone()),
                events: Box::new([]),
                rejections: Box::new([]),
                terminal_metadata_checkpoint: Some(Box::new(checkpoint)),
            },
            transient: self
                .profile
                .wants_outputs()
                .then_some(ClineTransientOutputPayload {
                    observations: Vec::new(),
                    rejected_outputs: Box::new([]),
                }),
        })
    }

    fn build_deleted_array_page(
        &self,
        metadata: &ClineMetadataCheckpoint,
        component: ClineEventComponent,
        observation: &ClineComponentObservation,
        prior: Option<&ClineArrayCheckpoint>,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let source = file_source(
            self.dialect,
            metadata.session.identity.clone(),
            observation.component,
            &observation.path,
        );
        let revision = certified_revision(observation, missing_revision(observation.component));
        let expected = prior.map_or_else(
            || ClinePageFrontier::zero(component),
            |prior| prior.final_frontier.clone(),
        );
        let next = ClinePageFrontier::zero(component);
        let transition = ClineComponentTransition::MissingPhysical;
        let fingerprint =
            core_payload_fingerprint(observation.component, transition, None, &[], &[], true);
        let evidence = ClineTerminalEvidence::Deleted;
        let core_bytes =
            estimated_page_envelope_bytes(&source, &revision, &expected, &next, Some(&evidence));
        if !owned_page_bounds_are_valid(core_bytes, 0, 0) {
            return Err(ClineNativePathError::Invariant {
                message: "Cline deletion page exceeded the 4 MiB Core/8 MiB total page bounds"
                    .to_owned(),
            });
        }
        Ok(ClineCertifiedPage {
            identity: page_identity(&source, &revision, &expected, &next, true, &fingerprint),
            source,
            source_revision: revision,
            expected_frontier: expected,
            next_safe_frontier: next,
            terminal: true,
            #[cfg(test)]
            terminal_evidence: Some(evidence),
            accounting: ClinePageAccounting {
                core_units: 0,
                potential_output_units: 0,
                logical_units: 0,
                conservative_core_bytes: core_bytes,
                transient_output_bytes: 0,
                conservative_serialized_bytes: core_bytes,
            },
            core: ClineCorePayload {
                #[cfg(test)]
                transition,
                session: None,
                events: Box::new([]),
                rejections: Box::new([]),
                terminal_metadata_checkpoint: None,
            },
            transient: self
                .profile
                .wants_outputs()
                .then_some(ClineTransientOutputPayload {
                    observations: Vec::new(),
                    rejected_outputs: Box::new([]),
                }),
        })
    }

    fn certify_page(&mut self, page: ClineCertifiedPage) -> Result<(), ClineNativePathError> {
        if self.pending_page.is_some() {
            return Err(ClineNativePathError::Invariant {
                message: "Cline reader attempted to buffer more than one certified page".to_owned(),
            });
        }
        self.stats.pages_certified = self.stats.pages_certified.saturating_add(1);
        self.pending_page = Some(page);
        self.stats.max_pages_buffered = self.stats.max_pages_buffered.max(1);
        Ok(())
    }

    fn run_before_exposure(&mut self, path: &Path, component: ClineComponent) {
        #[cfg(test)]
        if let Some(hook) = self.before_exposure.as_mut() {
            hook(path, component);
        }
        #[cfg(not(test))]
        let _ = (path, component);
    }
}

enum MetadataResolution {
    Ready(Box<MetadataReady>),
    Unsafe(ClineComponentFailure),
}

struct MetadataReady {
    checkpoint: ClineMetadataCheckpoint,
    page_emitted: bool,
    content_authority: Option<ClinePinnedContentAuthority>,
}

fn fallback_metadata(
    task: &ClineLiveTaskObservation,
    observation: ClineComponentObservation,
) -> ClineMetadataCheckpoint {
    ClineMetadataCheckpoint {
        observation,
        content_sha256: None,
        session: ClineSessionRow::new(
            ClineTaskIdentity::new(task.directory_task_id.clone()),
            ClineTaskIdentityOrigin::DirectoryNameDegraded,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    }
}

fn file_source(
    dialect: super::source::TaskJsonNativeDialect,
    identity: ClineTaskIdentity,
    component: ClineComponent,
    path: &Path,
) -> ClineFileSourceIdentity {
    ClineFileSourceIdentity {
        provider: dialect.provider.as_str(),
        task: identity,
        component,
        canonical_path: path.to_path_buf(),
        stable_id: format!(
            "{}:{}:{}",
            dialect.provider.as_str(),
            path.display(),
            component.file_name()
        )
        .into_boxed_str(),
    }
}

fn certified_revision(
    observation: &ClineComponentObservation,
    revision_sha256: [u8; 32],
) -> ClineCertifiedRevision {
    let token = observation
        .stamp()
        .map_or_else(|| "missing".to_owned(), |stamp| stamp.token());
    ClineCertifiedRevision {
        revision_sha256,
        observed_stamp_token: token.into_boxed_str(),
    }
}

fn missing_revision(component: ClineComponent) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-cline-nativepath-missing-component-v1\0");
    hasher.update([component as u8]);
    hasher.finalize().into()
}

fn metadata_frontier(checkpoint: &ClineMetadataCheckpoint) -> ClinePageFrontier {
    ClinePageFrontier::zero_component(checkpoint.observation.component)
        .advance_metadata(&checkpoint.session.metadata_hash)
}

fn classify_transition(
    prior: Option<&ClineArrayCheckpoint>,
    current: &ClineArrayCheckpoint,
    prior_prefix_matches: bool,
) -> ClineComponentTransition {
    let Some(prior) = prior else {
        return if current.observed_items == 0 {
            ClineComponentTransition::LogicalEmpty
        } else {
            ClineComponentTransition::Cold
        };
    };
    if current.observed_items == prior.observed_items
        && current.final_frontier == prior.final_frontier
    {
        return ClineComponentTransition::ControlOnlyRewrite;
    }
    if current.observed_items > prior.observed_items && prior_prefix_matches {
        return ClineComponentTransition::Append {
            prior_items: usize::try_from(prior.observed_items).unwrap_or(usize::MAX),
        };
    }
    // A shorter bounded summary cannot prove that every retained item is an
    // unchanged prefix. Publish it conservatively as a rewrite.
    ClineComponentTransition::Rewrite
}

fn source_changed(observation: &ClineComponentObservation) -> ClineComponentFailure {
    ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind: ClineComponentFailureKind::SourceChanged,
        message: "component changed before its page could be exposed".into(),
        retryable: true,
    }
}

fn deletion_metadata_authority_refusal(
    metadata: &ClineMetadataCheckpoint,
    array: &ClineComponentObservation,
) -> Option<ClineComponentFailure> {
    (metadata.observation.stamp().is_none()
        || metadata.session.identity_origin != ClineTaskIdentityOrigin::TaskMetadata)
        .then(|| ClineComponentFailure {
            component: array.component,
            path: metadata.observation.path.clone(),
            kind: ClineComponentFailureKind::SourceChanged,
            message: "Cline array deletion requires present metadata with a valid certified taskId"
                .into(),
            retryable: true,
        })
}

fn component_failure_outcome(failure: ClineComponentFailure) -> ClineComponentReadOutcome {
    ClineComponentReadOutcome {
        component: failure.component,
        path: failure.path.clone(),
        transition: None,
        pages: 0,
        failure: Some(failure),
    }
}

fn catalog_rejection(failure: ClineComponentFailure) -> ClineCatalogRejection {
    ClineCatalogRejection {
        path: failure.path,
        retryable: failure.retryable,
        message: failure.message,
    }
}

fn component_authority_failure(
    observation: &ClineComponentObservation,
    post_parse: bool,
) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
    let result = if post_parse {
        observation.post_parse_revalidate()
    } else {
        observation.revalidate()
    };
    match result {
        Ok(true) => Ok(None),
        Ok(false) | Err(ClineNativePathError::SourceChanged { .. }) => {
            Ok(Some(source_changed(observation)))
        }
        Err(ClineNativePathError::SourceAccess { .. }) => Ok(Some(source_changed(observation))),
        Err(error) if is_component_local_error(&error) => {
            Ok(Some(local_authority_failure(observation, &error)))
        }
        Err(error) => Err(error),
    }
}

fn directory_authority_failure(
    task: &ClineLiveTaskObservation,
    observation: &ClineComponentObservation,
) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
    match task.revalidate_directory() {
        Ok(true) => Ok(None),
        Ok(false) | Err(ClineNativePathError::SourceChanged { .. }) => {
            Ok(Some(source_changed(observation)))
        }
        Err(ClineNativePathError::SourceAccess { .. }) => Ok(Some(source_changed(observation))),
        Err(error) if is_component_local_error(&error) => {
            Ok(Some(local_authority_failure(observation, &error)))
        }
        Err(error) => Err(error),
    }
}

fn local_authority_failure(
    observation: &ClineComponentObservation,
    error: &ClineNativePathError,
) -> ClineComponentFailure {
    ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind: ClineComponentFailureKind::LocalIo,
        message: error.to_string().into_boxed_str(),
        retryable: true,
    }
}

fn output_pressure_rejection(
    component: ClineComponent,
    output: &crate::ProOutputObservation,
) -> Option<ClineItemRejection> {
    let event_component = event_component(component)?;
    Some(ClineItemRejection {
        component: event_component,
        native_index: output.coordinate.native_sequence,
        native_id: None,
        kind: ClineItemRejectionKind::OversizedTransientOutput,
        observed_bytes: u64::try_from(output.content.len()).unwrap_or(u64::MAX),
        detail: "Cline transient output exceeded the independently bounded page lane".into(),
    })
}

fn event_component(component: ClineComponent) -> Option<ClineEventComponent> {
    match component {
        ClineComponent::ApiHistory => Some(ClineEventComponent::ApiHistory),
        ClineComponent::UiMessages => Some(ClineEventComponent::UiMessages),
        ClineComponent::FallbackHistory => Some(ClineEventComponent::FallbackHistory),
        ClineComponent::TaskMetadata
        | ClineComponent::HistoryItem
        | ClineComponent::TaskIndex
        | ClineComponent::RootIndex => None,
    }
}

fn estimated_page_envelope_bytes(
    source: &ClineFileSourceIdentity,
    revision: &ClineCertifiedRevision,
    expected: &ClinePageFrontier,
    next: &ClinePageFrontier,
    evidence: Option<&ClineTerminalEvidence>,
) -> usize {
    32_usize
        .saturating_add(estimated_source_bytes(source))
        .saturating_add(estimated_revision_bytes(revision))
        .saturating_add(estimated_frontier_bytes(expected))
        .saturating_add(estimated_frontier_bytes(next))
        .saturating_add(1)
        .saturating_add(estimated_terminal_evidence_bytes(evidence))
        .saturating_add(6 * 8)
        .saturating_add(1)
        .saturating_add(estimated_transition_bytes())
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(8)
        .saturating_add(1)
        .saturating_add(1)
        .saturating_add(1)
}

pub(super) fn owned_page_bounds_are_valid(
    core_bytes: usize,
    transient_bytes: usize,
    logical_units: usize,
) -> bool {
    logical_units <= super::normalize::CLINE_NATIVE_PAGE_MAX_UNITS
        && core_bytes <= CLINE_NATIVE_CORE_PAGE_MAX_BYTES
        && core_bytes.saturating_add(transient_bytes) <= CLINE_NATIVE_PAGE_MAX_BYTES
}

fn estimated_transition_bytes() -> usize {
    // The largest legal transition encoding is its tag plus `prior_items`.
    1 + 8
}

fn estimated_terminal_evidence_bytes(evidence: Option<&ClineTerminalEvidence>) -> usize {
    1_usize.saturating_add(evidence.map_or(0, |evidence| match evidence {
        ClineTerminalEvidence::CompleteArray { .. } => 1 + 8 + 8 + 32,
        ClineTerminalEvidence::CompleteMetadata { content_sha256 } => {
            1 + 1 + usize::from(content_sha256.is_some()) * 32
        }
        ClineTerminalEvidence::Deleted => 1,
        ClineTerminalEvidence::ControlOnly { .. } => 1 + 32,
    }))
}

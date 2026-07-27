use super::*;

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
            component_outcomes: self.outcomes.clone().into_boxed_slice(),
            live_checkpoints: self.live_checkpoints.clone().into_boxed_slice(),
            missing_task_paths: missing_task_paths.into_boxed_slice(),
        })
    }

    pub(super) fn begin_route(
        &mut self,
        task: ClineLiveTaskObservation,
    ) -> Result<(), ClineNativePathError> {
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
        let needs_session = metadata.page.is_none();
        self.active_task = Some(ActiveTask {
            task: Box::new(task),
            metadata: metadata.checkpoint,
            metadata_content_authority: metadata.content_authority,
            deferred_metadata_page: metadata.page,
            discard_deferred_metadata_on_failure: previous.is_none(),
            component_failed: false,
            component_page_certified: false,
            identity_changed,
            api_history,
            ui_messages,
            fallback_history,
            event_components,
            next_component: 0,
            needs_session,
        });
        Ok(())
    }
}

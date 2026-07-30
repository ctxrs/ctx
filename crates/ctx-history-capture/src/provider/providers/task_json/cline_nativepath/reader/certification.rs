use super::*;

impl ClineNativeReader {
    pub(super) fn expose_component_page_after_metadata(
        &mut self,
        page: ClineCertifiedPage,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let deferred_metadata = self.active_task.as_mut().and_then(|task| {
            task.component_page_certified = true;
            if page.core.events.is_empty() {
                None
            } else {
                task.deferred_metadata_page.take()
            }
        });
        let Some(metadata) = deferred_metadata else {
            return Ok(page);
        };
        if self.pending_page.is_some() {
            return Err(ClineNativePathError::Invariant {
                message: "Cline reader attempted to buffer more than one certified page".to_owned(),
            });
        }
        self.pending_page = Some(page);
        self.stats.pages_certified = self.stats.pages_certified.saturating_add(1);
        self.stats.max_pages_buffered = self.stats.max_pages_buffered.max(1);
        Ok(metadata)
    }

    pub(super) fn certify_array_boundary(
        &mut self,
        active: &ActiveArray,
    ) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
        let observation = active.task.component(active.component.source_component());
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

    pub(super) fn certify_metadata_boundary(
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

    pub(super) fn finish_array_success(
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

    pub(super) fn finish_array_failure(
        &mut self,
        active: &ActiveArray,
        failure: ClineComponentFailure,
    ) {
        if let Some(task) = self.active_task.as_mut() {
            task.component_failed = true;
        }
        self.outcomes.push(ClineComponentReadOutcome {
            component: active.component.source_component(),
            path: active.source.canonical_path.clone(),
            transition: None,
            pages: active.pages,
            failure: Some(failure),
        });
    }

    pub(super) fn set_task_component(
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

    pub(super) fn resolve_metadata(
        &mut self,
        task: &ClineLiveTaskObservation,
    ) -> Result<MetadataResolution, ClineNativePathError> {
        let observation = task.metadata_authority();
        match &observation.state {
            ClineObservedFileState::Unavailable(_) => {
                let checkpoint = fallback_metadata(task, observation.clone());
                return Ok(MetadataResolution::Ready(Box::new(MetadataReady {
                    checkpoint,
                    page: None,
                    content_authority: None,
                })));
            }
            ClineObservedFileState::Missing => {
                if let Some(failure) = component_authority_failure(observation, false)?
                    .or(directory_authority_failure(task, observation)?)
                {
                    return Ok(MetadataResolution::Unsafe(failure));
                }
                let checkpoint = fallback_metadata(task, observation.clone());
                let page = self.build_metadata_page(checkpoint.clone())?;
                self.outcomes.push(ClineComponentReadOutcome {
                    component: observation.component,
                    path: observation.path.clone(),
                    transition: Some(ClineComponentTransition::Cold),
                    pages: 1,
                    failure: None,
                });
                return Ok(MetadataResolution::Ready(Box::new(MetadataReady {
                    checkpoint,
                    page: Some(page),
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
        let page = self.build_metadata_page(checkpoint.clone())?;
        self.outcomes.push(ClineComponentReadOutcome {
            component: observation.component,
            path: observation.path.clone(),
            transition: Some(ClineComponentTransition::Cold),
            pages: 1,
            failure: None,
        });
        Ok(MetadataResolution::Ready(Box::new(MetadataReady {
            checkpoint,
            page: Some(page),
            content_authority,
        })))
    }
}

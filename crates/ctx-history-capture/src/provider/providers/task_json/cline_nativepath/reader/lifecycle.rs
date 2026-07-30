use super::*;

impl ClineNativeReader {
    /// Cold-only construction. No provider component is opened or parsed.
    pub(crate) fn new(discovery: ClineDiscovery) -> Self {
        Self {
            discovery,
            route_index: 0,
            pending_page: None,
            active_task: None,
            active_array: None,
            outcomes: Vec::new(),
            live_checkpoints: Vec::new(),
            stats: ClinePublicationStats::default(),
        }
    }

    /// Advances at most one bounded native item into one certified page.
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

    pub(crate) fn finish_task(mut self) -> Result<ClineColdCompletion, ClineNativePathError> {
        if self.route_index != self.discovery.task_routes().len()
            || self.active_task.is_some()
            || self.active_array.is_some()
            || self.pending_page.is_some()
        {
            return Err(ClineNativePathError::Invariant {
                message: "Cline task completion requires a fully drained page reader".to_owned(),
            });
        }
        self.live_checkpoints
            .sort_by(|left, right| left.canonical_task_path.cmp(&right.canonical_task_path));
        Ok(ClineColdCompletion {
            component_outcomes: self.outcomes.into_boxed_slice(),
            live_checkpoints: self.live_checkpoints.into_boxed_slice(),
        })
    }

    pub(super) fn begin_route(
        &mut self,
        task: ClineLiveTaskObservation,
    ) -> Result<(), ClineNativePathError> {
        let metadata = match self.resolve_metadata(&task)? {
            MetadataResolution::Ready(ready) => *ready,
            MetadataResolution::Unsafe(failure) => {
                self.outcomes.push(component_failure_outcome(failure));
                return Ok(());
            }
        };
        let event_components = task
            .event_components()
            .filter_map(|component| match component {
                ClineComponent::ApiHistory => Some(ClineEventComponent::ApiHistory),
                ClineComponent::UiMessages => Some(ClineEventComponent::UiMessages),
                ClineComponent::FallbackHistory => Some(ClineEventComponent::FallbackHistory),
                ClineComponent::TaskMetadata
                | ClineComponent::HistoryItem
                | ClineComponent::TaskIndex => None,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let needs_session = metadata.page.is_none();
        self.active_task = Some(ActiveTask {
            task: Box::new(task),
            metadata: metadata.checkpoint,
            metadata_content_authority: metadata.content_authority,
            deferred_metadata_page: metadata.page,
            component_failed: false,
            component_page_certified: false,
            api_history: None,
            ui_messages: None,
            fallback_history: None,
            event_components,
            next_component: 0,
            needs_session,
        });
        Ok(())
    }
}

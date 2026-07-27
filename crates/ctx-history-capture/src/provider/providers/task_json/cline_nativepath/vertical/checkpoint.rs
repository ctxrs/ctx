use super::*;

pub(super) fn component_cursor_stream(
    dialect: TaskJsonNativeDialect,
    path: &Path,
) -> std::result::Result<String, ClineNativeVerticalError> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.component_cursor_stream_format,
        &identity,
    ))
}

pub(super) fn load_cline_task_checkpoint(
    store: &Store,
    machine_id: &str,
    task: &ClineLiveTaskObservation,
) -> Result<Option<ClineTaskCheckpoint>> {
    let stream =
        task_cursor_stream(task.dialect, &task.canonical_task_path).map_err(map_vertical_error)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let wire: ClineTaskCheckpointWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    wire.into_checkpoint(Some(task))
        .map(Some)
        .map_err(map_vertical_error)
}

pub(super) fn load_cline_task_checkpoint_by_path(
    store: &Store,
    machine_id: &str,
    task_path: &Path,
    dialect: TaskJsonNativeDialect,
) -> std::result::Result<Option<ClineTaskCheckpoint>, ClineNativeVerticalError> {
    let stream = task_cursor_stream(dialect, task_path)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    let wire: ClineTaskCheckpointWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    wire.into_checkpoint(None).map(Some)
}

pub(super) fn publish_task_json_task_checkpoint(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    checkpoint: &ClineTaskCheckpoint,
) -> Result<ProviderImportSummary> {
    publish_task_json_task_checkpoint_inner(store, bulk_guard, options, dialect, checkpoint)
        .map_err(map_vertical_error)
}

pub(super) fn publish_task_json_task_checkpoint_inner(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    checkpoint: &ClineTaskCheckpoint,
) -> std::result::Result<ProviderImportSummary, ClineNativeVerticalError> {
    let stream = task_cursor_stream(dialect, &checkpoint.canonical_task_path)?;
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    let wire = ClineTaskCheckpointWire::from_checkpoint(checkpoint);
    let encoded = serde_json::to_string(&wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Some(stored) = &stored {
        let committed = decode_native_path_committed_cursor(&stored.cursor)
            .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
        if committed.provider_cursor() == encoded {
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = task_checkpoint_publication_id(dialect, checkpoint, &transition);
    let retained_bytes = serde_json::to_vec(&wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
        .len();
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn task_cursor_stream(
    dialect: TaskJsonNativeDialect,
    path: &Path,
) -> std::result::Result<String, ClineNativeVerticalError> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.task_cursor_stream_format,
        &identity,
    ))
}

pub(super) fn task_checkpoint_publication_id(
    dialect: TaskJsonNativeDialect,
    checkpoint: &ClineTaskCheckpoint,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.task_publication_domain);
    digest.update(
        checkpoint
            .canonical_task_path
            .as_os_str()
            .as_encoded_bytes(),
    );
    digest.update(checkpoint.identity.as_str().as_bytes());
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "{}{}",
        dialect.task_publication_prefix,
        hex(&digest.finalize())
    )
}

impl ClineTaskCheckpointWire {
    fn from_checkpoint(checkpoint: &ClineTaskCheckpoint) -> Self {
        Self {
            version: CLINE_TASK_CURSOR_VERSION,
            canonical_task_path: checkpoint.canonical_task_path.clone(),
            api_history: checkpoint
                .api_history
                .as_ref()
                .map(ClineArrayCheckpointWire::from_checkpoint),
            ui_messages: checkpoint
                .ui_messages
                .as_ref()
                .map(ClineArrayCheckpointWire::from_checkpoint),
            fallback_history: checkpoint
                .fallback_history
                .as_ref()
                .map(ClineArrayCheckpointWire::from_checkpoint),
            task_metadata: ClineMetadataCheckpointWire::from_checkpoint(&checkpoint.task_metadata),
        }
    }

    fn into_checkpoint(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineTaskCheckpoint, ClineNativeVerticalError> {
        if self.version != CLINE_TASK_CURSOR_VERSION
            || live.is_some_and(|live| self.canonical_task_path != live.canonical_task_path)
        {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        let task_metadata = self.task_metadata.into_checkpoint(live)?;
        Ok(ClineTaskCheckpoint {
            identity: task_metadata.session.identity.clone(),
            canonical_task_path: self.canonical_task_path,
            api_history: self
                .api_history
                .map(|wire| wire.into_checkpoint(live))
                .transpose()?,
            ui_messages: self
                .ui_messages
                .map(|wire| wire.into_checkpoint(live))
                .transpose()?,
            fallback_history: self
                .fallback_history
                .map(|wire| wire.into_checkpoint(live))
                .transpose()?,
            task_metadata,
        })
    }
}

impl ClineArrayCheckpointWire {
    fn from_checkpoint(checkpoint: &ClineArrayCheckpoint) -> Self {
        Self {
            component: checkpoint.component as u8,
            observation: ClinePersistedObservation::from_observation(&checkpoint.observation),
            certified_revision_sha256: checkpoint.certified_revision_sha256,
            complete_bytes: checkpoint.complete_bytes,
            observed_items: checkpoint.observed_items,
            retained_rows: checkpoint.retained_rows,
            final_frontier: checkpoint.final_frontier.clone(),
        }
    }

    fn into_checkpoint(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineArrayCheckpoint, ClineNativeVerticalError> {
        let component = event_component(self.component)?;
        if self.observation.component != component.source_component() as u8 {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        Ok(ClineArrayCheckpoint {
            component,
            observation: self.observation.into_observation(live)?,
            certified_revision_sha256: self.certified_revision_sha256,
            complete_bytes: self.complete_bytes,
            observed_items: self.observed_items,
            retained_rows: self.retained_rows,
            final_frontier: self.final_frontier,
        })
    }
}

impl ClineMetadataCheckpointWire {
    fn from_checkpoint(checkpoint: &ClineMetadataCheckpoint) -> Self {
        Self {
            observation: ClinePersistedObservation::from_observation(&checkpoint.observation),
            content_sha256: checkpoint.content_sha256,
            session: ClineSessionRowWire::from_session(&checkpoint.session),
        }
    }

    fn into_checkpoint(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineMetadataCheckpoint, ClineNativeVerticalError> {
        Ok(ClineMetadataCheckpoint {
            observation: self.observation.into_observation(live)?,
            content_sha256: self.content_sha256,
            session: self.session.into_session()?,
        })
    }
}

impl ClinePersistedObservation {
    fn from_observation(observation: &ClineComponentObservation) -> Self {
        Self {
            component: observation.component as u8,
            path: observation.path.clone(),
            stamp_token: observation.stamp().map(super::super::ClineFileStamp::token),
            missing: observation.is_missing(),
        }
    }

    fn into_observation(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineComponentObservation, ClineNativeVerticalError> {
        let component = component(self.component)?;
        if let Some(live) = live {
            let current = live.component(component);
            if current.path != self.path {
                return Err(ClineNativeVerticalError::CorruptCursor);
            }
            let current_token = current.stamp().map(super::super::ClineFileStamp::token);
            if (self.missing && current.is_missing())
                || (!self.missing
                    && self.stamp_token.is_some()
                    && self.stamp_token == current_token)
            {
                return Ok(current.clone());
            }
        }
        if self.missing {
            return Ok(ClineComponentObservation {
                component,
                path: self.path,
                state: ClineObservedFileState::Missing,
            });
        }
        Ok(ClineComponentObservation {
            component,
            path: self.path,
            state: ClineObservedFileState::Unavailable(
                "persisted prior Cline component observation".into(),
            ),
        })
    }
}

impl ClineSessionRowWire {
    fn from_session(session: &ClineSessionRow) -> Self {
        Self {
            identity: session.identity.as_str().to_owned(),
            identity_origin: match session.identity_origin {
                ClineTaskIdentityOrigin::TaskMetadata => 0,
                ClineTaskIdentityOrigin::DirectoryNameDegraded => 1,
            },
            identity_aliases: session
                .identity_aliases
                .iter()
                .map(|alias| alias.as_str().to_owned())
                .collect(),
            title: session.title.as_deref().map(str::to_owned),
            workspace_directory: session.workspace_directory.as_deref().map(str::to_owned),
            created_at: session.created_at.as_deref().map(str::to_owned),
            last_modified: session.last_modified.as_deref().map(str::to_owned),
            model_id: session.model_id.as_deref().map(str::to_owned),
            model_provider: session.model_provider.as_deref().map(str::to_owned),
            tokens_input: session.tokens_input,
            tokens_output: session.tokens_output,
        }
    }

    fn into_session(self) -> std::result::Result<ClineSessionRow, ClineNativeVerticalError> {
        let identity_origin = match self.identity_origin {
            0 => ClineTaskIdentityOrigin::TaskMetadata,
            1 => ClineTaskIdentityOrigin::DirectoryNameDegraded,
            _ => return Err(ClineNativeVerticalError::CorruptCursor),
        };
        let mut session = ClineSessionRow::new(
            ClineTaskIdentity::new(self.identity),
            identity_origin,
            self.title.map(String::into_boxed_str),
            self.workspace_directory.map(String::into_boxed_str),
            self.created_at.map(String::into_boxed_str),
            self.last_modified.map(String::into_boxed_str),
            self.model_id.map(String::into_boxed_str),
            self.model_provider.map(String::into_boxed_str),
            self.tokens_input,
            self.tokens_output,
        );
        session.identity_aliases = self
            .identity_aliases
            .into_iter()
            .map(ClineTaskIdentity::new)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(session)
    }
}

pub(super) fn component(
    value: u8,
) -> std::result::Result<ClineComponent, ClineNativeVerticalError> {
    match value {
        value if value == ClineComponent::ApiHistory as u8 => Ok(ClineComponent::ApiHistory),
        value if value == ClineComponent::UiMessages as u8 => Ok(ClineComponent::UiMessages),
        value if value == ClineComponent::TaskMetadata as u8 => Ok(ClineComponent::TaskMetadata),
        value if value == ClineComponent::RootIndex as u8 => Ok(ClineComponent::RootIndex),
        value if value == ClineComponent::FallbackHistory as u8 => {
            Ok(ClineComponent::FallbackHistory)
        }
        value if value == ClineComponent::HistoryItem as u8 => Ok(ClineComponent::HistoryItem),
        value if value == ClineComponent::TaskIndex as u8 => Ok(ClineComponent::TaskIndex),
        _ => Err(ClineNativeVerticalError::CorruptCursor),
    }
}

pub(super) fn event_component(
    value: u8,
) -> std::result::Result<ClineEventComponent, ClineNativeVerticalError> {
    match value {
        value if value == ClineEventComponent::ApiHistory as u8 => {
            Ok(ClineEventComponent::ApiHistory)
        }
        value if value == ClineEventComponent::UiMessages as u8 => {
            Ok(ClineEventComponent::UiMessages)
        }
        value if value == ClineEventComponent::FallbackHistory as u8 => {
            Ok(ClineEventComponent::FallbackHistory)
        }
        _ => Err(ClineNativeVerticalError::CorruptCursor),
    }
}

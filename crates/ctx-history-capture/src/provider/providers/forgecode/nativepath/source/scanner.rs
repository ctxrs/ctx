use super::*;

impl ForgeCodeScanner {
    pub(in crate::provider::providers::forgecode) fn new(
        source: ForgeCodeSourceObservation,
        frontier: ForgeCodeFrontier,
        context: ProviderAdapterContext,
        wants_outputs: bool,
    ) -> Result<Self> {
        let source_root = context.source_root_display().or_else(|| {
            source
                .canonical_path
                .parent()
                .map(|path| path.display().to_string())
        });
        Ok(Self {
            source,
            frontier,
            context,
            source_root,
            wants_outputs,
            exhausted: false,
            active_decoded: None,
            decoded_rows: 0,
        })
    }

    #[cfg(test)]
    pub(in crate::provider::providers::forgecode) fn next_page(
        &mut self,
    ) -> Result<Option<ForgeCodePage>> {
        let database = Arc::clone(&self.source.database);
        let path = self.source.canonical_path.clone();
        database.read(&path, |connection| self.next_page_guarded(connection))
    }

    pub(in crate::provider::providers::forgecode) fn stream_pages(
        &mut self,
        mut emit: impl FnMut(ForgeCodePage) -> Result<()>,
    ) -> Result<()> {
        let database = Arc::clone(&self.source.database);
        let path = self.source.canonical_path.clone();
        database.read(&path, |connection| {
            while let Some(page) = self.next_page_guarded(connection)? {
                emit(page)?;
            }
            Ok(())
        })
    }

    pub(in crate::provider::providers::forgecode) fn decoded_rows(&self) -> u64 {
        self.decoded_rows
    }

    pub(in crate::provider::providers::forgecode) fn source_database(
        &self,
    ) -> &ForgeCodeSqliteDatabase {
        &self.source.database
    }

    fn next_page_guarded(&mut self, connection: &Connection) -> Result<Option<ForgeCodePage>> {
        if self.exhausted {
            return Ok(None);
        }
        let expected_frontier = self.frontier.clone();
        if !expected_frontier.row_complete {
            let active = self
                .active_decoded
                .clone()
                .ok_or(CaptureError::SystemInvariant(
                    "ForgeCode partial row lost its decoded row",
                ))?;
            if expected_frontier.rowid != Some(active.rowid) {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let page = self.project_row(connection, expected_frontier, active)?;
            self.frontier = page.next_frontier.clone();
            if page.next_frontier.row_complete {
                self.active_decoded = None;
            }
            if page.terminal {
                self.exhausted = true;
            }
            return Ok(Some(page));
        }
        let candidate = self.next_candidate(connection)?;
        let Some(candidate) = candidate else {
            self.exhausted = true;
            let page = ForgeCodePage {
                expected_frontier: expected_frontier.clone(),
                next_frontier: expected_frontier,
                terminal: true,
                row: None,
                events: Vec::new(),
                outputs: Vec::new(),
                touches: Vec::new(),
                rejections: Vec::new(),
                retained_bytes: 512,
                retained_output_bytes: 0,
            };
            return Ok(Some(page));
        };
        let page = self.page_for_candidate(connection, expected_frontier, candidate)?;
        self.frontier = page.next_frontier.clone();
        if page.next_frontier.row_complete {
            self.active_decoded = None;
        }
        if page.terminal {
            self.exhausted = true;
        }
        Ok(Some(page))
    }

    fn next_candidate(&self, connection: &Connection) -> Result<Option<ForgeCodeRowCandidate>> {
        if self.frontier.rowid.is_some() && !self.frontier.row_complete {
            return self.candidate_at(connection, self.frontier.rowid);
        }
        self.candidate_after(connection, self.frontier.rowid)
    }

    fn candidate_at(
        &self,
        connection: &Connection,
        rowid: Option<i64>,
    ) -> Result<Option<ForgeCodeRowCandidate>> {
        let rowid = rowid.ok_or(CaptureError::SystemInvariant(
            "ForgeCode partial frontier has no rowid",
        ))?;
        let sql = self.candidate_sql("where rowid = ?1");
        with_length_preflight(connection, || {
            connection
                .query_row(&sql, [rowid], row_candidate)
                .optional()
        })
    }

    fn candidate_after(
        &self,
        connection: &Connection,
        rowid: Option<i64>,
    ) -> Result<Option<ForgeCodeRowCandidate>> {
        let predicate = rowid.map_or("", |_| "where rowid > ?1");
        let sql = self.candidate_sql(predicate);
        with_length_preflight(connection, || match rowid {
            Some(rowid) => connection
                .query_row(&sql, [rowid], row_candidate)
                .optional(),
            None => connection.query_row(&sql, [], row_candidate).optional(),
        })
    }

    fn candidate_sql(&self, predicate: &str) -> String {
        let title = optional_column_expr(&self.source.columns, "title", "NULL");
        let context = optional_column_expr(&self.source.columns, "context", "NULL");
        let updated_at = optional_column_expr(&self.source.columns, "updated_at", "NULL");
        let metrics = optional_column_expr(&self.source.columns, "metrics", "NULL");
        let retained = retained_length_expr(&[
            "conversation_id",
            title,
            "CASE WHEN typeof(workspace_id) = 'integer' THEN NULL ELSE workspace_id END",
            context,
            "created_at",
            updated_at,
            metrics,
        ]);
        format!(
            "select rowid, {retained}, typeof(conversation_id), typeof({title}), \
             typeof(workspace_id), typeof({context}), typeof(created_at), \
             typeof({updated_at}), typeof({metrics}) from conversations {predicate} \
             order by rowid limit 1"
        )
    }

    fn page_for_candidate(
        &mut self,
        connection: &Connection,
        expected_frontier: ForgeCodeFrontier,
        candidate: ForgeCodeRowCandidate,
    ) -> Result<ForgeCodePage> {
        let row_line = provider_line_from_index(candidate.rowid.max(0) as u64);
        if let Some(reason) = candidate.rejection_reason() {
            return self.rejected_row_page(
                connection,
                expected_frontier,
                candidate.rowid,
                row_line,
                reason.to_owned(),
            );
        }
        if candidate.observed_bytes()? > MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 {
            return self.rejected_row_page(
                connection,
                expected_frontier,
                candidate.rowid,
                row_line,
                format!(
                    "ForgeCode conversation row exceeds the {}-byte hydration limit",
                    MAX_PROVIDER_SQLITE_VALUE_BYTES
                ),
            );
        }
        let hydrated = match self.hydrate(connection, candidate.rowid) {
            Ok(row) => row,
            Err(error) => {
                return self.rejected_row_page(
                    connection,
                    expected_frontier,
                    candidate.rowid,
                    row_line,
                    error.to_string(),
                )
            }
        };
        let decoded = match hydrated.decode() {
            Ok(row) => row,
            Err(error) => {
                return self.rejected_row_page(
                    connection,
                    expected_frontier,
                    candidate.rowid,
                    row_line,
                    error.reason(),
                )
            }
        };
        self.decoded_rows =
            self.decoded_rows
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "ForgeCode decoded-row counter overflowed",
                ))?;
        self.active_decoded = Some(decoded.clone());
        self.project_row(connection, expected_frontier, decoded)
    }

    fn rejected_row_page(
        &mut self,
        connection: &Connection,
        expected_frontier: ForgeCodeFrontier,
        rowid: i64,
        line: usize,
        error: String,
    ) -> Result<ForgeCodePage> {
        self.active_decoded = None;
        let next_frontier = ForgeCodeFrontier {
            rowid: Some(rowid),
            next_message: 0,
            row_complete: true,
        };
        Ok(ForgeCodePage {
            expected_frontier,
            terminal: !self.has_row_after(connection, rowid)?,
            next_frontier,
            row: None,
            events: Vec::new(),
            outputs: Vec::new(),
            touches: Vec::new(),
            rejections: vec![ProviderImportFailure { line, error }],
            retained_bytes: 1024,
            retained_output_bytes: 0,
        })
    }

    fn hydrate(&self, connection: &Connection, rowid: i64) -> Result<ForgeCodeHydratedRow> {
        let title = optional_column_expr(&self.source.columns, "title", "NULL");
        let context = optional_column_expr(&self.source.columns, "context", "NULL");
        let updated_at = optional_column_expr(&self.source.columns, "updated_at", "NULL");
        let metrics = optional_column_expr(&self.source.columns, "metrics", "NULL");
        let sql = format!(
            "select rowid, cast(conversation_id as blob), cast({title} as blob), \
             workspace_id, cast({context} as blob), cast(created_at as blob), \
             cast({updated_at} as blob), cast({metrics} as blob) \
             from conversations where rowid = ?1"
        );
        connection
            .query_row(&sql, [rowid], |row| {
                Ok(ForgeCodeHydratedRow {
                    rowid: row.get(0)?,
                    conversation_id: row.get(1)?,
                    title: row.get(2)?,
                    workspace_id: row.get(3)?,
                    context: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    metrics: row.get(7)?,
                })
            })
            .map_err(CaptureError::from)
    }

    fn project_row(
        &mut self,
        connection: &Connection,
        expected_frontier: ForgeCodeFrontier,
        hydrated: ForgeCodeDecodedRow,
    ) -> Result<ForgeCodePage> {
        let rowid = hydrated.rowid;
        let row_line = provider_line_from_index(rowid.max(0) as u64);
        let conversation_id = hydrated.conversation_id;
        let title = hydrated.title;
        let created_at = hydrated.created_at;
        let updated_at = hydrated.updated_at;
        let context_raw = hydrated.context;
        let metrics_raw = hydrated.metrics;
        let mut rejections = Vec::new();
        let context_value = context_raw
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| match serde_json::from_str::<Value>(raw) {
                Ok(value) => Some(value),
                Err(error) => {
                    rejections.push(ProviderImportFailure {
                        line: row_line,
                        error: format!(
                            "invalid JSON in ForgeCode conversations.context {conversation_id}: {error}"
                        ),
                    });
                    None
                }
            });
        let metrics_value = metrics_raw
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| match serde_json::from_str::<Value>(raw) {
                Ok(value) => Some(value),
                Err(error) => {
                    rejections.push(ProviderImportFailure {
                        line: row_line,
                        error: format!(
                            "invalid JSON in ForgeCode conversations.metrics {conversation_id}: {error}"
                        ),
                    });
                    None
                }
            });
        let messages = context_value
            .as_ref()
            .and_then(|value| value.get("messages"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let start = if expected_frontier.rowid == Some(rowid) && !expected_frontier.row_complete {
            usize::try_from(expected_frontier.next_message).map_err(|_| {
                CaptureError::InvalidPayload(
                    "ForgeCode NativePath message frontier exceeds usize".to_owned(),
                )
            })?
        } else {
            0
        };
        if start > messages.len() {
            return Err(CaptureError::InvalidPayload(
                "ForgeCode NativePath message frontier exceeds the current row".to_owned(),
            ));
        }
        let started_at = forgecode_timestamp(Some(&created_at), self.context.imported_at);
        let ended_at = updated_at
            .as_deref()
            .map(|raw| forgecode_timestamp(Some(raw), started_at));
        let context_metadata = context_value
            .as_ref()
            .map(context_without_messages)
            .unwrap_or(Value::Null);
        let initiator = context_value
            .as_ref()
            .and_then(|value| value.get("initiator"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let complete_content = ForgeCodeCompleteContentDigest::new(
            rowid,
            &conversation_id,
            title.as_deref(),
            hydrated.workspace_id,
            context_raw.as_deref(),
            &created_at,
            updated_at.as_deref(),
            metrics_raw.as_deref(),
        )?;
        let row = ForgeCodeConversationRow {
            rowid,
            source_record_digest: complete_content.record_digest(),
            canonical_record_bytes: complete_content.canonical_record_bytes(),
            conversation_id,
            title,
            workspace_id: hydrated.workspace_id,
            created_at,
            updated_at,
            context_metadata,
            metrics_metadata: metrics_value
                .as_ref()
                .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
            context_message_count: messages.len(),
            initiator,
        };
        let mut events = Vec::new();
        let mut outputs = Vec::new();
        let mut touches = Vec::new();
        let mut retained_core_bytes = 2_048_usize
            .saturating_add(estimated_row_bytes(&row))
            .saturating_add(rejections.iter().fold(0_usize, |bytes, rejection| {
                bytes.saturating_add(estimated_rejection_bytes(rejection))
            }));
        let mut retained_output_bytes = 0_usize;
        if retained_core_bytes > FORGECODE_NATIVE_PAGE_CONTENT_MAX_BYTES {
            return self.rejected_row_page(
                connection,
                expected_frontier,
                rowid,
                row_line,
                format!(
                    "ForgeCode conversation row exceeds the {FORGECODE_NATIVE_PAGE_MAX_BYTES}-byte retained-page limit"
                ),
            );
        }
        let mut next_index = start;
        while next_index < messages.len()
            && next_index.saturating_sub(start) < FORGECODE_NATIVE_MAX_MESSAGES_PER_PAGE
        {
            let entry = &messages[next_index];
            let entry_bytes = serde_json::to_vec(entry)?.len();
            let parts = forgecode_message_parts(entry);
            let event_type = forgecode_event_type(parts);
            let output_outcome =
                (event_type == EventType::ToolOutput).then(|| output_outcome(parts));
            let output_content = (self.wants_outputs && output_outcome.is_some())
                .then(|| forgecode_normalized_result_content(parts.body).map(String::into_bytes))
                .flatten();
            let provider_event_index = u64::try_from(next_index)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let occurred_at =
                started_at + Duration::milliseconds(i64::try_from(next_index).unwrap_or(i64::MAX));
            let retained_failure = output_outcome.as_ref().is_some_and(|outcome| {
                matches!(
                    outcome.outcome,
                    OutputOutcome::Failure | OutputOutcome::Timeout
                )
            });
            let mut message_event = None;
            let mut message_output = None;
            let mut message_touches = Vec::new();
            let mut message_rejections = Vec::new();
            if output_outcome.is_none() || retained_failure {
                if entry_bytes > FORGECODE_NATIVE_MAX_EVENT_BYTES {
                    message_rejections.push(ProviderImportFailure {
                        line: provider_line_from_index(provider_event_index),
                        error: format!(
                            "ForgeCode message {provider_event_index} exceeds the {FORGECODE_NATIVE_MAX_EVENT_BYTES}-byte retained-event limit"
                        ),
                    });
                } else {
                    let mut event = forgecode_event(
                        &row.conversation_id,
                        entry,
                        provider_event_index,
                        occurred_at,
                    );
                    if output_outcome.is_none() {
                        complete_content.attach_message(&mut event, || {
                            forgecode_message_text(parts, event_type)
                        })?;
                    }
                    if let Some(metadata) = event.metadata.as_object_mut() {
                        metadata.insert(
                            "source_record_ordinal".to_owned(),
                            Value::from(ordered_rowid(rowid)),
                        );
                        metadata.insert(
                            "source_record_subrecord_index".to_owned(),
                            Value::from(u32::try_from(next_index).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    "ForgeCode message index exceeds u32".to_owned(),
                                )
                            })?),
                        );
                    }
                    message_event = Some(ForgeCodeRetainedEvent {
                        event,
                        provider_event_index,
                    });
                }
            }
            if self.wants_outputs {
                if let Some(outcome) = output_outcome {
                    let content = output_content.unwrap_or_default();
                    if content.len() > FORGECODE_NATIVE_MAX_OUTPUT_BYTES {
                        message_rejections.push(ProviderImportFailure {
                            line: provider_line_from_index(provider_event_index),
                            error: format!(
                                "ForgeCode output {provider_event_index} exceeds the {FORGECODE_NATIVE_MAX_OUTPUT_BYTES}-byte transient-output limit"
                            ),
                        });
                    } else {
                        message_output = Some(ProOutputObservation {
                            kind: OutputObservationKind::Tool,
                            coordinate: OutputNativeCoordinate {
                                unit_key: format!(
                                    "forgecode:{}:message:{next_index:010}:output",
                                    row.conversation_id
                                ),
                                native_sequence: ordered_rowid(rowid),
                                native_record_id: Some(format!(
                                    "conversation:{}:message:{provider_event_index}",
                                    row.conversation_id
                                )),
                                source_record_ordinal: Some(ordered_rowid(rowid)),
                                source_record_subrecord_index: Some(
                                    u32::try_from(next_index).map_err(|_| {
                                        CaptureError::InvalidPayload(
                                            "ForgeCode message index exceeds u32".to_owned(),
                                        )
                                    })?,
                                ),
                                byte_start: None,
                                byte_end_exclusive: None,
                            },
                            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
                            associations: OutputAssociations {
                                direct_session_id: row.conversation_id.clone(),
                                root_session_id: row.conversation_id.clone(),
                                parent_session_id: None,
                                provider_session_id: Some(row.conversation_id.clone()),
                                agent_id: row.initiator.clone(),
                                repository: None,
                            },
                            call_id: forgecode_tool_result_call_id(parts),
                            command: None,
                            outcome,
                            locator: OutputSourceLocator {
                                version: 1,
                                kind: FORGECODE_NATIVE_LOCATOR_KIND.to_owned(),
                                payload: rowid.to_be_bytes().to_vec(),
                            },
                            content,
                        });
                    }
                }
            }
            let touch_outcome = visit_provider_file_touch_drafts_with_limit(
                entry,
                event_type_supports_structured_file_touches(event_type),
                FORGECODE_NATIVE_MAX_TOUCHES_PER_MESSAGE,
                |(touch_ordinal, touch)| {
                    let provider_touch_index =
                        if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                            touch_ordinal
                        } else {
                            (provider_event_index << 16) | touch_ordinal
                        };
                    message_touches.push(ForgeCodeFileTouch {
                        provider_touch_index,
                        provider_event_index: Some(provider_event_index),
                        raw_source_path: Some(self.source.canonical_path.display().to_string()),
                        source_root: self.source_root.clone(),
                        path: touch.path,
                        change_kind: touch.change_kind,
                        old_path: touch.old_path,
                        line_count_delta: None,
                        confidence: touch.confidence,
                        occurred_at,
                        metadata: touch.metadata,
                    });
                    Ok::<(), CaptureError>(())
                },
            )?;
            if touch_outcome.limit_exceeded() {
                message_rejections.push(ProviderImportFailure {
                    line: provider_line_from_index(provider_event_index),
                    error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                });
            }
            let message_core_bytes = message_event
                .as_ref()
                .map(estimated_retained_event_bytes)
                .unwrap_or_default()
                .saturating_add(message_touches.iter().fold(0_usize, |bytes, touch| {
                    bytes.saturating_add(estimated_touch_bytes(touch))
                }))
                .saturating_add(message_rejections.iter().fold(0_usize, |bytes, rejection| {
                    bytes.saturating_add(estimated_rejection_bytes(rejection))
                }));
            let message_output_bytes = message_output
                .as_ref()
                .map(estimated_output_bytes)
                .unwrap_or_default();
            let next_retained_bytes = retained_core_bytes
                .saturating_add(retained_output_bytes)
                .saturating_add(message_core_bytes)
                .saturating_add(message_output_bytes);
            if next_retained_bytes > FORGECODE_NATIVE_PAGE_CONTENT_MAX_BYTES {
                if next_index > start {
                    break;
                }
                let rejection = ProviderImportFailure {
                    line: provider_line_from_index(provider_event_index),
                    error: format!(
                        "ForgeCode message {provider_event_index} exceeds the {FORGECODE_NATIVE_PAGE_MAX_BYTES}-byte retained-page limit"
                    ),
                };
                retained_core_bytes =
                    retained_core_bytes.saturating_add(estimated_rejection_bytes(&rejection));
                rejections.push(rejection);
                next_index = next_index.saturating_add(1);
                continue;
            }
            if let Some(event) = message_event {
                events.push(event);
            }
            if let Some(output) = message_output {
                outputs.push(output);
            }
            touches.extend(message_touches);
            rejections.extend(message_rejections);
            retained_core_bytes = retained_core_bytes.saturating_add(message_core_bytes);
            retained_output_bytes = retained_output_bytes.saturating_add(message_output_bytes);
            next_index = next_index.saturating_add(1);
        }
        let row_complete = next_index == messages.len();
        if row_complete {
            if let Some(metrics) = metrics_value.as_ref() {
                let mut metric_touches = Vec::new();
                let limit_exceeded = forgecode_for_each_metric_file_touch_with_limit(
                    metrics,
                    &self.source.canonical_path.display().to_string(),
                    ended_at.unwrap_or(started_at),
                    FORGECODE_NATIVE_MAX_METRIC_TOUCHES,
                    |(_, mut touch)| {
                        touch.source_root.clone_from(&self.source_root);
                        metric_touches.push(touch);
                        Ok::<(), CaptureError>(())
                    },
                )?;
                let metric_bytes = metric_touches.iter().fold(0_usize, |bytes, touch| {
                    bytes.saturating_add(estimated_touch_bytes(touch))
                });
                let limit_rejection = limit_exceeded.then(|| ProviderImportFailure {
                    line: row_line,
                    error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                });
                let metric_total_bytes = metric_bytes.saturating_add(
                    limit_rejection
                        .as_ref()
                        .map(estimated_rejection_bytes)
                        .unwrap_or_default(),
                );
                if retained_core_bytes
                    .saturating_add(retained_output_bytes)
                    .saturating_add(metric_total_bytes)
                    > FORGECODE_NATIVE_PAGE_MAX_BYTES
                {
                    let rejection = ProviderImportFailure {
                        line: row_line,
                        error: format!(
                            "ForgeCode metrics exceed the {FORGECODE_NATIVE_PAGE_MAX_BYTES}-byte retained-page limit"
                        ),
                    };
                    retained_core_bytes =
                        retained_core_bytes.saturating_add(estimated_rejection_bytes(&rejection));
                    rejections.push(rejection);
                } else {
                    retained_core_bytes = retained_core_bytes.saturating_add(metric_total_bytes);
                    touches.extend(metric_touches);
                    rejections.extend(limit_rejection);
                }
            }
        }
        let retained_bytes = retained_core_bytes.saturating_add(retained_output_bytes);
        if retained_bytes > FORGECODE_NATIVE_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "ForgeCode NativePath page exceeds its retained byte bound".to_owned(),
            ));
        }
        let next_frontier = ForgeCodeFrontier {
            rowid: Some(rowid),
            next_message: u32::try_from(next_index).map_err(|_| {
                CaptureError::InvalidPayload("ForgeCode message index exceeds u32".to_owned())
            })?,
            row_complete,
        };
        Ok(ForgeCodePage {
            expected_frontier,
            terminal: row_complete && !self.has_row_after(connection, rowid)?,
            next_frontier,
            row: Some(row),
            events,
            outputs,
            touches,
            rejections,
            retained_bytes,
            retained_output_bytes,
        })
    }

    fn has_row_after(&self, connection: &Connection, rowid: i64) -> Result<bool> {
        connection
            .query_row(
                "select exists(select 1 from conversations where rowid > ?1)",
                [rowid],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(CaptureError::from)
    }
}

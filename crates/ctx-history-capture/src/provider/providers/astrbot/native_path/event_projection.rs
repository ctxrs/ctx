use super::*;

pub(super) fn normalized_event(
    committed_store: &Store,
    options: &ProviderImportOptions,
    session_fact: &SessionFact,
    source_id: Uuid,
    session: &Session,
    fact: &EventFact,
) -> Result<Event> {
    let event_hash = crate::compute_payload_hash(&fact.payload)?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::AstrBot,
        &session_fact.provider_session_id,
        source_id,
        fact.provider_event_index,
        fact.provider_event_index,
        &event_hash,
        None,
        fact.legacy_provider_event_index,
        true,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let mut payload = fact.payload.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "provider".to_owned(),
            Value::String(CaptureProvider::AstrBot.as_str().to_owned()),
        );
        object.insert(
            "provider_session_id".to_owned(),
            Value::String(session_fact.provider_session_id.clone()),
        );
        object.insert(
            "provider_event_index".to_owned(),
            json!(fact.provider_event_index),
        );
        object.insert(
            "provider_event_hash".to_owned(),
            Value::String(event_hash.clone()),
        );
        object.insert("cursor".to_owned(), Value::String(fact.cursor.clone()));
    }
    let mut provider_metadata = fact.metadata.clone();
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": session_fact.provider_session_id,
        "provider_event_index": fact.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "cursor": fact.cursor,
        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "source_record_ordinal": fact.source_record_ordinal,
        "source_record_subrecord_index": fact.source_record_subrecord_index,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: fact.event_type,
        role: fact.role,
        occurred_at: fact.occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

pub(super) fn reconcile_astrbot_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    session: &Session,
    fact: &EventFact,
    mut normalized: Event,
) -> Result<bool> {
    if matches!(
        fact.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        let Some(exact_legacy_hash) = exact_released_output_hash(committed_store, session, fact)?
        else {
            return group
                .reconcile_provider_event(
                    &normalized,
                    ProviderEventHashAuthority::NormalizedPayloadFallback,
                )
                .map_err(CaptureError::from);
        };
        if normalized
            .payload
            .get("result_outcome")
            .and_then(Value::as_str)
            != Some("failure")
        {
            normalized.event_type = EventType::Message;
            normalized.sync.deleted_at = Some(context.imported_at);
            normalized.sync.metadata["retired_by"] =
                Value::String("astrbot_v025_output_scrub".to_owned());
        }
        return group
            .reconcile_provider_event_migrating_exact_legacy_provider_hash(
                &normalized,
                &exact_legacy_hash,
            )
            .map_err(CaptureError::from);
    }

    group
        .reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &normalized,
            &fact.legacy_provider_event_hash,
        )
        .map_err(CaptureError::from)
}

pub(super) fn exact_released_output_hash(
    store: &Store,
    session: &Session,
    fact: &EventFact,
) -> Result<Option<String>> {
    let Some(expected_payload_hash) = fact.released_v025_payload_hash.as_deref() else {
        return Ok(None);
    };
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    for event in store.events_for_session(session.id)? {
        let metadata = &event.sync.metadata;
        let hash = metadata.get("provider_event_hash").and_then(Value::as_str);
        let exact_payload = event
            .payload
            .get("body")
            .map(crate::compute_payload_hash)
            .transpose()?
            .as_deref()
            == Some(expected_payload_hash);
        if event.event_type == EventType::Message
            && event.sync.deleted_at.is_none()
            && metadata.get("provider_event_hash_authority").is_none()
            && metadata.get("provider_session_id").and_then(Value::as_str)
                == Some(provider_session_id)
            && metadata.get("provider_event_index").and_then(Value::as_u64)
                == Some(fact.provider_event_index)
            && metadata.get("source_format").and_then(Value::as_str)
                == Some(ASTRBOT_SQLITE_SOURCE_FORMAT)
            && metadata.pointer("/metadata/source").and_then(Value::as_str)
                == Some("astrbot_conversations")
            && hash == Some(fact.legacy_provider_event_hash.as_str())
            && event
                .payload
                .get("provider_event_hash")
                .and_then(Value::as_str)
                == hash
            && exact_payload
        {
            return Ok(hash.map(str::to_owned));
        }
    }
    Ok(None)
}

impl<'a> AstrBotReader<'a> {
    pub(super) fn new(conn: &'a Connection, sql: AstrBotSql, frontier: AstrBotFrontier) -> Self {
        Self {
            conn,
            sql,
            frontier,
            active_conversation: None,
            relationship_projection_ready: false,
        }
    }

    pub(super) fn next_page(&mut self, collect_outputs: bool) -> Result<Option<AstrBotPage>> {
        if self.frontier.terminal() {
            return Ok(None);
        }
        let expected_frontier = self.frontier.clone();
        let mut units = Vec::new();
        let mut outputs = Vec::new();
        let mut rejections = Vec::new();
        let mut source_units = 0_usize;
        let mut core_bytes = 1024_usize;
        let mut output_bytes = 0_usize;

        loop {
            if source_units >= PAGE_MAX_SOURCE_UNITS {
                break;
            }
            match self.frontier.phase {
                ScanPhase::Conversations => {
                    if self.active_conversation.is_none() {
                        self.active_conversation = self.load_active_conversation()?;
                    }
                    let Some(active) = self.active_conversation.as_mut() else {
                        self.frontier.phase = ScanPhase::PlatformMessages;
                        if source_units != 0 {
                            break;
                        }
                        continue;
                    };
                    let item_count = active.items.len().max(1);
                    let item_index = active.next_item_index;
                    let session = conversation_session_fact(&active.row);
                    let item = active.items.get(item_index);
                    let (event, output, rejection) = conversation_event(
                        &active.row,
                        active.physical_rowid,
                        item_index,
                        item,
                        active.content_is_array,
                        self.frontier.next_native_ordinal,
                        collect_outputs,
                    )?;
                    let rejection = rejection.or_else(|| {
                        if item_index == 0 {
                            active.rejection.take()
                        } else {
                            None
                        }
                    });
                    let include_session = item_index == 0;
                    let mut unit =
                        (include_session || event.is_some()).then_some(CoreUnit { session, event });
                    let mut unit_bytes = unit.as_ref().map_or(64, estimated_unit_bytes);
                    let unit_exceeds_page_bound = unit_bytes > PAGE_MAX_CORE_BYTES;
                    if unit_exceeds_page_bound {
                        unit = None;
                        unit_bytes = 64;
                    }
                    let output_estimate =
                        output.as_ref().map_or(0, |output| output.estimated_bytes);
                    if source_units != 0
                        && (core_bytes.saturating_add(unit_bytes) > PAGE_MAX_CORE_BYTES
                            || output_bytes.saturating_add(output_estimate) > PAGE_MAX_OUTPUT_BYTES)
                    {
                        break;
                    }
                    source_units = source_units.saturating_add(1);
                    core_bytes = core_bytes
                        .saturating_add(unit_bytes)
                        .min(PAGE_MAX_CORE_BYTES);
                    if let Some(unit) = unit {
                        units.push(unit);
                    }
                    if let Some(output) = output {
                        if output_estimate <= PAGE_MAX_OUTPUT_BYTES {
                            output_bytes = output_bytes.saturating_add(output_estimate);
                            outputs.push(output);
                        } else {
                            rejections.push(PageRejection {
                                line: ordinal_line(self.frontier.next_native_ordinal),
                                detail: "AstrBot output exceeds the bounded Pro replay page"
                                    .to_owned(),
                            });
                        }
                    }
                    if let Some(detail) = rejection {
                        rejections.push(PageRejection {
                            line: ordinal_line(self.frontier.next_native_ordinal),
                            detail,
                        });
                    }
                    if unit_exceeds_page_bound {
                        rejections.push(PageRejection {
                            line: ordinal_line(self.frontier.next_native_ordinal),
                            detail: "AstrBot conversation record exceeds the bounded Core publication page"
                                .to_owned(),
                        });
                    }
                    self.frontier.next_native_ordinal =
                        self.frontier.next_native_ordinal.saturating_add(1);
                    active.next_item_index = active.next_item_index.saturating_add(1);
                    if active.next_item_index >= item_count {
                        finish_conversation_row(&mut self.frontier, active);
                        self.active_conversation = None;
                    } else {
                        self.frontier.conversation_in_row = Some(ConversationInRow {
                            physical_rowid: active.physical_rowid,
                            row_sha256: active.row_sha256,
                            next_item_index: u32::try_from(active.next_item_index)
                                .unwrap_or(u32::MAX),
                        });
                    }
                }
                ScanPhase::PlatformMessages => {
                    if !self.relationship_projection_ready {
                        prepare_relationship_projection(self.conn, &self.sql)?;
                        self.relationship_projection_ready = true;
                    }
                    let Some(initial) = self.sql.platform_message_candidate_initial.as_deref()
                    else {
                        self.frontier.phase = ScanPhase::Complete;
                        break;
                    };
                    let after = self.sql.platform_message_candidate_after.as_deref().ok_or(
                        CaptureError::SystemInvariant(
                            "AstrBot platform-message keyset SQL is incomplete",
                        ),
                    )?;
                    let Some(candidate) = fetch_candidate(
                        self.conn,
                        initial,
                        after,
                        self.frontier.platform_after_rowid,
                    )?
                    else {
                        self.frontier.phase = ScanPhase::Complete;
                        break;
                    };
                    let (mut unit, rejection, row_sha256) =
                        self.platform_unit(candidate, self.frontier.next_native_ordinal)?;
                    let mut unit_bytes = unit.as_ref().map_or(64, estimated_unit_bytes);
                    let unit_exceeds_page_bound = unit_bytes > PAGE_MAX_CORE_BYTES;
                    if unit_exceeds_page_bound {
                        unit = None;
                        unit_bytes = 64;
                    }
                    if source_units != 0
                        && core_bytes.saturating_add(unit_bytes) > PAGE_MAX_CORE_BYTES
                    {
                        break;
                    }
                    source_units = source_units.saturating_add(1);
                    core_bytes = core_bytes
                        .saturating_add(unit_bytes)
                        .min(PAGE_MAX_CORE_BYTES);
                    if let Some(unit) = unit {
                        units.push(unit);
                    }
                    if let Some(detail) = rejection {
                        rejections.push(PageRejection {
                            line: ordinal_line(self.frontier.next_native_ordinal),
                            detail,
                        });
                    }
                    if unit_exceeds_page_bound {
                        rejections.push(PageRejection {
                            line: ordinal_line(self.frontier.next_native_ordinal),
                            detail: "AstrBot platform-message record exceeds the bounded Core publication page"
                                .to_owned(),
                        });
                    }
                    self.frontier.platform_after_rowid = Some(candidate.physical_rowid);
                    self.frontier.platform_prefix_sha256 =
                        chain_hash(self.frontier.platform_prefix_sha256, row_sha256);
                    self.frontier.last_platform_order = Some(candidate.legacy_order);
                    self.frontier.next_native_ordinal =
                        self.frontier.next_native_ordinal.saturating_add(1);
                }
                ScanPhase::Complete => break,
            }
        }

        Ok(Some(AstrBotPage {
            expected_frontier,
            next_frontier: self.frontier.clone(),
            terminal: self.frontier.terminal(),
            retained_core_bytes: core_bytes,
            units,
            outputs,
            rejections,
        }))
    }

    pub(super) fn load_active_conversation(&self) -> Result<Option<ActiveConversation>> {
        if let Some(in_row) = &self.frontier.conversation_in_row {
            let row = hydrate_conversation(
                self.conn,
                &self.sql.conversation_hydration,
                in_row.physical_rowid,
            )?;
            let row_sha256 = serialized_hash(b"astrbot-conversation-row-v1\0", &row)?;
            if row_sha256 != in_row.row_sha256 {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let (items, content_is_array) = conversation_items(&row.content);
            let next_item_index = usize::try_from(in_row.next_item_index).map_err(|_| {
                CaptureError::InvalidPayload(
                    "AstrBot conversation item frontier exceeds platform limits".to_owned(),
                )
            })?;
            if next_item_index >= items.len().max(1) {
                return Err(CaptureError::InvalidPayload(
                    "AstrBot conversation item frontier is out of range".to_owned(),
                ));
            }
            return Ok(Some(ActiveConversation {
                physical_rowid: in_row.physical_rowid,
                order: LegacyOrderKey {
                    timestamp_is_present: row.created_at.is_some(),
                    timestamp: row.created_at.unwrap_or(0),
                    logical_id: row.row_id,
                    physical_rowid: in_row.physical_rowid,
                },
                row_sha256,
                row,
                items,
                content_is_array,
                next_item_index,
                rejection: None,
            }));
        }
        let Some(candidate) = fetch_candidate(
            self.conn,
            &self.sql.conversation_candidate_initial,
            &self.sql.conversation_candidate_after,
            self.frontier.conversation_after_rowid,
        )?
        else {
            return Ok(None);
        };
        let observed = candidate.observed_bytes()?;
        if observed > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX) {
            return Ok(Some(rejected_conversation(
                candidate,
                "AstrBot conversation row exceeds the provider record limit",
            )));
        }
        let row = hydrate_conversation(
            self.conn,
            &self.sql.conversation_hydration,
            candidate.physical_rowid,
        )?;
        let row_sha256 = serialized_hash(b"astrbot-conversation-row-v1\0", &row)?;
        let (items, content_is_array) = conversation_items(&row.content);
        Ok(Some(ActiveConversation {
            physical_rowid: candidate.physical_rowid,
            order: candidate.legacy_order,
            row_sha256,
            row,
            items,
            content_is_array,
            next_item_index: 0,
            rejection: None,
        }))
    }

    pub(super) fn platform_unit(
        &self,
        candidate: RowCandidate,
        native_ordinal: u64,
    ) -> Result<(Option<CoreUnit>, Option<String>, [u8; 32])> {
        if candidate.observed_bytes()?
            > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
        {
            return Ok((
                None,
                Some("AstrBot platform-message row exceeds the provider record limit".to_owned()),
                candidate_hash(b"astrbot-platform-oversize-v1\0", candidate),
            ));
        }
        let hydration =
            self.sql
                .platform_message_hydration
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "AstrBot platform-message hydration SQL is missing",
                ))?;
        let row = hydrate_platform_message(self.conn, hydration, candidate.physical_rowid)?;
        let row_sha256 = serialized_hash(b"astrbot-platform-row-v1\0", &row)?;
        let link = linked_platform_message_parent(self.conn, row.llm_checkpoint_id.as_deref())?;
        let Some(text) = row
            .content
            .as_deref()
            .map(provider_json_text)
            .as_ref()
            .and_then(provider_value_text)
            .filter(|text| !text.trim().is_empty())
        else {
            return Ok((None, None, row_sha256));
        };
        let session = platform_session_fact(&row, link.as_ref());
        let role = if row.sender_id.as_deref() == row.user_id.as_deref() {
            Some(EventRole::User)
        } else {
            Some(EventRole::Assistant)
        };
        let event_index = 1_000_000u64.saturating_add(u64::try_from(row.id).unwrap_or(0));
        let event_type = EventType::Message;
        let occurred_at = timestamp(row.created_at, session.started_at);
        let body = json!({
            "message_id": row.id,
            "platform_id": row.platform_id,
            "user_id": row.user_id,
            "sender_id": row.sender_id,
            "sender_name": row.sender_name,
            "content": row.content.as_deref().map(provider_json_text),
            "llm_checkpoint_id": row.llm_checkpoint_id,
        });
        Ok((
            Some(CoreUnit {
                session,
                event: Some(EventFact {
                    provider_event_index: event_index,
                    legacy_provider_event_index: Some(event_index),
                    legacy_provider_event_hash: format!("platform-message:{}", row.id),
                    released_v025_payload_hash: None,
                    cursor: format!("platform_message_history:id:{}", row.id),
                    source_record_ordinal: native_ordinal,
                    source_record_subrecord_index: 0,
                    event_type,
                    role,
                    occurred_at,
                    payload: astrbot_event_payload(event_type, &text, &body),
                    metadata: json!({
                        "source": "astrbot_platform_message_history",
                        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "message_id": row.id,
                    }),
                }),
            }),
            None,
            row_sha256,
        ))
    }
}

pub(super) fn conversation_event(
    row: &ConversationRow,
    physical_rowid: i64,
    item_index: usize,
    item: Option<&Value>,
    content_is_array: bool,
    native_ordinal: u64,
    collect_output: bool,
) -> Result<(Option<EventFact>, Option<OutputFact>, Option<String>)> {
    let Some(item) = item else {
        return Ok((None, None, None));
    };
    if checkpoint_id(item).is_some() {
        return Ok((None, None, None));
    }
    let text = if content_is_array {
        item_text(item)
    } else {
        provider_value_text(item)
    };
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return Ok((None, None, None));
    };
    let provider_session_id = provider_session_id(row);
    let output = item_is_output(item);
    let outcome = output.then(|| output_outcome(item));
    let event_type = if output {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    let event_index = u64::try_from(item_index).unwrap_or(u64::MAX);
    let released_payload = released_v025_message_payload(&text, item);
    let released_payload_hash = crate::compute_payload_hash(&released_payload)?;
    let legacy_provider_event_hash = if content_is_array {
        item_id(item)
            .map(|id| format!("conversation:{id}"))
            .unwrap_or_else(|| released_payload_hash.clone())
    } else {
        format!("conversation-row:{}", row.row_id)
    };
    let cursor = if content_is_array {
        format!("conversation:{}:item:{item_index}", row.conversation_id)
    } else {
        format!("conversation:{}:content", row.conversation_id)
    };
    let body = item.clone();
    let mut event = EventFact {
        provider_event_index: event_index,
        legacy_provider_event_index: Some(event_index),
        legacy_provider_event_hash: legacy_provider_event_hash.clone(),
        released_v025_payload_hash: output.then_some(released_payload_hash),
        cursor: cursor.clone(),
        source_record_ordinal: native_ordinal,
        source_record_subrecord_index: u32::try_from(item_index).unwrap_or(u32::MAX),
        event_type,
        role: item_role(item),
        occurred_at: timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH),
        payload: astrbot_event_payload(event_type, &text, &body),
        metadata: json!({
            "source": "astrbot_conversations",
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": row.conversation_id,
            "inner_conversation_id": row.inner_conversation_id,
            "item_index": item_index,
        }),
    };
    if !output {
        let locator = super::super::astrbot_complete_message_locator(physical_rowid, item_index)?;
        attach_astrbot_complete_content_locator(
            &mut event,
            &locator,
            &super::super::model::conversation_values(row.clone()),
            &text,
            &legacy_provider_event_hash,
        )?;
    }
    let output_fact = if output && collect_output {
        let locator = super::super::astrbot_complete_message_locator(physical_rowid, item_index)?;
        let content = text.into_bytes();
        let estimated_bytes = content
            .len()
            .saturating_add(provider_session_id.len())
            .saturating_add(1024);
        Some(OutputFact {
            observation: ProOutputObservation {
                kind: OutputObservationKind::Tool,
                coordinate: OutputNativeCoordinate {
                    unit_key: format!("astrbot/{}/{item_index:010}", row.conversation_id),
                    native_sequence: native_ordinal,
                    native_record_id: item_id(item).map(str::to_owned),
                    source_record_ordinal: Some(native_ordinal),
                    source_record_subrecord_index: u32::try_from(item_index).ok(),
                    byte_start: None,
                    byte_end_exclusive: None,
                },
                occurred_at_unix_ms: row.created_at,
                associations: OutputAssociations {
                    direct_session_id: provider_session_id.clone(),
                    root_session_id: provider_session_id.clone(),
                    parent_session_id: None,
                    provider_session_id: Some(provider_session_id),
                    agent_id: row.persona_id.clone(),
                    repository: None,
                },
                call_id: item
                    .get("call_id")
                    .or_else(|| item.get("tool_call_id"))
                    .or_else(|| item.get("toolCallId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                command: None,
                outcome: OutputOutcomeMetadata {
                    outcome: outcome.unwrap_or(OutputOutcome::Unknown),
                    exit_code: item
                        .get("exit_code")
                        .or_else(|| item.get("exitCode"))
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    duration_ms: item
                        .get("duration_ms")
                        .or_else(|| item.get("durationMs"))
                        .and_then(Value::as_u64),
                },
                locator: OutputSourceLocator {
                    version: 1,
                    kind: locator.kind().to_owned(),
                    payload: locator.value().to_vec(),
                },
                content,
            },
            estimated_bytes,
        })
    } else {
        None
    };
    Ok((Some(event), output_fact, None))
}

pub(in super::super) fn released_v025_message_payload(text: &str, body: &Value) -> Value {
    let (text, truncated) = provider_local_preview(text, PROVIDER_MAX_TEXT_CHARS);
    let mut retained_body = released_v025_message_body(body, None);
    if let Some(object) = retained_body.as_object_mut() {
        object.insert(
            "content_retention".to_owned(),
            Value::String("full_text".to_owned()),
        );
    }
    json!({
        "text": text,
        "truncated": truncated,
        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
        "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        "content_retention": "full_text",
    })
}

pub(super) fn released_v025_message_body(value: &Value, key: Option<&str>) -> Value {
    if key.is_some_and(|key| released_v025_omits_message_field(key, value)) {
        return json!({
            "content_retention": "metadata_only",
            "omitted_bytes": released_v025_value_bytes(value),
            "contains_patch_or_diff": released_v025_contains_patch_or_diff(value),
        });
    }
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| released_v025_message_body(item, key))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), released_v025_message_body(value, Some(key))))
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub(super) fn released_v025_omits_message_field(key: &str, value: &Value) -> bool {
    let key = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        key.as_str(),
        "output"
            | "stdout"
            | "stderr"
            | "tooloutput"
            | "toolresult"
            | "toolresults"
            | "tooluseresult"
            | "toolcallstates"
            | "commandoutput"
            | "executionoutput"
            | "result"
            | "results"
            | "diff"
            | "patch"
            | "oldstring"
            | "newstring"
            | "oldcontent"
            | "newcontent"
            | "beforecontent"
            | "aftercontent"
            | "beforetext"
            | "aftertext"
    ) || (matches!(key.as_str(), "input" | "arguments" | "args" | "params")
        && released_v025_contains_patch_or_diff(value))
}

pub(super) fn released_v025_value_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        _ => serde_json::to_string(value)
            .map(|text| text.len())
            .unwrap_or_default(),
    }
}

pub(super) fn released_v025_contains_patch_or_diff(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            text.contains("*** Begin Patch")
                || text.contains("diff --git ")
                || text.starts_with("@@")
                || text.starts_with("+++ ")
                || text.starts_with("--- ")
                || text.contains("\n@@")
                || text.contains("\n+++ ")
                || text.contains("\n--- ")
        }
        Value::Array(items) => items.iter().any(released_v025_contains_patch_or_diff),
        Value::Object(object) => object.values().any(released_v025_contains_patch_or_diff),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

pub(super) fn astrbot_event_payload(event_type: EventType, text: &str, body: &Value) -> Value {
    let retained_text = provider_policy_event_text(event_type, text, body);
    let retained_body = provider_policy_body(event_type, body);
    json!({
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": provider_result_identifier_evidence(event_type, text, body),
        "result_outcome": provider_result_outcome_evidence(event_type, body),
        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
        "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
    })
}

pub(super) fn attach_astrbot_complete_content_locator(
    event: &mut EventFact,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
    native_record_id: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("AstrBot complete content exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "AstrBot complete-content profile is not registered",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id,
        astrbot_logical_record_digest(values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "AstrBot complete-content locator exceeds its typed bounds",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("AstrBot complete-content locator metadata is malformed"),
    )?;
    Ok(())
}

pub(super) fn astrbot_logical_record_digest(
    values: &[NativeSqliteValue],
) -> Result<CompleteContentBodyDigest> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-complete-content-sqlite-logical-row-v1\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("AstrBot logical-row digest formatting failed"),
    )
}

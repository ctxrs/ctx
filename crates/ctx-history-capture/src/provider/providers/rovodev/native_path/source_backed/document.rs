use super::*;

#[derive(Debug)]
struct ProjectedMessage {
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    touched_files: Vec<String>,
    touch_limit_exceeded: bool,
}

fn project_message(
    message: &serde_json::Value,
    _index: usize,
    document: &PreparedDocument,
) -> std::result::Result<Option<ProjectedMessage>, String> {
    if !message.is_object() {
        return Err(bounded_failure(
            "Rovo Dev message_history member must be an object",
        ));
    }
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(serde_json::Value::as_str);
    let mut event_type = rovodev_event_type(message, role_text);
    if event_type == EventType::ToolOutput {
        let outcome = output_outcome(message);
        if !matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
            return Ok(None);
        }
        if output_kind(message) == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
    }
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    let role = Some(provider_role_from_message(message, role_text));
    let mut touched_files = Vec::new();
    let include_structured = event_type_supports_structured_file_touches(event_type);
    let outcome = visit_provider_file_touch_drafts_with_limit(
        message,
        include_structured,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| {
            touched_files.push(touch.path);
            Ok::<(), CaptureError>(())
        },
    )
    .map_err(|error| bounded_failure(error.to_string()))?;
    Ok(Some(ProjectedMessage {
        event_type,
        role,
        occurred_at,
        touched_files,
        touch_limit_exceeded: outcome.limit_exceeded(),
    }))
}

fn output_kind(value: &serde_json::Value) -> OutputObservationKind {
    let tool_name = recursive_string_field(value, &["tool_name", "toolName", "name", "tool"])
        .unwrap_or_else(|| "tool".to_owned());
    if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    }
}

fn output_outcome(value: &serde_json::Value) -> OutputOutcome {
    if value_timed_out(value) {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    }
}

fn recursive_string_field(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| recursive_string_field(value, fields)),
        serde_json::Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(serde_json::Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| recursive_string_field(value, fields))
            }),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => None,
    }
}

fn value_timed_out(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(value_timed_out),
        serde_json::Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

pub(super) fn scan_rovodev_document(
    authority: &RovoDevTreeAuthority,
    leaf: &RovoDevDocumentLeaf,
    context: &ProviderAdapterContext,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> SourceBackedRouteResult<DocumentSourceTerminal> {
    let snapshot = open_leaf(authority, leaf, context).map_err(rovodev_route_error)?;
    let observation = snapshot
        .observation(leaf.source_key.clone())
        .map_err(rovodev_route_error)?;
    sink.begin_source(leaf.source_key.clone())?;

    let mut counts = ScannedSourceCounts::default();
    match snapshot.document.as_ref() {
        Err(_) => {
            counts.rejected_records = 1;
        }
        Ok(document) => {
            counts.rejected_records = document.initial_failure_count;
            for (index, raw_message) in document.messages.iter().enumerate() {
                let serialized_bytes = serde_json::to_vec(raw_message)
                    .map_err(|error| {
                        rovodev_route_error(RovoDevSourceBackedError::Capture(error.into()))
                    })?
                    .len();
                if serialized_bytes > SOURCE_BACKED_MAX_RECORD_BYTES {
                    counts.rejected_records =
                        checked_add(counts.rejected_records, 1).map_err(rovodev_route_error)?;
                    continue;
                }
                match project_message(raw_message, index, document) {
                    Err(_) => {
                        counts.rejected_records =
                            checked_add(counts.rejected_records, 1).map_err(rovodev_route_error)?;
                    }
                    Ok(None) => {
                        counts.ignored_records =
                            checked_add(counts.ignored_records, 1).map_err(rovodev_route_error)?;
                    }
                    Ok(Some(event)) => {
                        if event.touch_limit_exceeded {
                            counts.rejected_records = checked_add(counts.rejected_records, 1)
                                .map_err(rovodev_route_error)?;
                        }
                        sink.emit_document(
                            lexical_document(leaf, &snapshot, document, raw_message, index, event)
                                .map_err(rovodev_route_error)?,
                        )?;
                        counts.retained_records =
                            checked_add(counts.retained_records, 1).map_err(rovodev_route_error)?;
                        counts.indexed_documents = checked_add(counts.indexed_documents, 1)
                            .map_err(rovodev_route_error)?;
                    }
                }
            }
        }
    }
    counts.complete_records = counts
        .retained_records
        .checked_add(counts.rejected_records)
        .and_then(|count| count.checked_add(counts.ignored_records))
        .ok_or_else(|| rovodev_route_error(RovoDevSourceBackedError::CountMismatch))?;
    counts.certified_bytes = snapshot.certified_bytes;
    snapshot.revalidate_files().map_err(rovodev_route_error)?;
    authority
        .authority
        .revalidate()
        .map_err(|error| rovodev_route_error(error.into()))?;
    Ok(DocumentSourceTerminal {
        source: leaf.source_key.clone(),
        opening: observation.clone(),
        closing: observation,
        parser_revision: PARSER_REVISION,
        content_digest: snapshot.source_sha256,
        counts,
    })
}

fn open_leaf(
    authority: &RovoDevTreeAuthority,
    leaf: &RovoDevDocumentLeaf,
    context: &ProviderAdapterContext,
) -> RovoDevSourceBackedResult<RovoDevSnapshot> {
    let snapshot = RovoDevSnapshot::read(
        &leaf.source,
        context,
        &authority.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    if !snapshot.matches(&leaf.proof) {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    let (provider_session_id, parent_provider_session_id) = snapshot
        .document
        .as_ref()
        .map(|document| {
            (
                document.provider_session_id.as_str(),
                document.parent_provider_session_id.as_deref(),
            )
        })
        .unwrap_or((leaf.source.provider_session_id.as_str(), None));
    if provider_session_id != leaf.provider_session_id
        || parent_provider_session_id != leaf.parent_provider_session_id.as_deref()
        || unique_message_ids(&snapshot) != leaf.unique_message_ids
    {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    Ok(snapshot)
}

fn checked_add(left: u64, right: u64) -> RovoDevSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(RovoDevSourceBackedError::CountMismatch)
}

fn lexical_document(
    leaf: &RovoDevDocumentLeaf,
    snapshot: &RovoDevSnapshot,
    document: &PreparedDocument,
    raw_message: &serde_json::Value,
    index: usize,
    event: ProjectedMessage,
) -> RovoDevSourceBackedResult<LexicalDocument> {
    let native_item_key = native_item_key(leaf, raw_message, index)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &leaf.source_key,
        session_id: leaf.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let message_index =
        u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
    let native_record_id = provider_message_id(raw_message, message_index);
    let locator = SourceRecordLocator::new(
        leaf.source_key.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_CONTEXT_FILE)?,
            record_coordinate: TypedKey::composite(vec![
                TypedKey::utf8(MESSAGE_OBJECT_KIND)?,
                TypedKey::U64(message_index),
                TypedKey::utf8(&native_record_id)?,
            ])?,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(snapshot.source_sha256),
        snapshot.context_sha256,
    )?;
    let body = lexical_body(raw_message, event.event_type);
    Ok(LexicalDocument {
        event_id,
        session_id: leaf.session_id,
        parent_session_id: leaf.parent_session_id,
        root_session_id: leaf.root_session_id,
        source: leaf.source_key.clone(),
        locator,
        provider_session_id: Some(document.provider_session_id.clone()),
        branch: provider_string_field(
            &document.metadata,
            &[
                "branch",
                "git_branch",
                "gitBranch",
                "vcs_branch",
                "vcsBranch",
            ],
        )
        .or_else(|| document.context_branch.clone()),
        source_path: Some(leaf.source.context_path.display().to_string()),
        agent_type: if document.parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        }
        .as_str()
        .to_owned(),
        is_primary: document.parent_provider_session_id.is_none(),
        event_sequence: message_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: document.cwd.clone(),
        cwd: document.cwd.clone(),
        touched_files: event.touched_files,
    })
}

fn native_item_key(
    leaf: &RovoDevDocumentLeaf,
    message: &serde_json::Value,
    index: usize,
) -> RovoDevSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = explicit_message_id(message)
        .filter(|native_id| leaf.unique_message_ids.contains(*native_id))
    {
        return Ok(NativeItemKey::native_id(
            EVENT_KEY_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    let coordinate = TypedKey::composite(vec![
        explicit_message_id(message)
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(
            u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        ),
    ])?;
    Ok(NativeItemKey::revision_scoped_position(
        EVENT_POSITION_KIND,
        coordinate,
        TypedKey::bytes(leaf.proof.source_sha256.to_vec())?,
    )?)
}

fn lexical_body(raw_message: &serde_json::Value, event_type: EventType) -> String {
    let text = provider_block_text(raw_message).unwrap_or_default();
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text
    }
}

pub(super) fn hydrate_rovodev_group(
    root: &Path,
    context: &ProviderAdapterContext,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    let expected_source = request
        .events()
        .first()
        .map(|event| event.locator().source().clone())
        .ok_or_else(|| invalid_locator("Rovo Dev hydration group is empty"))?;
    if request.events().iter().any(|event| {
        event.locator().validate_contract().is_err()
            || !event
                .locator()
                .source()
                .exact_descriptor_eq(&expected_source)
    }) {
        return Err(invalid_locator(
            "Rovo Dev hydration group has invalid or mixed-source locators",
        ));
    }

    let tree = match discover_rovodev_source_backed(root, context.clone())
        .map_err(temporarily_unavailable)?
    {
        RovoDevSourceBackedDisposition::Complete(tree) => tree,
        RovoDevSourceBackedDisposition::Unavailable => {
            return Err(temporarily_unavailable(
                "Rovo Dev selected sessions root is temporarily unavailable",
            ));
        }
    };
    let mut matches = tree.leaves.iter().filter(|leaf| {
        leaf.provider_leaf
            .source_key
            .exact_descriptor_eq(&expected_source)
    });
    let leaf = matches
        .next()
        .ok_or_else(|| missing_record("the exact Rovo Dev source is absent"))?;
    if matches.next().is_some() {
        return Err(stale_evidence(
            "more than one Rovo Dev leaf owns the exact source",
        ));
    }
    let snapshot =
        open_leaf(&tree.authority, &leaf.provider_leaf, context).map_err(stale_evidence)?;
    let mut records = Vec::with_capacity(request.events().len());
    for event in request.events() {
        let provider_bytes =
            hydrate_from_snapshot(&leaf.provider_leaf, &snapshot, event.locator())?;
        records.push(HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes,
        });
    }
    snapshot.revalidate_files().map_err(stale_evidence)?;
    tree.authority
        .authority
        .revalidate()
        .map_err(stale_evidence)?;
    let result = BatchHydrationResult::new(records).map_err(invalid_locator)?;
    result.validate_for_request(request)?;
    Ok(result)
}

fn hydrate_from_snapshot(
    leaf: &RovoDevDocumentLeaf,
    snapshot: &RovoDevSnapshot,
    locator: &SourceRecordLocator,
) -> Result<Vec<u8>, HydrationFailure> {
    if locator.source().provider() != CaptureProvider::RovoDev.as_str()
        || locator.source().source_format() != ROVODEV_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(invalid_locator(
            "locator is not an exact Rovo Dev session-tree record",
        ));
    }
    if !leaf.source_key.exact_descriptor_eq(locator.source())
        || locator.certified_source_revision_digest() != Some(&snapshot.source_sha256)
        || locator.record_digest() != &snapshot.context_sha256
    {
        return Err(stale_evidence(
            "Rovo Dev locator source revision no longer matches provider bytes",
        ));
    }
    let (message_index, expected_native_id) =
        decode_tree_coordinate(locator.coordinate()).map_err(invalid_locator)?;
    let document = snapshot
        .document
        .as_ref()
        .map_err(|_| stale_evidence("Rovo Dev source no longer contains a valid document"))?;
    let message = document
        .messages
        .get(message_index)
        .ok_or_else(|| missing_record("Rovo Dev message coordinate is absent"))?;
    let observed_native_id = provider_message_id(
        message,
        u64::try_from(message_index)
            .map_err(|_| invalid_locator("Rovo Dev message coordinate exceeds platform limits"))?,
    );
    if observed_native_id != expected_native_id {
        return Err(stale_evidence(
            "Rovo Dev locator native message identity changed",
        ));
    }
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(serde_json::Value::as_str);
    let decoded_display_text = lexical_body(message, rovodev_event_type(message, role_text));
    Ok(decoded_display_text.into_bytes())
}

fn decode_tree_coordinate(
    coordinate: &NativeRecordCoordinate,
) -> RovoDevSourceBackedResult<(usize, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(relative_file) = relative_file_key else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(object_kind), TypedKey::U64(message_index), TypedKey::Utf8(native_id)] =
        parts.as_slice()
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    if relative_file != RELATIVE_CONTEXT_FILE
        || object_kind != MESSAGE_OBJECT_KIND
        || native_id.is_empty()
    {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    }
    Ok((
        usize::try_from(*message_index)
            .map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        native_id.clone(),
    ))
}

fn invalid_locator(detail: impl std::fmt::Display) -> HydrationFailure {
    crate::provider::source_backed::hydration_failure(HydrationFailureKind::InvalidLocator, detail)
}

fn missing_record(detail: impl std::fmt::Display) -> HydrationFailure {
    crate::provider::source_backed::hydration_failure(HydrationFailureKind::MissingRecord, detail)
}

fn stale_evidence(detail: impl std::fmt::Display) -> HydrationFailure {
    crate::provider::source_backed::hydration_failure(
        HydrationFailureKind::StaleRecordEvidence,
        detail,
    )
}

fn temporarily_unavailable(detail: impl std::fmt::Display) -> HydrationFailure {
    crate::provider::source_backed::hydration_failure(
        HydrationFailureKind::TemporarilyUnavailable,
        detail,
    )
}

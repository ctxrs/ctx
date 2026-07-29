use super::*;

#[derive(Debug)]
struct ResolvedSource {
    session_path: JunieSessionPath,
    source: SourceKey,
    session_id: StableEntityId,
    provider_session_id: String,
}

#[derive(Debug)]
pub(crate) struct JunieLocatorResolverV0 {
    sources: HashMap<StableEntityId, ResolvedSource>,
}

impl JunieLocatorResolverV0 {
    pub(crate) fn discover(root: impl AsRef<Path>) -> JunieSourceBackedResultV0<Self> {
        let mut sources = HashMap::new();
        let visit = visit_junie_session_event_paths(root.as_ref(), &mut |session_path, _| {
            let provider_session_id = junie_provider_session_id(&session_path)?;
            let source = source_key(&provider_session_id).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Junie source-backed identity is invalid: {error}"
                ))
            })?;
            let session_id = session_identity(&source, &provider_session_id).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Junie source-backed session identity is invalid: {error}"
                ))
            })?;
            let resolved = ResolvedSource {
                session_path,
                source: source.clone(),
                session_id,
                provider_session_id: provider_session_id.clone(),
            };
            if sources.insert(source.identity(), resolved).is_some() {
                return Err(CaptureError::InvalidPayload(format!(
                    "Junie native session {provider_session_id:?} resolves to more than one source"
                )));
            }
            Ok(())
        })?;
        if visit.rejection_count != 0 {
            return Err(JunieSourceBackedErrorV0::IncompleteDiscovery(
                visit.rejection_count,
            ));
        }
        Ok(Self { sources })
    }

    pub(crate) fn discover_for_hydration(root: impl AsRef<Path>) -> Result<Self, HydrationFailure> {
        Self::discover(root)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let resolved = self
            .sources
            .get(&first.locator().source().identity())
            .ok_or_else(|| hydration_failure(HydrationFailureKind::ConfirmedDeleted))?;
        for request in requests {
            request
                .locator()
                .validate_contract()
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
            validate_locator_source(request.locator(), resolved)?;
        }
        let opening = JunieSessionObservation::read(&resolved.session_path)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        let opening_observation = source_observation(&resolved.source, &opening)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        let opening_revision_digest: [u8; 32] =
            Sha256::digest(opening_observation.revision()).into();
        validate_revision_policy(first.locator(), opening_revision_digest)?;
        let file = resolved
            .session_path
            .open_events()
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        let hydrated = requests
            .iter()
            .map(|request| {
                validate_revision_policy(request.locator(), opening_revision_digest)?;
                self.hydrate_from_file(request, resolved, &file)
            })
            .collect::<Result<Vec<_>, _>>()?;
        file.revalidate()
            .and_then(|()| resolved.session_path.revalidate_root())
            .map_err(|_| hydration_failure(HydrationFailureKind::StaleSourceEvidence))?;
        let closing = JunieSessionObservation::read(&resolved.session_path)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        if closing != opening {
            return Err(hydration_failure(HydrationFailureKind::StaleSourceEvidence));
        }
        Ok(hydrated)
    }

    fn hydrate_from_file(
        &self,
        request: &EventHydrationRequest,
        resolved: &ResolvedSource,
        file: &crate::common::io::OpenedProviderSourceFile,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        validate_locator_source(request.locator(), resolved)?;
        let exact_text = match request.locator().coordinate() {
            NativeRecordCoordinate::Jsonl {
                byte_offset,
                byte_length,
                physical_ordinal,
                native_session_key,
                native_event_key,
            } => {
                let expected_event_key = TypedKey::composite(vec![
                    TypedKey::utf8(USER_PROMPT_COORDINATE_KIND)
                        .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
                    TypedKey::U64(request_event_sequence(native_event_key)?),
                ])
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
                if native_session_key.as_ref()
                    != Some(&TypedKey::Utf8(resolved.provider_session_id.clone()))
                    || native_event_key.as_ref() != Some(&expected_event_key)
                {
                    return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
                }
                validate_event_identity(
                    request,
                    resolved,
                    request_event_sequence(native_event_key)?,
                )?;
                let payload = read_payload(file, *byte_offset, *byte_length)?;
                if Sha256::digest(&payload).as_slice() != request.locator().record_digest() {
                    return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
                }
                replay_user_prompt(*physical_ordinal, &payload)?
            }
            NativeRecordCoordinate::TreeRecord {
                relative_file_key,
                record_coordinate,
            } => {
                if relative_file_key != &TypedKey::Utf8(RELATIVE_EVENTS_FILE.to_owned()) {
                    return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
                }
                let (event_sequence, target, entries) = decode_record_set(record_coordinate)?;
                validate_event_identity(request, resolved, event_sequence)?;
                let values = read_record_set(file, &entries, request.locator().record_digest())?;
                replay_record_set(&target, &values)?
            }
            NativeRecordCoordinate::ProviderNative {
                namespace,
                coordinate,
            } if namespace == UNAVAILABLE_COORDINATE_NAMESPACE => {
                let (target, event_sequence) = decode_unavailable_coordinate(coordinate)?;
                validate_event_identity(request, resolved, event_sequence)?;
                if &unavailable_digest(event_sequence, &target) != request.locator().record_digest()
                {
                    return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
                }
                return Err(HydrationFailure {
                    kind: HydrationFailureKind::UnsupportedParserRevision,
                    detail: format!(
                        "Junie exact reopening requires at most {MAX_RECORD_SET_ENTRIES} source records"
                    ),
                });
            }
            _ => return Err(hydration_failure(HydrationFailureKind::InvalidLocator)),
        };
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: exact_text.into_bytes(),
        })
    }
}

impl ContentSourceResolver for JunieLocatorResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let mut hydrated = self.hydrate_requests(std::slice::from_ref(request))?;
        hydrated
            .pop()
            .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: format!("invalid Junie batch hydration result: {error}"),
            },
        )?;
        result.validate_for_request(request)?;
        Ok(result)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

fn validate_revision_policy(
    locator: &SourceRecordLocator,
    current_revision_digest: [u8; 32],
) -> Result<(), HydrationFailure> {
    let expected = locator
        .certified_source_revision_digest()
        .copied()
        .ok_or_else(|| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    match locator.revision_policy() {
        LocatorRevisionPolicy::StableRecordEvidence => Ok(()),
        LocatorRevisionPolicy::ExactSourceRevision if expected == current_revision_digest => Ok(()),
        LocatorRevisionPolicy::ExactSourceRevision => {
            Err(hydration_failure(HydrationFailureKind::StaleSourceEvidence))
        }
    }
}

fn validate_event_identity(
    request: &EventHydrationRequest,
    resolved: &ResolvedSource,
    event_sequence: u64,
) -> Result<(), HydrationFailure> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(event_sequence),
        PositionStability::AppendStable,
    )
    .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    let expected = derive_event_id(EventIdentityInput {
        source: &resolved.source,
        session_id: resolved.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    if expected != request.event_id() {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(())
}

fn validate_locator_source(
    locator: &SourceRecordLocator,
    resolved: &ResolvedSource,
) -> Result<(), HydrationFailure> {
    if locator.source().provider() != CaptureProvider::Junie.as_str()
        || locator.source().source_format() != JUNIE_SESSION_EVENTS_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_none()
        || !locator.source().exact_descriptor_eq(&resolved.source)
    {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if namespace != SOURCE_ANCHOR_NAMESPACE
        || key != &TypedKey::Utf8(resolved.provider_session_id.clone())
    {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(())
}

fn request_event_sequence(native_event_key: &Option<TypedKey>) -> Result<u64, HydrationFailure> {
    let Some(TypedKey::Composite(parts)) = native_event_key else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(kind), TypedKey::U64(sequence)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if kind != USER_PROMPT_COORDINATE_KIND {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(*sequence)
}

pub(super) fn read_payload(
    file: &crate::common::io::OpenedProviderSourceFile,
    byte_offset: u64,
    byte_length: u64,
) -> Result<Vec<u8>, HydrationFailure> {
    if byte_length == 0 || byte_length > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64 {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let length = usize::try_from(byte_length)
        .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    let record = file
        .read_exact_range(byte_offset, length, MAX_JUNIE_TRANSIENT_TURN_BYTES)
        .map_err(|error| match error {
            CaptureError::InvalidPayload(_) => {
                hydration_failure(HydrationFailureKind::MissingRecord)
            }
            _ => hydration_failure(HydrationFailureKind::TemporarilyUnavailable),
        })?;
    Ok(strip_jsonl_ending(&record).to_vec())
}

#[derive(Debug)]
pub(super) struct RecordSetEntry {
    pub(super) ordinal: u64,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) payload_digest: [u8; 32],
}

fn decode_record_set(
    coordinate: &TypedKey,
) -> Result<(u64, SourceBackedTarget, Vec<RecordSetEntry>), HydrationFailure> {
    let TypedKey::Composite(parts) = coordinate else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(kind), TypedKey::U64(event_sequence), target, TypedKey::Composite(encoded_entries)] =
        parts.as_slice()
    else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if kind != RECORD_SET_COORDINATE_KIND {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
        ));
    }
    if encoded_entries.is_empty() || encoded_entries.len() > MAX_RECORD_SET_ENTRIES {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let target = decode_target(target)?;
    let mut entries = Vec::with_capacity(encoded_entries.len());
    for encoded in encoded_entries {
        let TypedKey::Composite(parts) = encoded else {
            return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
        };
        let [TypedKey::U64(ordinal), TypedKey::U64(byte_start), TypedKey::U64(byte_end_exclusive), TypedKey::Bytes(payload_digest)] =
            parts.as_slice()
        else {
            return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
        };
        let payload_digest: [u8; 32] = payload_digest
            .as_slice()
            .try_into()
            .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
        if byte_start >= byte_end_exclusive
            || entries.last().is_some_and(|prior: &RecordSetEntry| {
                prior.ordinal >= *ordinal || prior.byte_end_exclusive > *byte_start
            })
        {
            return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
        }
        entries.push(RecordSetEntry {
            ordinal: *ordinal,
            byte_start: *byte_start,
            byte_end_exclusive: *byte_end_exclusive,
            payload_digest,
        });
    }
    Ok((*event_sequence, target, entries))
}

fn decode_target(target: &TypedKey) -> Result<SourceBackedTarget, HydrationFailure> {
    let TypedKey::Composite(parts) = target else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::U64(tag), TypedKey::U64(first), TypedKey::U64(second)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    match (*tag, *first, *second) {
        (1, 0, 0) => Ok(SourceBackedTarget::UserPrompt),
        (2, 0, 0) => Ok(SourceBackedTarget::AssistantMessage),
        (3, first, 0) => Ok(SourceBackedTarget::StepCall {
            step_order: u32::try_from(first)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
        }),
        (4, first, 0) => Ok(SourceBackedTarget::StepOutput {
            step_order: u32::try_from(first)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
        }),
        (5, first, second) => Ok(SourceBackedTarget::FileChange {
            step_order: u32::try_from(first)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
            change_index: u32::try_from(second)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
        }),
        _ => Err(hydration_failure(HydrationFailureKind::InvalidLocator)),
    }
}

pub(super) fn read_record_set(
    file: &crate::common::io::OpenedProviderSourceFile,
    entries: &[RecordSetEntry],
    expected_digest: &[u8; 32],
) -> Result<Vec<(u64, Value)>, HydrationFailure> {
    let total_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        total.checked_add(entry.byte_end_exclusive.saturating_sub(entry.byte_start))
    });
    if total_bytes.is_none_or(|bytes| bytes > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64) {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let mut aggregate = Sha256::new();
    aggregate.update(RECORD_SET_DIGEST_DOMAIN);
    aggregate.update((entries.len() as u64).to_be_bytes());
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let payload = read_payload(
            file,
            entry.byte_start,
            entry.byte_end_exclusive.saturating_sub(entry.byte_start),
        )?;
        let observed: [u8; 32] = Sha256::digest(&payload).into();
        if observed != entry.payload_digest {
            return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
        }
        aggregate.update(entry.ordinal.to_be_bytes());
        aggregate.update(entry.byte_start.to_be_bytes());
        aggregate.update(entry.byte_end_exclusive.to_be_bytes());
        aggregate.update(observed);
        let value = serde_json::from_slice(&payload)
            .map_err(|_| hydration_failure(HydrationFailureKind::UnsupportedParserRevision))?;
        values.push((entry.ordinal, value));
    }
    let observed: [u8; 32] = aggregate.finalize().into();
    if &observed != expected_digest {
        return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
    }
    Ok(values)
}

pub(super) fn replay_user_prompt(ordinal: u64, payload: &[u8]) -> Result<String, HydrationFailure> {
    let value: Value = serde_json::from_slice(payload)
        .map_err(|_| hydration_failure(HydrationFailureKind::UnsupportedParserRevision))?;
    if value.get("kind").and_then(Value::as_str) != Some("UserPromptEvent") {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
        ));
    }
    value
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            let _ = ordinal;
            hydration_failure(HydrationFailureKind::MissingRecord)
        })
}

pub(super) fn replay_record_set(
    target: &SourceBackedTarget,
    values: &[(u64, Value)],
) -> Result<String, HydrationFailure> {
    let mut buffer = JunieAssistantBuffer::default();
    for (ordinal, value) in values {
        if value.get("kind").and_then(Value::as_str) != Some("SessionA2uxEvent") {
            return Err(hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
            ));
        }
        let agent = value
            .get("event")
            .and_then(|event| event.get("agentEvent"))
            .ok_or_else(|| hydration_failure(HydrationFailureKind::UnsupportedParserRevision))?;
        let occurred_at = value
            .get("timestampMs")
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        if !junie_merge_buffered_agent_event(
            &mut buffer,
            agent,
            ordinal.saturating_add(1),
            occurred_at,
        ) {
            return Err(hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
            ));
        }
    }
    match target {
        SourceBackedTarget::UserPrompt => {
            Err(hydration_failure(HydrationFailureKind::InvalidLocator))
        }
        SourceBackedTarget::AssistantMessage => {
            let text = junie_buffer_result_text(&buffer);
            if text.is_empty() {
                Err(hydration_failure(HydrationFailureKind::MissingRecord))
            } else {
                Ok(text)
            }
        }
        SourceBackedTarget::StepCall { step_order } => {
            let step = step_by_order(&buffer, *step_order)?;
            Ok(step_call_text(step))
        }
        SourceBackedTarget::StepOutput { step_order } => {
            let step = step_by_order(&buffer, *step_order)?;
            junie_step_output_projection(step)
                .map(|output| output.details.to_owned())
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
        }
        SourceBackedTarget::FileChange {
            step_order,
            change_index,
        } => {
            let step = step_by_order(&buffer, *step_order)?;
            let change = step
                .changes
                .get(*change_index as usize)
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
            let path = change
                .get("afterRelativePath")
                .and_then(Value::as_str)
                .or_else(|| change.get("beforeRelativePath").and_then(Value::as_str))
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
            Ok(format!("Edit: {path}"))
        }
    }
}

fn step_by_order(
    buffer: &JunieAssistantBuffer,
    step_order: u32,
) -> Result<&JunieStepAgg, HydrationFailure> {
    let step_id = buffer
        .step_ids_in_order
        .get(step_order as usize)
        .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
    buffer
        .steps
        .get(step_id)
        .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
}

fn step_call_text(step: &JunieStepAgg) -> String {
    if let Some(command) = &step.command {
        format!("Bash: {command}")
    } else if step.files.is_some() {
        step.label
            .clone()
            .unwrap_or_else(|| "View files".to_owned())
    } else {
        step.label
            .clone()
            .unwrap_or_else(|| "Junie tool step".to_owned())
    }
}

fn decode_unavailable_coordinate(
    coordinate: &TypedKey,
) -> Result<(TypedKey, u64), HydrationFailure> {
    let TypedKey::Composite(parts) = coordinate else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [target, TypedKey::U64(event_sequence)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    decode_target(target)?;
    Ok((target.clone(), *event_sequence))
}

fn hydration_failure(kind: HydrationFailureKind) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: "Junie source-backed locator could not be verified".to_owned(),
    }
}

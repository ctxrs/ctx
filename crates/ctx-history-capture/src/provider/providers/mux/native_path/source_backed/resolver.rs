use super::*;

#[derive(Debug)]
pub(crate) struct MuxSourceBackedResolverV0 {
    sources: HashMap<StableEntityId, MuxSourceBackedCandidate>,
}

#[derive(Debug)]
pub(super) struct MuxLogicalRecordCoordinate {
    pub(super) stream_kind: MuxStreamKind,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) source_record_ordinal: u64,
    pub(super) event_sequence: u64,
    pub(super) native_record_id: String,
}

impl MuxSourceBackedResolverV0 {
    pub(crate) fn discover(root: &Path, observed_at: DateTime<Utc>) -> MuxSourceBackedResult<Self> {
        let mut sources = HashMap::new();
        for candidate in discover_mux_source_backed_sources(root, observed_at)? {
            let provider_session_id = candidate.provider_session_id().to_owned();
            if sources
                .insert(candidate.source_key.identity(), candidate)
                .is_some()
            {
                return Err(MuxSourceBackedError::DuplicateNativeSession(
                    provider_session_id,
                ));
            }
        }
        Ok(Self { sources })
    }

    pub(crate) fn discover_for_hydration(
        root: &Path,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, HydrationFailure> {
        Self::discover(root, observed_at)
            .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let candidate = self
            .sources
            .get(&first.locator().source().identity())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "the exact Mux session is absent from the complete source inventory",
                )
            })?;
        for request in requests {
            validate_mux_locator(candidate, request.locator())?;
        }
        let opening = admit_mux_candidate(candidate).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        let current_observation = source_observation(&candidate.source_key, &opening.wire)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        let current_revision_digest: [u8; 32] =
            Sha256::digest(current_observation.revision()).into();
        let hydrated = requests
            .iter()
            .map(|request| {
                hydrate_mux_request(candidate, &opening, current_revision_digest, request)
            })
            .collect::<Result<Vec<_>, _>>()?;
        opening
            .revalidate(&candidate.authority)
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;
        let closing = admit_mux_candidate(candidate).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        closing
            .revalidate(&candidate.authority)
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;
        if closing.wire != opening.wire {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "Mux compound source changed during exact hydration",
            ));
        }
        Ok(hydrated)
    }
}

impl ContentSourceResolver for MuxSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let mut hydrated = self.hydrate_requests(std::slice::from_ref(request))?;
        hydrated.pop().ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Mux exact event hydration returned no record",
            )
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid Mux batch hydration result: {error}"),
                )
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

fn validate_mux_locator(
    candidate: &MuxSourceBackedCandidate,
    locator: &SourceRecordLocator,
) -> Result<(), HydrationFailure> {
    locator
        .validate_contract()
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    if locator.source().provider() != CaptureProvider::Mux.as_str()
        || locator.source().source_format() != MUX_SOURCE_FORMAT
        || locator.source().schema_variant() != MUX_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.certified_source_revision_digest().is_none()
        || !candidate.source_key.exact_descriptor_eq(locator.source())
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator source descriptor is invalid",
        ));
    }
    let coordinate = decode_mux_coordinate(locator)?;
    let expected_policy = if coordinate.stream_kind.is_partial() {
        LocatorRevisionPolicy::ExactSourceRevision
    } else {
        LocatorRevisionPolicy::StableRecordEvidence
    };
    if locator.revision_policy() != expected_policy {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator revision policy does not match its native stream",
        ));
    }
    Ok(())
}

fn hydrate_mux_request(
    candidate: &MuxSourceBackedCandidate,
    opening: &MuxObservedSource,
    current_revision_digest: [u8; 32],
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let coordinate = decode_mux_coordinate(request.locator())?;
    if coordinate.stream_kind.is_partial()
        && request.locator().certified_source_revision_digest() != Some(&current_revision_digest)
    {
        return Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Mux partial snapshot revision changed",
        ));
    }
    let source = match coordinate.stream_kind {
        MuxStreamKind::Chat => opening.chat_file.as_ref(),
        MuxStreamKind::Partial => opening.partial_file.as_ref(),
    }
    .ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Mux locator stream is absent from the current session source",
        )
    })?;
    let payload = read_mux_payload(source, &coordinate)?;
    if Sha256::digest(&payload).as_slice() != request.locator().record_digest() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux source record digest changed",
        ));
    }
    let value = serde_json::from_slice::<serde_json::Value>(&payload).map_err(|error| {
        hydration_failure(HydrationFailureKind::UnsupportedParserRevision, error)
    })?;
    if !value.is_object() {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Mux native record is not an object",
        ));
    }
    validate_mux_native_identity(candidate, request, &coordinate, &payload, &value)?;
    let provider_bytes = mux_exact_logical_content(&value)?;
    Ok(HydratedProviderRecord {
        event_id: request.event_id(),
        provider_bytes: provider_bytes.into_bytes(),
    })
}

pub(super) fn read_mux_payload(
    source: &OpenedProviderSourceFile,
    coordinate: &MuxLogicalRecordCoordinate,
) -> Result<Vec<u8>, HydrationFailure> {
    let byte_length = coordinate
        .byte_end_exclusive
        .checked_sub(coordinate.byte_start)
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux locator byte range moved backwards",
            )
        })?;
    if byte_length == 0 || byte_length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64 {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator byte range exceeds the native record bound",
        ));
    }
    if coordinate.byte_end_exclusive > source.len() {
        return Err(hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Mux locator byte range is no longer present",
        ));
    }
    if coordinate.stream_kind == MuxStreamKind::Chat && coordinate.byte_start > 0 {
        let boundary = source
            .read_exact_range(coordinate.byte_start - 1, 1, 1)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        if boundary != b"\n" {
            return Err(hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Mux chat record start boundary changed",
            ));
        }
    }
    let length = usize::try_from(byte_length).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator byte range exceeds platform limits",
        )
    })?;
    let provider_bytes = source
        .read_exact_range(
            coordinate.byte_start,
            length,
            MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
        )
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    if coordinate.stream_kind == MuxStreamKind::Partial {
        if coordinate.byte_start != 0
            || coordinate.byte_end_exclusive != source.len()
            || coordinate.source_record_ordinal != 0
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux partial locator does not address its whole snapshot",
            ));
        }
        return Ok(provider_bytes);
    }
    let first_newline = provider_bytes.iter().position(|byte| *byte == b'\n');
    if first_newline.is_some_and(|position| position + 1 != provider_bytes.len())
        || (first_newline.is_none() && coordinate.byte_end_exclusive != source.len())
    {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux chat record end boundary changed",
        ));
    }
    Ok(strip_jsonl_record_ending(&provider_bytes).to_vec())
}

fn strip_jsonl_record_ending(record: &[u8]) -> &[u8] {
    record
        .strip_suffix(b"\n")
        .unwrap_or(record)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| record.strip_suffix(b"\n").unwrap_or(record))
}

pub(super) fn encode_mux_coordinate(
    stream_kind: MuxStreamKind,
    legacy_locator: &[u8],
    source_record_ordinal: u64,
    event_sequence: u64,
    native_record_id: &str,
) -> MuxSourceBackedResult<TypedKey> {
    let (tag, byte_start, byte_end_exclusive) =
        decode_mux_legacy_range(legacy_locator).ok_or(MuxSourceBackedError::InvalidLocator)?;
    let expected_tag = if stream_kind.is_partial() { 2 } else { 1 };
    if tag != expected_tag {
        return Err(MuxSourceBackedError::InvalidLocator);
    }
    Ok(TypedKey::composite(vec![
        TypedKey::U64(2),
        TypedKey::U64(tag),
        TypedKey::U64(byte_start),
        TypedKey::U64(byte_end_exclusive),
        TypedKey::U64(source_record_ordinal),
        TypedKey::U64(event_sequence),
        TypedKey::utf8(native_record_id)?,
    ])?)
}

pub(super) fn decode_mux_legacy_range(value: &[u8]) -> Option<(u64, u64, u64)> {
    if value.len() != 17 {
        return None;
    }
    let tag = u64::from(value[0]);
    let byte_start = u64::from_be_bytes(value[1..9].try_into().ok()?);
    let byte_end_exclusive = u64::from_be_bytes(value[9..17].try_into().ok()?);
    if !matches!(tag, 1 | 2) || byte_start >= byte_end_exclusive || (tag == 2 && byte_start != 0) {
        return None;
    }
    Some((tag, byte_start, byte_end_exclusive))
}

pub(super) fn decode_mux_coordinate(
    locator: &SourceRecordLocator,
) -> Result<MuxLogicalRecordCoordinate, HydrationFailure> {
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator does not use a provider-native coordinate",
        ));
    };
    if namespace != MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE {
        return Err(hydration_failure(
            if namespace.starts_with("mux.") {
                HydrationFailureKind::UnsupportedParserRevision
            } else {
                HydrationFailureKind::InvalidLocator
            },
            "Mux locator namespace is unsupported",
        ));
    }
    let TypedKey::Composite(parts) = coordinate else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is malformed",
        ));
    };
    let [TypedKey::U64(version), TypedKey::U64(tag), TypedKey::U64(byte_start), TypedKey::U64(byte_end_exclusive), TypedKey::U64(source_record_ordinal), TypedKey::U64(event_sequence), TypedKey::Utf8(native_record_id)] =
        parts.as_slice()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is malformed",
        ));
    };
    if *version != 2 {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Mux locator parser revision is unsupported",
        ));
    }
    let stream_kind = match *tag {
        1 => MuxStreamKind::Chat,
        2 => MuxStreamKind::Partial,
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux locator stream tag is invalid",
            ))
        }
    };
    if byte_start >= byte_end_exclusive
        || native_record_id.is_empty()
        || (stream_kind.is_partial() && (*byte_start != 0 || *source_record_ordinal != 0))
        || (!stream_kind.is_partial() && event_sequence != source_record_ordinal)
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is internally inconsistent",
        ));
    }
    Ok(MuxLogicalRecordCoordinate {
        stream_kind,
        byte_start: *byte_start,
        byte_end_exclusive: *byte_end_exclusive,
        source_record_ordinal: *source_record_ordinal,
        event_sequence: *event_sequence,
        native_record_id: native_record_id.clone(),
    })
}

fn validate_mux_native_identity(
    candidate: &MuxSourceBackedCandidate,
    request: &EventHydrationRequest,
    coordinate: &MuxLogicalRecordCoordinate,
    payload: &[u8],
    value: &serde_json::Value,
) -> Result<(), HydrationFailure> {
    let line_number = usize::try_from(coordinate.source_record_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux native ordinal exceeds platform limits",
            )
        })?;
    let role = value
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let native_record_id = mux_event_id(
        value,
        line_number,
        role,
        coordinate.stream_kind.is_partial(),
    );
    if native_record_id != coordinate.native_record_id {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux native record identity changed",
        ));
    }
    let expected_sequence = if coordinate.stream_kind.is_partial() {
        MUX_PARTIAL_NATIVE_ORDINAL | (mux_partial_event_index(payload) & MUX_MAX_ORDINAL)
    } else {
        coordinate.source_record_ordinal
    };
    if expected_sequence != coordinate.event_sequence {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux native event sequence changed",
        ));
    }
    let native_item_key = NativeItemKey::native_id(
        MUX_NATIVE_ITEM_NAMESPACE,
        TypedKey::utf8(&native_record_id).map_err(|error| {
            hydration_failure(HydrationFailureKind::UnsupportedParserRevision, error)
        })?,
    )
    .map_err(|error| hydration_failure(HydrationFailureKind::UnsupportedParserRevision, error))?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source: &candidate.source_key,
        session_id: candidate.session_id,
        logical_item_kind: MUX_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    if expected_event_id != request.event_id() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux event identity does not match its native coordinate",
        ));
    }
    if let Some(output) = mux_output_projection(value) {
        if !output.body_available {
            return Err(hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Mux native output body is unavailable",
            ));
        }
        if !matches!(
            output.outcome,
            MuxOutputOutcome::Failure | MuxOutputOutcome::Timeout
        ) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux successful output is not an indexed Core event",
            ));
        }
    }
    Ok(())
}

pub(super) fn mux_exact_logical_content(
    value: &serde_json::Value,
) -> Result<String, HydrationFailure> {
    let event_type = mux_event_type(value);
    if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return mux_result_content(value).ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Mux exact output body is unavailable",
            )
        });
    }
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        for part in parts {
            match part.get("type").and_then(serde_json::Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(mux_exact_tool_part_text(part)),
                Some("file") => {
                    if let Some(label) = mux_exact_file_part_text(part) {
                        rendered.push(label);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return Ok(rendered.join("\n"));
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return Ok(text);
    }
    Ok(mux_event_text(value, event_type))
}

fn mux_exact_tool_part_text(part: &serde_json::Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(serde_json::Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push_str("\ninput: ");
        text.push_str(&mux_exact_value_text(input));
    }
    if let Some(output) = part.get("output") {
        text.push_str("\noutput: ");
        text.push_str(&mux_exact_value_text(output));
    }
    if let Some(nested) = part
        .get("nestedCalls")
        .and_then(serde_json::Value::as_array)
    {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push_str("\nnested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn mux_exact_value_text(value: &serde_json::Value) -> String {
    provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn mux_exact_file_part_text(part: &serde_json::Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(serde_json::Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

fn hydration_failure(
    kind: HydrationFailureKind,
    detail: impl std::fmt::Display,
) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}

use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionHydrationRequest, SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey,
    SourceObservation as ProjectionSourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use thiserror::Error;

use super::*;
use crate::complete_content::{
    jsonl::ExactJsonlSourceBinding, AuthorizedSourceRoute, CompleteContentBodyDigest,
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily,
    SourceAccessBroker, SourceSnapshot,
};

const SOURCE_SCHEMA_VARIANT: &str = "meta-json-messages-jsonl-v1";
const SOURCE_ANCHOR_NAMESPACE: &str = "mistral-vibe-session-id";
const NATIVE_SESSION_NAMESPACE: &str = "mistral-vibe-session";
const NATIVE_EVENT_NAMESPACE: &str = "mistral-vibe-message";
const NATIVE_EVENT_POSITION_KIND: &str = "mistral-vibe-messages-jsonl-ordinal";
const LOGICAL_SESSION_KIND: &str = "mistral-vibe-session";
const LOGICAL_EVENT_KIND: &str = "mistral-vibe-event";
const SOURCE_REVISION_KIND: &str = "mistral-vibe-meta-messages-observation-v1";
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx.mistral-vibe.source-revision.v1\0";
const SOURCE_CONTENT_DIGEST_DOMAIN: &[u8] = b"ctx.mistral-vibe.source-content.v1\0";
const SOURCE_FRONTIER_KIND: &str = "mistral-vibe-meta-messages-prefix-v1";
const PARSER_REVISION: &str = "mistral-vibe-source-backed-v1";

#[derive(Debug, Error)]
pub(crate) enum MistralVibeSourceBackedError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    ResolverContract(#[from] SourceResolverContractError),
    #[error("Mistral Vibe source-backed root contains no complete session directories")]
    EmptyRoot,
    #[error("Mistral Vibe source-backed root contains duplicate session IDs")]
    DuplicateSessionId,
    #[error("Mistral Vibe source-backed count overflow")]
    CountOverflow,
}

pub(crate) type MistralVibeSourceBackedResult<T> =
    std::result::Result<T, MistralVibeSourceBackedError>;

#[derive(Debug)]
pub(crate) struct MistralVibeSourceBackedLeaf {
    pub(crate) source: CertifiedSource,
    pub(crate) documents: Vec<LexicalDocument>,
}

#[derive(Debug)]
pub(crate) struct MistralVibeSourceBackedScan {
    pub(crate) leaves: Vec<MistralVibeSourceBackedLeaf>,
    pub(crate) resolver: MistralVibeSourceResolver,
}

#[derive(Debug, Clone)]
struct ResolvableSource {
    source: SourceKey,
    native: MistralVibeSessionSource,
    observation: reader::SourceObservation,
    revision_digest: [u8; 32],
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MistralVibeSourceResolver {
    sources: BTreeMap<SourceKey, ResolvableSource>,
}

/// Scans each complete Mistral Vibe session directory as one composite source.
///
/// `meta.json` contributes native session identity, bounded session metadata,
/// and source/revision evidence. It never produces an independent lexical
/// document or a durable content body.
pub(crate) fn scan_mistral_vibe_source_backed(
    root: &Path,
    imported_at: DateTime<Utc>,
) -> MistralVibeSourceBackedResult<MistralVibeSourceBackedScan> {
    let mut discovered = Vec::new();
    visit_mistral_vibe_session_sources(root, &mut |source| {
        discovered.push(source);
        Ok(())
    })?;
    if discovered.is_empty() {
        return Err(MistralVibeSourceBackedError::EmptyRoot);
    }
    discovered.sort_by(|left, right| left.messages_path.cmp(&right.messages_path));

    let mut native_session_ids = HashSet::new();
    let mut leaves = Vec::with_capacity(discovered.len());
    let mut resolver = MistralVibeSourceResolver::default();
    for source in discovered {
        let (leaf, resolvable, native_session_id) = scan_leaf(source, imported_at)?;
        if !native_session_ids.insert(native_session_id) {
            return Err(MistralVibeSourceBackedError::DuplicateSessionId);
        }
        if resolver
            .sources
            .insert(resolvable.source.clone(), resolvable)
            .is_some()
        {
            return Err(MistralVibeSourceBackedError::DuplicateSessionId);
        }
        leaves.push(leaf);
    }
    leaves.sort_by(|left, right| {
        left.source
            .observation()
            .source()
            .cmp(right.source.observation().source())
    });
    Ok(MistralVibeSourceBackedScan { leaves, resolver })
}

fn scan_leaf(
    native: MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
) -> MistralVibeSourceBackedResult<(MistralVibeSourceBackedLeaf, ResolvableSource, String)> {
    let opening = reader::SourceObservation::read(&native)?;
    let (session, _) = SessionFact::from_source(&native, imported_at)?;
    let native_session_id = session.provider_session_id.clone();
    let source = source_key(&native_session_id)?;
    let session_id = session_identity(&source, &native_session_id)?;
    let opening_revision = projection_observation(&source, &opening)?;
    let revision_digest = source_revision_digest(opening_revision.revision());

    let file = File::open(&opening.canonical_messages_path)?;
    let mut file_reader = BufReader::new(file);
    let mut content_hasher = Sha256::new();
    content_hasher.update(SOURCE_CONTENT_DIGEST_DOMAIN);
    content_hasher.update(opening.metadata.length.to_be_bytes());
    content_hasher.update(opening.metadata_sha256);

    let mut documents = Vec::new();
    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut next_ordinal = 0_u64;
    let mut complete_prefix_end = 0_u64;

    loop {
        let start = complete_prefix_end;
        let hasher_before = content_hasher.clone();
        let line = read_bounded_line(
            &mut file_reader,
            &mut content_hasher,
            opening.messages.length,
            start,
        )?;
        match line {
            Line::EndOfFile => break,
            Line::IncompleteTail => {
                content_hasher = hasher_before;
                break;
            }
            Line::Oversized { end } => {
                complete_records = checked_increment(complete_records)?;
                rejected_records = checked_increment(rejected_records)?;
                next_ordinal = checked_increment(next_ordinal)?;
                complete_prefix_end = end;
            }
            Line::Complete { bytes, end } => {
                complete_records = checked_increment(complete_records)?;
                let ordinal = next_ordinal;
                next_ordinal = checked_increment(next_ordinal)?;
                complete_prefix_end = end;
                match lexical_document(
                    &source,
                    session_id,
                    &session,
                    ordinal,
                    start,
                    end,
                    &bytes,
                    revision_digest,
                )? {
                    RecordProjection::Retained(document) => {
                        retained_records = checked_increment(retained_records)?;
                        documents.push(document);
                    }
                    RecordProjection::Rejected => {
                        rejected_records = checked_increment(rejected_records)?;
                    }
                    RecordProjection::Ignored => {
                        ignored_records = checked_increment(ignored_records)?;
                    }
                }
            }
        }
    }

    let closing = reader::SourceObservation::read(&native)?;
    let closing_revision = projection_observation(&source, &closing)?;
    let content_digest: [u8; 32] = content_hasher.finalize().into();
    let certified_bytes = opening
        .metadata
        .length
        .checked_add(complete_prefix_end)
        .ok_or(MistralVibeSourceBackedError::CountOverflow)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: u64::try_from(documents.len())
            .map_err(|_| MistralVibeSourceBackedError::CountOverflow)?,
        certified_bytes,
    };
    let frontier = SourceFrontier::new(
        SOURCE_FRONTIER_KIND,
        TypedKey::composite(vec![
            TypedKey::bytes(opening.metadata_sha256.to_vec())?,
            TypedKey::U64(complete_prefix_end),
            TypedKey::U64(next_ordinal),
        ])?,
        certified_bytes,
        content_digest,
    )?;
    let certified = CertifiedSource::certify_with_frontier(
        opening_revision,
        closing_revision,
        PARSER_REVISION,
        content_digest,
        counts,
        Some(frontier),
    )?;
    let resolvable = ResolvableSource {
        source,
        native,
        observation: closing,
        revision_digest,
    };
    Ok((
        MistralVibeSourceBackedLeaf {
            source: certified,
            documents,
        },
        resolvable,
        native_session_id,
    ))
}

enum RecordProjection {
    Retained(LexicalDocument),
    Rejected,
    Ignored,
}

#[allow(clippy::too_many_arguments)]
fn lexical_document(
    source: &SourceKey,
    session_id: StableEntityId,
    session: &SessionFact,
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    bytes: &[u8],
    revision_digest: [u8; 32],
) -> MistralVibeSourceBackedResult<RecordProjection> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(RecordProjection::Ignored);
    }
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(RecordProjection::Rejected),
    };
    let role = match valid_mistral_vibe_record_role(&value) {
        Ok(role) => role,
        Err(_) => return Ok(RecordProjection::Rejected),
    };
    let mut event_type = mistral_vibe_event_type(role, &value);
    let output = (event_type == EventType::ToolOutput)
        .then(|| output_metadata(&value, line_number(ordinal), role, session.cwd.as_deref()));
    if output.as_ref().is_some_and(|output| {
        !matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    }) {
        return Ok(RecordProjection::Ignored);
    }

    let lexical_text = if let Some(output) = &output {
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        format!(
            "Mistral Vibe failed {} output",
            value.get("name").and_then(Value::as_str).unwrap_or("tool")
        )
    } else {
        mistral_vibe_event_text(role, &value, event_type)
    };
    let body = provider_local_preview(&lexical_text, MAX_BODY_PREVIEW_CHARS).0;
    if body.is_empty() {
        return Ok(RecordProjection::Rejected);
    }

    let native_event_id = provider_native_event_id(&value);
    let native_item_key = match native_event_id.as_deref() {
        Some(native_event_id) => {
            NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, TypedKey::utf8(native_event_id)?)?
        }
        None => NativeItemKey::certified_position(
            NATIVE_EVENT_POSITION_KIND,
            TypedKey::U64(ordinal),
            PositionStability::AppendStable,
        )?,
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_key = native_event_id
        .map(TypedKey::utf8)
        .transpose()?
        .unwrap_or(TypedKey::U64(ordinal));
    let byte_length = byte_end_exclusive
        .checked_sub(byte_start)
        .ok_or(MistralVibeSourceBackedError::CountOverflow)?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: byte_start,
            byte_length,
            physical_ordinal: ordinal,
            native_session_key: Some(TypedKey::utf8(session.provider_session_id.as_str())?),
            native_event_key: Some(native_event_key),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        Sha256::digest(bytes).into(),
    )?;
    let touches = collect_touches(&value)?
        .touches
        .into_iter()
        .map(|touch| touch.path)
        .collect();
    let role = crate::provider::normalization::provider_role(Some(role));
    Ok(RecordProjection::Retained(LexicalDocument {
        event_id,
        session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        event_sequence: ordinal,
        occurred_at_unix_ms: Some(
            native_jsonl_timestamp(&value)
                .unwrap_or(session.started_at)
                .timestamp_millis(),
        ),
        event_type: event_type.as_str().to_owned(),
        role: Some(role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: session.cwd.clone(),
        touched_files: touches,
    }))
}

fn provider_native_event_id(value: &Value) -> Option<String> {
    value
        .get("message_id")
        .or_else(|| value.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn line_number(ordinal: u64) -> usize {
    usize::try_from(ordinal)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}

fn checked_increment(value: u64) -> MistralVibeSourceBackedResult<u64> {
    value
        .checked_add(1)
        .ok_or(MistralVibeSourceBackedError::CountOverflow)
}

fn source_key(native_session_id: &str) -> MistralVibeSourceBackedResult<SourceKey> {
    let anchor =
        SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, TypedKey::utf8(native_session_id)?)?;
    Ok(SourceKey::derive(
        CaptureProvider::MistralVibe.as_str(),
        MISTRAL_VIBE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> MistralVibeSourceBackedResult<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(NATIVE_SESSION_NAMESPACE, TypedKey::utf8(native_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

#[derive(Serialize)]
struct CompositeRevision<'a> {
    capture_revision: u32,
    policy_revision: u32,
    metadata: &'a FileStamp,
    messages: &'a FileStamp,
    metadata_sha256: [u8; 32],
    exact_content_revision: &'a str,
}

fn projection_observation(
    source: &SourceKey,
    observation: &reader::SourceObservation,
) -> MistralVibeSourceBackedResult<ProjectionSourceObservation> {
    let revision = serde_json::to_vec(&CompositeRevision {
        capture_revision: MISTRAL_VIBE_CAPTURE_REVISION,
        policy_revision: MISTRAL_VIBE_POLICY_REVISION,
        metadata: &observation.metadata,
        messages: &observation.messages,
        metadata_sha256: observation.metadata_sha256,
        exact_content_revision: &observation.exact_content_revision,
    })?;
    Ok(ProjectionSourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        revision,
    )?)
}

fn source_revision_digest(revision: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SOURCE_REVISION_DIGEST_DOMAIN);
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision);
    digest.finalize().into()
}

impl ContentSourceResolver for MistralVibeSourceResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let mut hydrated = self.hydrate_requests(std::slice::from_ref(request))?;
        hydrated
            .pop()
            .ok_or_else(|| invalid_locator("empty event hydration result"))
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

impl MistralVibeSourceResolver {
    fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let first_locator = first.locator();
        validate_locator(first_locator)?;
        let route = self
            .sources
            .get(first_locator.source())
            .ok_or_else(|| unavailable("Mistral Vibe source route is unavailable"))?;
        first_locator
            .source()
            .validate_exact_descriptor(&route.source)
            .map_err(|_| invalid_locator("Mistral Vibe source descriptor does not match"))?;
        let expected_revision = first_locator
            .certified_source_revision_digest()
            .copied()
            .ok_or_else(|| invalid_locator("Mistral Vibe locator has no exact revision"))?;
        if expected_revision != route.revision_digest {
            return Err(stale_source(
                "Mistral Vibe locator revision is no longer active",
            ));
        }
        for request in requests {
            validate_locator(request.locator())?;
            request
                .locator()
                .source()
                .validate_exact_descriptor(&route.source)
                .map_err(|_| invalid_locator("Mistral Vibe hydration batch crosses sources"))?;
            if request.locator().certified_source_revision_digest() != Some(&expected_revision) {
                return Err(invalid_locator(
                    "Mistral Vibe hydration batch crosses source revisions",
                ));
            }
        }

        revalidate_revision(route, expected_revision)?;
        let event_uuid = first.event_id().as_uuid();
        let access = SourceAccessBroker::new()
            .admit(
                AuthorizedSourceRoute {
                    source_id: route.source.identity().as_uuid(),
                    provider: CaptureProvider::MistralVibe,
                    source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
                    family: CompleteContentSourceFamily::Jsonl,
                    raw_source_path: route.native.messages_path.clone(),
                    source_root: Some(route.native.session_dir.clone()),
                    source_identity: Some(route.source.identity().to_string()),
                    source_snapshot: SourceSnapshot {
                        size_bytes: Some(route.observation.messages.length),
                        modified_at_ms: None,
                        sha256: None,
                    },
                },
                event_uuid,
            )
            .map_err(map_complete_content_error)?;
        let current_path_identity =
            provider_path_identity(&route.observation.canonical_messages_path)
                .map_err(|_| unavailable("Mistral Vibe path identity is unavailable"))?;
        let expected_binding = ExactJsonlSourceBinding::new(
            &route.observation.exact_content_revision,
            &current_path_identity,
        );
        if access.exact_jsonl_binding() != Some(&expected_binding) {
            return Err(stale_source(
                "Mistral Vibe auxiliary source evidence changed during admission",
            ));
        }

        let mut hydrated = Vec::with_capacity(requests.len());
        for request in requests {
            let (byte_offset, byte_length) = locator_range(request.locator())?;
            let byte_end_exclusive = byte_offset
                .checked_add(byte_length)
                .ok_or_else(|| invalid_locator("Mistral Vibe locator range overflowed"))?;
            let expected_digest =
                CompleteContentBodyDigest::parse(lower_hex(request.locator().record_digest()))
                    .ok_or_else(|| invalid_locator("Mistral Vibe locator digest is invalid"))?;
            let provider_bytes = access
                .read_jsonl_record(
                    byte_offset,
                    byte_end_exclusive,
                    &expected_digest,
                    request.event_id().as_uuid(),
                )
                .map_err(map_complete_content_error)?;
            hydrated.push(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            });
        }
        access
            .revalidate_jsonl(event_uuid)
            .map_err(map_complete_content_error)?;
        revalidate_revision(route, expected_revision)?;
        Ok(hydrated)
    }
}

fn validate_locator(locator: &SourceRecordLocator) -> std::result::Result<(), HydrationFailure> {
    locator
        .validate_contract()
        .map_err(|_| invalid_locator("Mistral Vibe locator contract is invalid"))?;
    if locator.source().provider() != CaptureProvider::MistralVibe.as_str()
        || locator.source().source_format() != MISTRAL_VIBE_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(invalid_locator(
            "locator does not describe a source-backed Mistral Vibe record",
        ));
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(invalid_locator(
            "Mistral Vibe locator source anchor is invalid",
        ));
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return Err(invalid_locator(
            "Mistral Vibe locator session key is invalid",
        ));
    };
    if namespace != SOURCE_ANCHOR_NAMESPACE {
        return Err(invalid_locator(
            "Mistral Vibe locator source namespace is invalid",
        ));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_length,
        native_session_key,
        native_event_key,
        ..
    } = locator.coordinate()
    else {
        return Err(invalid_locator("Mistral Vibe locator is not a JSONL range"));
    };
    if *byte_length == 0
        || native_session_key.as_ref() != Some(&TypedKey::Utf8(native_session_id.clone()))
        || native_event_key.is_none()
    {
        return Err(invalid_locator(
            "Mistral Vibe locator native coordinates are inconsistent",
        ));
    }
    Ok(())
}

fn locator_range(
    locator: &SourceRecordLocator,
) -> std::result::Result<(u64, u64), HydrationFailure> {
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        ..
    } = locator.coordinate()
    else {
        return Err(invalid_locator("Mistral Vibe locator is not a JSONL range"));
    };
    Ok((*byte_offset, *byte_length))
}

fn revalidate_revision(
    route: &ResolvableSource,
    expected: [u8; 32],
) -> std::result::Result<(), HydrationFailure> {
    let current = reader::SourceObservation::read(&route.native)
        .map_err(|_| unavailable("Mistral Vibe source could not be observed"))?;
    let current = projection_observation(&route.source, &current)
        .map_err(|_| unavailable("Mistral Vibe source revision could not be encoded"))?;
    if source_revision_digest(current.revision()) != expected {
        return Err(stale_source(
            "Mistral Vibe meta.json or messages.jsonl changed",
        ));
    }
    Ok(())
}

fn map_complete_content_error(error: CompleteContentError) -> HydrationFailure {
    let kind = match error.kind {
        CompleteContentErrorKind::SourceRecordMissing => HydrationFailureKind::MissingRecord,
        CompleteContentErrorKind::ContentVerificationFailed => {
            HydrationFailureKind::StaleRecordEvidence
        }
        CompleteContentErrorKind::SourceChanged => HydrationFailureKind::StaleSourceEvidence,
        CompleteContentErrorKind::HydrationUnsupported => HydrationFailureKind::InvalidLocator,
        CompleteContentErrorKind::SourceMissing | CompleteContentErrorKind::SourceUnreadable => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        CompleteContentErrorKind::ContentTooLarge => HydrationFailureKind::InvalidLocator,
    };
    HydrationFailure {
        kind,
        detail: format!(
            "Mistral Vibe exact hydration failed: {}",
            error.kind.as_str()
        ),
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn unavailable(detail: &'static str) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::TemporarilyUnavailable,
        detail: detail.to_owned(),
    }
}

fn stale_source(detail: &'static str) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::StaleSourceEvidence,
        detail: detail.to_owned(),
    }
}

fn invalid_locator(detail: &'static str) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::InvalidLocator,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::TimeZone;
    use ctx_history_core::{ContentSourceResolver, EventHydrationRequest, SessionHydrationRequest};
    use serde_json::json;

    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, Vec<Vec<u8>>) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let session = root.join("session-alpha");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("meta.json"),
            serde_json::to_vec(&json!({
                "session_id": "session-alpha",
                "title": "metadata-only-sentinel-a",
                "start_time": "2026-07-28T12:00:00Z",
                "environment": {
                    "working_directory": "/workspace/project"
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let records = vec![
            serde_json::to_vec(&json!({
                "role": "user",
                "message_id": "message-user",
                "timestamp": "2026-07-28T12:00:01Z",
                "content": format!("cold exact sentinel {}", "x".repeat(4_096))
            }))
            .unwrap(),
            serde_json::to_vec(&json!({
                "role": "assistant",
                "message_id": "message-assistant",
                "timestamp": "2026-07-28T12:00:02Z",
                "content": "bounded assistant response"
            }))
            .unwrap(),
        ];
        let mut messages = Vec::new();
        for record in &records {
            messages.extend_from_slice(record);
            messages.push(b'\n');
        }
        fs::write(session.join("messages.jsonl"), messages).unwrap();
        (temp, root, records)
    }

    fn imported_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 28, 12, 30, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn cold_scan_emits_stable_bounded_documents_and_exact_grouped_hydration() {
        let (_temp, root, records) = fixture();
        let first = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
        let second =
            scan_mistral_vibe_source_backed(&root, imported_at() + chrono::Duration::hours(1))
                .unwrap();

        assert_eq!(first.leaves.len(), 1);
        let leaf = &first.leaves[0];
        assert_eq!(leaf.source.counts().complete_records, 2);
        assert_eq!(leaf.source.counts().retained_records, 2);
        assert_eq!(leaf.source.counts().indexed_documents, 2);
        assert_eq!(leaf.documents.len(), 2);
        assert_eq!(
            leaf.documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>(),
            second.leaves[0]
                .documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            leaf.documents[0].session_id,
            second.leaves[0].documents[0].session_id
        );
        assert_eq!(
            leaf.documents[0].body.chars().count(),
            MAX_BODY_PREVIEW_CHARS
        );
        assert!(leaf
            .documents
            .iter()
            .all(|document| !document.body.contains("metadata-only-sentinel")));
        assert!(leaf.documents.iter().all(|document| {
            document.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
                && document
                    .locator
                    .certified_source_revision_digest()
                    .is_some()
        }));

        let requests = leaf
            .documents
            .iter()
            .map(|document| {
                EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
            })
            .collect::<Vec<_>>();
        let session_request =
            SessionHydrationRequest::new(leaf.documents[0].session_id, requests).unwrap();
        let hydrated = first.resolver.hydrate_session(&session_request).unwrap();
        assert_eq!(hydrated.len(), records.len());
        for (hydrated, expected) in hydrated.iter().zip(records) {
            assert_eq!(
                hydrated.provider_bytes.strip_suffix(b"\n").unwrap(),
                expected
            );
        }
    }

    #[test]
    fn metadata_mutation_invalidates_exact_hydration_without_changing_ids() {
        let (_temp, root, _) = fixture();
        let before = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
        let document = &before.leaves[0].documents[0];
        let request =
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
        assert!(before.resolver.hydrate_event(&request).is_ok());

        let metadata_path = root.join("session-alpha/meta.json");
        let metadata = fs::read_to_string(&metadata_path)
            .unwrap()
            .replace("metadata-only-sentinel-a", "metadata-only-sentinel-b");
        fs::write(metadata_path, metadata).unwrap();

        let failure = before.resolver.hydrate_event(&request).unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

        let after = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
        assert_eq!(
            before.leaves[0]
                .documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>(),
            after.leaves[0]
                .documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );
        assert_ne!(
            before.leaves[0].documents[0]
                .locator
                .certified_source_revision_digest(),
            after.leaves[0].documents[0]
                .locator
                .certified_source_revision_digest()
        );
    }
}

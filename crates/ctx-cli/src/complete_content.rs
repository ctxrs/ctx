use std::{collections::BTreeMap, io, path::PathBuf};

use clap::ValueEnum;
use ctx_history_capture::complete_content::{
    jsonl::JsonlCompleteContentResolver, sqlite::SqliteCompleteContentResolver,
    structured::StructuredCompleteContentResolver, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolverRegistry, CompleteMessageRequest,
    PersistedCompleteContentLocatorV1, SourceSnapshot, COMPLETE_CONTENT_LOCATOR_METADATA_KEY,
};
use ctx_history_core::{Event, EventRole, EventType};
use ctx_history_store::Store;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::transcript::event_content;

const INDEXED_MESSAGE_LIMIT_CHARS: usize = 16_000;
pub(crate) const CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct SerializedByteCounter {
    bytes: usize,
}

impl io::Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ContentPolicy {
    Indexed,
    Complete,
}

impl ContentPolicy {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Complete => "complete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentOrigin {
    CtxIndex,
    ProviderSource,
}

impl ContentOrigin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CtxIndex => "ctx_index",
            Self::ProviderSource => "provider_source",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EventContentOutcome {
    pub(crate) requested: ContentPolicy,
    pub(crate) complete: bool,
    pub(crate) origin: ContentOrigin,
    pub(crate) stored_truncated: bool,
    pub(crate) source_verified: bool,
}

impl EventContentOutcome {
    pub(crate) fn as_json(&self) -> Value {
        json!({
            "requested": self.requested.as_str(),
            "complete": self.complete,
            "origin": self.origin.as_str(),
            "stored_truncated": self.stored_truncated,
            "source_verified": self.source_verified,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedEventContent {
    pub(crate) text: String,
    pub(crate) outcome: EventContentOutcome,
    pub(crate) complete_content_available: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedContent {
    requested: ContentPolicy,
    events: BTreeMap<Uuid, ResolvedEventContent>,
}

impl ResolvedContent {
    pub(crate) fn requested(&self) -> ContentPolicy {
        self.requested
    }

    pub(crate) fn event(&self, event: &Event) -> Option<&ResolvedEventContent> {
        self.events.get(&event.id)
    }
}

pub(crate) fn default_resolver_registry() -> CompleteContentResolverRegistry {
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(JsonlCompleteContentResolver::new());
    registry.register(SqliteCompleteContentResolver::new());
    registry.register(StructuredCompleteContentResolver::new());
    registry
}

pub(crate) fn enforce_complete_content_output_limit(
    policy: ContentPolicy,
    serialized_output_bytes: usize,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    if policy == ContentPolicy::Complete && serialized_output_bytes > output_limit_bytes {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentTooLarge,
            event_id,
        ));
    }
    Ok(())
}

pub(crate) fn enforce_complete_content_cli_output_limit(
    policy: ContentPolicy,
    rendered_output: &str,
    writes_stdout: bool,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let serialized_output_bytes = rendered_output.len().saturating_add(usize::from(
        writes_stdout && !rendered_output.ends_with('\n'),
    ));
    enforce_complete_content_output_limit(
        policy,
        serialized_output_bytes,
        output_limit_bytes,
        event_id,
    )
}

pub(crate) fn serialized_json_line_bytes(value: &Value) -> serde_json::Result<usize> {
    let mut counter = SerializedByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes.saturating_add(1))
}

pub(crate) fn resolve_event_contents(
    store: &Store,
    events: &[&Event],
    policy: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<ResolvedContent, CompleteContentError> {
    resolve_event_contents_with_registry(
        store,
        events,
        policy,
        output_limit_bytes,
        &default_resolver_registry(),
    )
}

pub(crate) fn resolve_event_contents_with_registry(
    store: &Store,
    events: &[&Event],
    policy: ContentPolicy,
    output_limit_bytes: usize,
    registry: &CompleteContentResolverRegistry,
) -> Result<ResolvedContent, CompleteContentError> {
    let mut resolved = events
        .iter()
        .map(|event| {
            let retention = message_retention(event);
            let stored_truncated = !matches!(&retention, MessageRetention::Complete);
            let complete_content_available =
                matches!(&retention, MessageRetention::Eligible { .. });
            (
                event.id,
                ResolvedEventContent {
                    text: event_content(event),
                    outcome: EventContentOutcome {
                        requested: policy,
                        complete: !stored_truncated,
                        origin: ContentOrigin::CtxIndex,
                        stored_truncated,
                        source_verified: false,
                    },
                    complete_content_available,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if policy == ContentPolicy::Indexed {
        return Ok(ResolvedContent {
            requested: policy,
            events: resolved,
        });
    }

    let mut grouped = BTreeMap::<Uuid, Vec<CompleteMessageRequest>>::new();
    for event in events {
        let (indexed_text, indexed_limit_chars) = match message_retention(event) {
            MessageRetention::Complete => continue,
            MessageRetention::Eligible {
                indexed_text,
                indexed_limit_chars,
            } => (indexed_text, indexed_limit_chars),
            MessageRetention::PolicyBounded => continue,
            MessageRetention::IneligibleTruncated => {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::HydrationUnsupported,
                    event.id,
                ));
            }
        };
        let request = complete_message_request(store, event, indexed_text, indexed_limit_chars)?;
        let source_id = event.capture_source_id.ok_or_else(|| {
            CompleteContentError::new(CompleteContentErrorKind::HydrationUnsupported, event.id)
        })?;
        grouped.entry(source_id).or_default().push(request);
    }

    for requests in grouped.values_mut() {
        requests.sort_by_key(|request| {
            (
                request.source_record_ordinal,
                request.source_record_subrecord_index,
            )
        });
        for message in registry.resolve(requests)? {
            let Some(content) = resolved.get_mut(&message.event_id) else {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    message.event_id,
                ));
            };
            content.text = message.text;
            content.outcome = EventContentOutcome {
                requested: policy,
                complete: true,
                origin: ContentOrigin::ProviderSource,
                stored_truncated: true,
                source_verified: true,
            };
        }
    }

    let mut output_bytes = 0usize;
    for event in events {
        let content = resolved.get(&event.id).ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event.id,
            )
        })?;
        output_bytes = output_bytes.saturating_add(content.text.len());
        if output_bytes > output_limit_bytes {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentTooLarge,
                event.id,
            ));
        }
    }
    Ok(ResolvedContent {
        requested: policy,
        events: resolved,
    })
}

fn complete_message_request(
    store: &Store,
    event: &Event,
    indexed_text: String,
    indexed_limit_chars: usize,
) -> Result<CompleteMessageRequest, CompleteContentError> {
    let fail = |kind| CompleteContentError::new(kind, event.id);
    let source_id = event
        .capture_source_id
        .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?;
    let source = store
        .get_capture_source(source_id)
        .map_err(|_| fail(CompleteContentErrorKind::HydrationUnsupported))?;
    let raw_source_path = source
        .descriptor
        .raw_source_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?;
    let source_format = source
        .descriptor
        .source_format
        .clone()
        .or_else(|| metadata_string(&event.sync.metadata, "source_format"))
        .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?;
    let persisted = event
        .sync
        .metadata
        .get(COMPLETE_CONTENT_LOCATOR_METADATA_KEY)
        .and_then(PersistedCompleteContentLocatorV1::from_metadata_value)
        .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?;
    let expected_hash_authority =
        match metadata_string(&event.sync.metadata, "provider_event_hash_authority").as_deref() {
            Some("provider_supplied") => CompleteContentHashAuthority::ProviderSupplied,
            Some("normalized_payload_fallback") => {
                CompleteContentHashAuthority::NormalizedPayloadFallback
            }
            _ => return Err(fail(CompleteContentErrorKind::HydrationUnsupported)),
        };
    Ok(CompleteMessageRequest {
        event_id: event.id,
        provider: source.descriptor.provider,
        source_format,
        raw_source_path,
        source_root: source.descriptor.source_root.map(PathBuf::from),
        source_identity: source.descriptor.source_identity,
        source_family: Some(persisted.family()),
        source_locator: persisted.source_locator(),
        source_snapshot: source_snapshot(&source.sync.metadata),
        provider_session_id: event
            .session_id
            .and_then(|id| store.get_session(id).ok())
            .and_then(|session| session.external_session_id),
        source_record_ordinal: metadata_u64(&event.sync.metadata, "source_record_ordinal")
            .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?,
        source_record_subrecord_index: metadata_u64(
            &event.sync.metadata,
            "source_record_subrecord_index",
        )
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?,
        expected_provider_event_hash: metadata_string(&event.sync.metadata, "provider_event_hash")
            .ok_or_else(|| fail(CompleteContentErrorKind::HydrationUnsupported))?,
        expected_hash_authority,
        expected_native_record_id: Some(persisted.native_record_id().to_owned()),
        expected_record_digest: Some(persisted.record_sha256().clone()),
        expected_body_digest: Some(persisted.body_sha256().clone()),
        indexed_text,
        indexed_limit_chars,
    })
}

enum MessageRetention {
    Complete,
    Eligible {
        indexed_text: String,
        indexed_limit_chars: usize,
    },
    PolicyBounded,
    IneligibleTruncated,
}

fn message_retention(event: &Event) -> MessageRetention {
    let ordinary_message = event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        );
    if policy_bounded_output(event) {
        return MessageRetention::PolicyBounded;
    }
    for (retention_pointer, text_pointer) in [
        ("/body/text_retention", "/body/text"),
        ("/body/body/text_retention", "/body/body/text"),
        ("/text_retention", "/text"),
    ] {
        let Some(retention) = event.payload.pointer(retention_pointer) else {
            continue;
        };
        if retention.get("truncated").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        if !ordinary_message {
            return MessageRetention::PolicyBounded;
        }
        if retention.get("omission_applied").and_then(Value::as_bool) == Some(true) {
            return MessageRetention::IneligibleTruncated;
        }
        let Some(limit) = retention
            .get("limit_chars")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            return MessageRetention::IneligibleTruncated;
        };
        if limit != INDEXED_MESSAGE_LIMIT_CHARS {
            return MessageRetention::IneligibleTruncated;
        }
        let Some(text) = event.payload.pointer(text_pointer).and_then(Value::as_str) else {
            return MessageRetention::IneligibleTruncated;
        };
        return MessageRetention::Eligible {
            indexed_text: text.to_owned(),
            indexed_limit_chars: limit,
        };
    }
    MessageRetention::Complete
}

fn policy_bounded_output(event: &Event) -> bool {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return false;
    }

    [
        Some(&event.payload),
        event.payload.pointer("/body"),
        event.payload.pointer("/body/body"),
    ]
    .into_iter()
    .flatten()
    .any(|body| {
        body.get("output_truncated").and_then(Value::as_bool) == Some(true)
            || body.get("truncated").and_then(Value::as_bool) == Some(true)
            || body.get("output_retention").and_then(Value::as_str) == Some("metadata_only")
    })
}

pub(crate) fn source_snapshot(metadata: &Value) -> SourceSnapshot {
    SourceSnapshot {
        size_bytes: metadata_u64(metadata, "last_imported_size_bytes"),
        modified_at_ms: metadata
            .get("last_imported_modified_at_ms")
            .and_then(Value::as_i64),
        sha256: metadata_string(metadata, "last_imported_sha256"),
    }
}

fn metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn metadata_u64(metadata: &Value, key: &str) -> Option<u64> {
    metadata.get(key).and_then(Value::as_u64)
}

pub(crate) fn complete_content_error_json(error: &CompleteContentError) -> Value {
    json!({
        "error": error.kind.as_str(),
        "error_code": error.kind.as_str(),
        "ctx_event_id": error.event_id,
        "retryable": error.retryable,
        "remediation": format!("ctx locate event {}", error.event_id),
    })
}

#[cfg(test)]
mod tests;

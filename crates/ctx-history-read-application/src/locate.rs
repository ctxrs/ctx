use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, SourceKey};
use ctx_history_index_query::{CoreEventRecord, EventRecord, SessionRecord};
use serde_json::{json, Value};

use crate::generation::PinnedGenerationRead;
use crate::json::compact_json;
use crate::{
    reference_needs_retained_peer, resolve_core_event_with_refs, resolve_show_session_with_refs,
    timestamp_json, CompactPresentationProjection, GenerationReadError, GenerationReadPort,
    GenerationReadReceipt, GenerationReadRequest, GenerationReadTarget, PinnedHistoryQuery,
    RetainedPeerRead,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateRequest {
    Event {
        selector: String,
    },
    Session {
        selector: Option<String>,
        provider_session_id: Option<String>,
        provider: Option<CaptureProvider>,
        provider_key: Option<String>,
        source_id: Option<String>,
    },
}

#[derive(Debug)]
pub enum LocateResult {
    Event(Box<CoreEventRecord>),
    Session {
        session: Box<SessionRecord>,
        first_event: Box<EventRecord>,
    },
}

impl PinnedHistoryQuery<'_> {
    pub(crate) fn locate(&self, request: &LocateRequest) -> Result<LocateResult> {
        match request {
            LocateRequest::Event { selector } => {
                resolve_core_event_with_refs(&self.references, selector)
                    .map(|event| LocateResult::Event(Box::new(event)))
            }
            LocateRequest::Session {
                selector,
                provider_session_id,
                provider,
                provider_key,
                source_id,
            } => {
                let session = resolve_show_session_with_refs(
                    &self.references,
                    selector.as_deref(),
                    provider_session_id.as_deref(),
                    *provider,
                    provider_key.as_deref(),
                    source_id.as_deref(),
                )?;
                let first_event = self
                    .index
                    .events_for_session(session.session_id.as_uuid())?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        anyhow!(
                            "session {} has no event in the pinned Core generation",
                            session.session_id
                        )
                    })?;
                Ok(LocateResult::Session {
                    session: Box::new(session),
                    first_event: Box::new(first_event),
                })
            }
        }
    }
}

pub struct LocateApplicationRequest {
    pub request: LocateRequest,
    pub generation_target: GenerationReadTarget,
    pub compact_projection: bool,
}

impl LocateApplicationRequest {
    fn retained_peer_read(&self) -> RetainedPeerRead {
        let selector_needs_peer = match &self.request {
            LocateRequest::Event { selector } => reference_needs_retained_peer(selector),
            LocateRequest::Session { selector, .. } => selector
                .as_deref()
                .is_some_and(reference_needs_retained_peer),
        };
        if self.compact_projection || selector_needs_peer {
            RetainedPeerRead::IfAvailable
        } else {
            RetainedPeerRead::Omit
        }
    }
}

#[derive(Debug)]
pub enum LocateApplicationError<GenerationError> {
    Generation(GenerationReadError<GenerationError>),
    Query(anyhow::Error),
    Projection(anyhow::Error),
}

pub struct LocateApplicationResult {
    generation: PinnedGenerationRead,
    pub result: LocateResult,
    pub read_model: Value,
    pub compact_read_model: Option<Value>,
}

impl LocateApplicationResult {
    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        self.generation.receipt()
    }

    pub fn into_read_models(self) -> (Value, Option<Value>) {
        (self.read_model, self.compact_read_model)
    }
}

pub fn execute_locate<Generation: GenerationReadPort>(
    request: LocateApplicationRequest,
    generation_port: &mut Generation,
) -> std::result::Result<LocateApplicationResult, LocateApplicationError<Generation::Error>> {
    let retained_peer = request.retained_peer_read();
    let LocateApplicationRequest {
        request,
        generation_target,
        compact_projection,
    } = request;
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target: generation_target,
            retained_peer,
        },
    )
    .map_err(LocateApplicationError::Generation)?;
    let result = PinnedHistoryQuery::new(generation.index(), generation.retained_peer())
        .locate(&request)
        .map_err(LocateApplicationError::Query)?;
    let read_model = locate_read_model(&result);
    let compact_read_model = compact_projection
        .then(|| {
            CompactPresentationProjection::new(generation.index(), generation.retained_peer())
                .project(&read_model)
        })
        .transpose()
        .map_err(LocateApplicationError::Projection)?;
    Ok(LocateApplicationResult {
        generation,
        result,
        read_model,
        compact_read_model,
    })
}

pub fn locate_read_model(result: &LocateResult) -> Value {
    match result {
        LocateResult::Session {
            session,
            first_event,
        } => locate_session_read_model(session, first_event),
        LocateResult::Event(event) => locate_event_read_model(event),
    }
}

fn locate_session_read_model(session: &SessionRecord, first_event: &EventRecord) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_location",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_key": session.provider_key,
        "source_id": session.source_id,
        "provider_session_id": session.provider_session_id,
        "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": session.root_session_id.map(|id| id.as_uuid()),
        "started_at": timestamp_json(session.first_occurred_at_unix_ms),
        "source": source_read_model(&first_event.source),
    }))
}

fn locate_event_read_model(event: &CoreEventRecord) -> Value {
    let (provider_key, source_id) = event
        .custom_source_identity()
        .map_or((None, None), |(provider_key, source_id)| {
            (Some(provider_key), Some(source_id))
        });
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_location",
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_key": provider_key,
        "source_id": source_id,
        "provider_session_id": event.provider_session_id,
        "provider_event_id": event.native_event_id,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "source": source_read_model(&event.source),
    }))
}

fn source_read_model(source: &SourceKey) -> Value {
    json!({
        "ctx_source_id": source.identity().as_uuid(),
        "source_format": source.source_format(),
        "schema_variant": source.schema_variant(),
        "provider_identity_version": source.provider_identity_version(),
    })
}

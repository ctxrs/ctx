use std::path::PathBuf;

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, NativeRecordCoordinate, SourceRecordLocator};
use ctx_history_index::{EventRecord, SessionRecord};
use serde_json::{json, Value};

use crate::{
    local_usage::{CliUsage, ResultObservationAction},
    output::{compact_json, print_json},
    provider_args::ProviderArg,
    transcript::{
        print_locate_event_text, print_locate_session_text, provider_resume_json, write_output,
    },
    LocateArgs, LocateTarget,
};

use super::{
    render::{
        locate_event_text_output_bytes, locate_session_text_output_bytes, pretty_json_stdout_bytes,
        render_locate_event_availability_text, timestamp_json,
    },
    shared::{
        event_source_json, open_index, resolve_event, resolve_session, session_source_json,
        source_path_exists, validate_ctx_id, validate_session_selector,
    },
};

pub(crate) fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    local_usage: &mut CliUsage,
) -> Result<()> {
    validate_locate_target(&args.target)?;
    let index = open_index(&data_root)?;
    let (value, json_output) = match args.target {
        LocateTarget::Session(args) => {
            let provider = args.provider.map(ProviderArg::capture_provider);
            let session = match (args.id.as_deref(), args.provider_session.as_deref()) {
                (Some(id), None) => resolve_session(&index, id)?,
                (None, Some(provider_session_id)) => {
                    let matches = index.sessions_by_provider_session_id(
                        provider_session_id,
                        provider.map(CaptureProvider::as_str),
                    )?;
                    match matches.as_slice() {
                        [] => {
                            return Err(anyhow!(
                                "provider session {provider_session_id:?} was not found in the source-backed Core generation"
                            ));
                        }
                        [session] => session.clone(),
                        matches => {
                            return Err(anyhow!(
                                "provider session {provider_session_id:?} is ambiguous; first matches are {} and {}; pass --provider or a ctx session ID",
                                matches[0].session_id,
                                matches[1].session_id
                            ));
                        }
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(anyhow!(
                        "pass either a ctx session ID or --provider-session, not both"
                    ));
                }
                (None, None) => {
                    return Err(anyhow!(
                        "source-backed session lookup requires a ctx session ID or --provider-session"
                    ));
                }
            };
            if let Some(provider) = provider {
                if session.provider != provider.as_str() {
                    return Err(anyhow!(
                        "source-backed session {} belongs to provider {}, not {}",
                        session.session_id,
                        session.provider,
                        provider
                    ));
                }
            }
            let first_event = index
                .events_for_session(session.session_id.as_uuid())?
                .into_iter()
                .next();
            let value = locate_session_value(&session, first_event.as_ref());
            (value, args.format.is_json())
        }
        LocateTarget::Event(args) => {
            let event = resolve_event(&index, &args.id)?;
            let value = locate_event_value(&event);
            (value, args.format.is_json())
        }
    };
    let content_bytes = serde_json::to_vec(&value)?.len();
    let output_bytes = if json_output {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else if value["target"] == "session" {
        let output_bytes = locate_session_text_output_bytes(&value);
        print_locate_session_text(&value)?;
        output_bytes
    } else {
        let output_bytes = locate_event_text_output_bytes(&value);
        print_locate_event_text(&value)?;
        let availability = render_locate_event_availability_text(&value);
        if !availability.is_empty() {
            write_output(availability, None)?;
        }
        output_bytes
    };
    local_usage.set_result_observation(ResultObservationAction::Locate, 1, 0, content_bytes);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

pub(super) fn validate_locate_target(target: &LocateTarget) -> Result<()> {
    match target {
        LocateTarget::Session(args) => {
            validate_session_selector(args.id.as_deref(), args.provider_session.as_deref())
        }
        LocateTarget::Event(args) => validate_ctx_id(&args.id, "event").map(|_| ()),
    }
}

pub(super) fn locate_session_value(
    session: &SessionRecord,
    first_event: Option<&EventRecord>,
) -> Value {
    let provider = session
        .provider
        .parse::<CaptureProvider>()
        .unwrap_or(CaptureProvider::Unknown);
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_location",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": session.root_session_id.as_uuid(),
        "agent_type": session.agent_type,
        "started_at": timestamp_json(session.first_occurred_at_unix_ms),
        "source": session_source_json(session, first_event),
        "resume": provider_resume_json(provider, session.provider_session_id.as_deref()),
    }))
}

pub(super) fn locate_event_value(event: &EventRecord) -> Value {
    let provider = event
        .provider
        .parse::<CaptureProvider>()
        .unwrap_or(CaptureProvider::Unknown);
    let source_exists = source_path_exists(event.source_path.as_deref());
    let (source_family, locator_kind) = locator_kind(&event.locator);
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_location",
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "source": event_source_json(event),
        "source_record": safe_source_record_json(&event.locator),
        "complete_content": {
            "locator_available": true,
            "available": source_exists.unwrap_or(false),
            "source_authority": "provider",
            "source_family": source_family,
            "locator_kind": locator_kind,
        },
        "resume": provider_resume_json(provider, event.provider_session_id.as_deref()),
    }))
}

fn locator_kind(locator: &SourceRecordLocator) -> (&'static str, &'static str) {
    match locator.coordinate() {
        NativeRecordCoordinate::Jsonl { .. } => ("jsonl", "jsonl"),
        NativeRecordCoordinate::ProviderSqlite { .. } => ("sqlite", "provider_sqlite"),
        NativeRecordCoordinate::Document { .. } => ("document", "document"),
        NativeRecordCoordinate::TreeRecord { .. } => ("tree", "tree_record"),
        NativeRecordCoordinate::ProviderNative { .. } => ("provider_native", "provider_native"),
    }
}

fn safe_source_record_json(locator: &SourceRecordLocator) -> Value {
    match locator.coordinate() {
        NativeRecordCoordinate::Jsonl {
            byte_offset,
            byte_length,
            physical_ordinal,
            ..
        } => json!({
            "kind": "jsonl",
            "byte_offset": byte_offset,
            "byte_length": byte_length,
            "ordinal": physical_ordinal,
        }),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation, ..
        } => json!({
            "kind": "provider_sqlite",
            "logical_relation": logical_relation,
        }),
        NativeRecordCoordinate::Document { json_pointer, .. } => compact_json(json!({
            "kind": "document",
            "json_pointer": json_pointer,
        })),
        NativeRecordCoordinate::TreeRecord { .. } => json!({
            "kind": "tree_record",
        }),
        NativeRecordCoordinate::ProviderNative { namespace, .. } => json!({
            "kind": "provider_native",
            "namespace": namespace,
        }),
    }
}

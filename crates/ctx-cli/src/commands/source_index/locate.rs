use std::path::PathBuf;

use anyhow::{anyhow, Result};
use ctx_history_core::SourceKey;
use ctx_history_index::{CoreEventRecord, EventRecord, SessionRecord};
use serde_json::{json, Value};

use crate::{
    commands::locate::{LocateArgs, LocateTarget},
    local_usage::{CliUsage, ResultObservationAction},
    output::{compact_json, print_json},
    ui::{canonical_human_output_bytes, Ui},
};

use super::{
    compact_presentation::{reference_needs_retained_peer, CompactPresentation},
    compact_ref::CompactRefResolver,
    render::{pretty_json_stdout_bytes, render_locate_document, timestamp_json},
    shared::{
        index_root, open_index, resolve_core_event_with_refs, validate_ctx_id,
        validate_session_selector,
    },
    show::resolve_show_session_with_refs,
};

pub(crate) fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    validate_locate_target(&args.target)?;
    let index = open_index(&data_root)?;
    let (value, compact_value, json_output) = match args.target {
        LocateTarget::Session(args) => {
            let json_output = args.format.is_json();
            let compact = CompactPresentation::open_if_needed(
                &index,
                &index_root(&data_root),
                !json_output
                    || args
                        .id
                        .as_deref()
                        .is_some_and(reference_needs_retained_peer),
            )?;
            let pinned_references = CompactRefResolver::new(&index, None);
            let input_resolver = compact
                .as_ref()
                .map(CompactPresentation::resolver)
                .unwrap_or(pinned_references);
            let provider = args.provider.map(|provider| provider.capture_provider());
            let session = resolve_show_session_with_refs(
                &input_resolver,
                args.id.as_deref(),
                args.provider_session.as_deref(),
                provider,
            )?;
            let first_event = index
                .events_for_session(session.session_id.as_uuid())?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    anyhow!(
                        "session {} has no event in the pinned Core generation",
                        session.session_id
                    )
                })?;
            let value = locate_session_value(&session, &first_event);
            let compact_value = (!json_output)
                .then(|| {
                    compact
                        .as_ref()
                        .expect("human locate opens compact presentation")
                        .project(&value)
                })
                .transpose()?;
            (value, compact_value, json_output)
        }
        LocateTarget::Event(args) => {
            let json_output = args.format.is_json();
            let compact = CompactPresentation::open_if_needed(
                &index,
                &index_root(&data_root),
                !json_output || reference_needs_retained_peer(&args.id),
            )?;
            let pinned_references = CompactRefResolver::new(&index, None);
            let input_resolver = compact
                .as_ref()
                .map(CompactPresentation::resolver)
                .unwrap_or(pinned_references);
            let event = resolve_core_event_with_refs(&input_resolver, &args.id)?;
            let value = locate_event_value(&event);
            let compact_value = (!json_output)
                .then(|| {
                    compact
                        .as_ref()
                        .expect("human locate opens compact presentation")
                        .project(&value)
                })
                .transpose()?;
            (value, compact_value, json_output)
        }
    };

    let content_bytes = serde_json::to_vec(&value)?.len();
    let render_value = compact_value.as_ref().unwrap_or(&value);
    let output_bytes = if json_output {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let document = render_locate_document(render_value, ui.stdout_context());
        let output_bytes =
            canonical_human_output_bytes(|context| render_locate_document(render_value, context));
        ui.write_stdout(&document)?;
        output_bytes
    };
    local_usage.set_result_observation(ResultObservationAction::Locate, 1, 0, content_bytes);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

fn validate_locate_target(target: &LocateTarget) -> Result<()> {
    match target {
        LocateTarget::Session(args) => {
            validate_session_selector(args.id.as_deref(), args.provider_session.as_deref())
        }
        LocateTarget::Event(args) => validate_ctx_id(&args.id, "event").map(|_| ()),
    }
}

fn locate_session_value(session: &SessionRecord, first_event: &EventRecord) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_location",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": session.root_session_id.as_uuid(),
        "started_at": timestamp_json(session.first_occurred_at_unix_ms),
        "source": source_value(&first_event.source),
    }))
}

fn locate_event_value(event: &CoreEventRecord) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_location",
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "provider_event_id": event.native_event_id,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "source": source_value(&event.source),
    }))
}

fn source_value(source: &SourceKey) -> Value {
    json!({
        "ctx_source_id": source.identity().as_uuid(),
        "source_format": source.source_format(),
        "schema_variant": source.schema_variant(),
        "provider_identity_version": source.provider_identity_version(),
    })
}

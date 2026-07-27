use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::decode_native_path_committed_cursor;
use serde::{Deserialize, Serialize};

use crate::provider::importer::CertifiedProviderCursor;
use crate::released_jsonl_cursor::released_jsonl_position_offset;
use crate::{CaptureError, Result};

use super::{
    reader::{direct_jsonl_prefix_sha256, direct_jsonl_source_revision},
    DirectJsonlCheckpoint, DirectJsonlFileObservation, DirectJsonlSession,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};
use crate::provider::providers::native_jsonl::normalization::native_jsonl_session_metadata_from_normalized_header;

const DIRECT_JSONL_CURSOR_VERSION: u32 = 1;
const RELEASED_NATIVE_JSONL_PARSER_REVISION: u32 = 4;
const RELEASED_NATIVE_JSONL_POLICY_REVISION: u32 = 7;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectJsonlCursorWire {
    version: u32,
    kind: String,
    checkpoint: DirectJsonlCheckpoint,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedNativeJsonlParserCheckpoint {
    session: Option<ReleasedNativeJsonlSessionCheckpoint>,
    next_ordinal: u64,
    #[serde(rename = "accepted_captures")]
    _accepted_source_records: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    #[serde(default)]
    rejected_records: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedNativeJsonlSessionCheckpoint {
    native_session_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    agent_type: ctx_history_core::AgentType,
    status: ctx_history_core::SessionStatus,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    #[serde(rename = "header_anchor")]
    _header_anchor: ReleasedNativeJsonlHeaderAnchor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedNativeJsonlHeaderAnchor {
    #[serde(rename = "ordinal")]
    _ordinal: u64,
    #[serde(rename = "start")]
    _start: u64,
    #[serde(rename = "end")]
    _end: u64,
    #[serde(rename = "payload_sha256")]
    _payload_sha256: [u8; 32],
}

pub(crate) enum DirectJsonlCursorDecode {
    Native(DirectJsonlCheckpoint),
    Migrated(DirectJsonlCheckpoint),
    Reset,
}

pub(crate) fn encode_direct_jsonl_cursor(checkpoint: &DirectJsonlCheckpoint) -> Result<String> {
    Ok(serde_json::to_string(&DirectJsonlCursorWire {
        version: DIRECT_JSONL_CURSOR_VERSION,
        kind: "direct-native-jsonl".to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

pub(crate) fn decode_direct_jsonl_cursor(
    encoded_store_cursor: &str,
    provider: CaptureProvider,
    source_format: &str,
    path: &Path,
    observation: &DirectJsonlFileObservation,
) -> Result<DirectJsonlCursorDecode> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    if let Ok(wire) = serde_json::from_str::<DirectJsonlCursorWire>(&encoded) {
        if wire.version == DIRECT_JSONL_CURSOR_VERSION
            && wire.kind == "direct-native-jsonl"
            && wire.checkpoint.is_supported_for(provider, source_format)
        {
            return Ok(DirectJsonlCursorDecode::Native(wire.checkpoint));
        }
        return Ok(DirectJsonlCursorDecode::Reset);
    }
    migrate_released_cursor(&encoded, provider, source_format, path, observation)
}

pub(crate) fn decode_direct_jsonl_native_cursor(
    encoded_store_cursor: &str,
    provider: CaptureProvider,
    source_format: &str,
) -> Option<DirectJsonlCheckpoint> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let wire = serde_json::from_str::<DirectJsonlCursorWire>(&encoded).ok()?;
    (wire.version == DIRECT_JSONL_CURSOR_VERSION
        && wire.kind == "direct-native-jsonl"
        && wire.checkpoint.is_supported_for(provider, source_format))
    .then_some(wire.checkpoint)
}

pub(crate) fn direct_jsonl_cursor_matches_publication(
    encoded_store_cursor: &str,
    publication_id: &str,
    provider_cursor: &str,
) -> bool {
    decode_native_path_committed_cursor(encoded_store_cursor).is_ok_and(|cursor| {
        cursor.publication_id() == publication_id && cursor.provider_cursor() == provider_cursor
    })
}

fn migrate_released_cursor(
    encoded: &str,
    provider: CaptureProvider,
    source_format: &str,
    path: &Path,
    observation: &DirectJsonlFileObservation,
) -> Result<DirectJsonlCursorDecode> {
    if matches!(
        provider,
        CaptureProvider::Antigravity
            | CaptureProvider::CopilotCli
            | CaptureProvider::Qoder
            | CaptureProvider::Tabnine
            | CaptureProvider::Windsurf
    ) {
        return Ok(DirectJsonlCursorDecode::Reset);
    }
    let Some(released) = CertifiedProviderCursor::decode_if_certified(encoded)? else {
        return Ok(DirectJsonlCursorDecode::Reset);
    };
    if released.parser_revision() != RELEASED_NATIVE_JSONL_PARSER_REVISION
        || released.policy_revision() != RELEASED_NATIVE_JSONL_POLICY_REVISION
        || released.source_revision() != direct_jsonl_source_revision(observation)
    {
        return Ok(DirectJsonlCursorDecode::Reset);
    }
    let complete_prefix_end = released_jsonl_position_offset(released.native_position())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if complete_prefix_end > observation.length {
        return Ok(DirectJsonlCursorDecode::Reset);
    }
    let released_checkpoint: ReleasedNativeJsonlParserCheckpoint =
        released.parser_checkpoint().deserialize()?;
    let session = released_checkpoint.session.map(|session| {
        let is_subagent = session.parent_provider_session_id.is_some()
            || session.agent_type == ctx_history_core::AgentType::Subagent;
        DirectJsonlSession {
            native_session_id: session.native_session_id,
            provider_session_id: session.provider_session_id,
            root_provider_session_id: session.parent_provider_session_id.clone(),
            parent_provider_session_id: session.parent_provider_session_id,
            external_agent_id: session.external_agent_id,
            agent_type: session.agent_type,
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            status: session.status,
            started_at: session.started_at,
            ended_at: None,
            cwd: session.cwd,
            metadata: native_jsonl_session_metadata_from_normalized_header(
                provider,
                source_format,
                &serde_json::Value::Null,
                path,
            ),
        }
    });
    let canonical_path = std::fs::canonicalize(path)?;
    let checkpoint = DirectJsonlCheckpoint {
        version: DirectJsonlCheckpoint::VERSION,
        parser_revision: DIRECT_JSONL_NATIVEPATH_PARSER_REVISION,
        policy_revision: DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
        provider,
        source_format: source_format.to_owned(),
        source_path: canonical_path,
        source_observation: observation.clone(),
        complete_prefix_end,
        complete_prefix_sha256: direct_jsonl_prefix_sha256(path, complete_prefix_end)?,
        next_raw_ordinal: released_checkpoint.next_ordinal,
        accepted_events: released_checkpoint.accepted_events,
        accepted_file_touches: released_checkpoint.accepted_file_touches,
        rejected_records: released_checkpoint
            .rejected_records
            .max(released.rejected_records()),
        session,
        terminal: complete_prefix_end == observation.length,
    };
    Ok(DirectJsonlCursorDecode::Migrated(checkpoint))
}

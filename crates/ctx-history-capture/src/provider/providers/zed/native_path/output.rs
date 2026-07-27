use std::{path::Path, sync::Arc};

use ctx_history_core::{CaptureProvider, EventType};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    provider::native_ingestion::{
        process_pro_replay_only, NativePageAccounting, NativeProOutputPage, NativeProReplayPage,
        NativeSafeFrontier, NativeSourceIdentity, NATIVE_INGESTION_PAGE_MAX_BYTES,
        NATIVE_INGESTION_PAGE_MAX_UNITS,
    },
    OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator, ProOutputObservation,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition,
};

use super::{
    acquire_immutable_snapshot,
    dto::{ZedNativePage, ZedNativeSink},
    query::scan_zed_native_snapshot,
    staging::ZedNativeStaging,
    ZedNativeResult, ZedSnapshotAcquisition,
};

const ZED_OUTPUT_CURSOR_VERSION: u32 = 1;
const ZED_OUTPUT_PARSER_REVISION: &str = "zed-nativepath-output-v1";
const ZED_OUTPUT_PAGE_FIXED_BYTES: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ZedOutputFrontier {
    version: u32,
    next_output_ordinal: u64,
}

struct ZedOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    next_output_ordinal: u64,
}

pub(super) fn replay_zed_outputs_or_mark_behind(
    path: &Path,
    staging: &ZedNativeStaging,
    evidence_path: &Path,
    canonical_source_identity: &str,
    source_revision: &str,
    expected_snapshot_revision: &str,
    expected_capability_digest: &str,
    expected_source_integrity_digest: &str,
    expected_core_generation_digest: &str,
    sink: Option<&Arc<dyn ProOutputSink>>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_zed_outputs(
        path,
        staging,
        evidence_path,
        canonical_source_identity,
        source_revision,
        expected_snapshot_revision,
        expected_capability_digest,
        expected_source_integrity_digest,
        expected_core_generation_digest,
        sink.as_ref(),
    ) {
        sink.mark_behind(error);
    }
}

fn replay_zed_outputs(
    path: &Path,
    staging: &ZedNativeStaging,
    evidence_path: &Path,
    canonical_source_identity: &str,
    source_revision: &str,
    expected_snapshot_revision: &str,
    expected_capability_digest: &str,
    expected_source_integrity_digest: &str,
    expected_core_generation_digest: &str,
    sink: &dyn ProOutputSink,
) -> Result<(), ProOutputSinkError> {
    let snapshot = match acquire_immutable_snapshot(path).map_err(output_source_error)? {
        ZedSnapshotAcquisition::Acquired(snapshot) => *snapshot,
        ZedSnapshotAcquisition::Incomplete { .. } => {
            return Err(ProOutputSinkError::new(
                "zed_output_source_changed",
                "Zed output source changed while acquiring an immutable snapshot",
            ));
        }
    };
    if snapshot.snapshot_revision != expected_snapshot_revision {
        return Err(ProOutputSinkError::new(
            "zed_output_revision_mismatch",
            "Zed output replay no longer matches the committed Core generation",
        ));
    }
    let mut discard = ZedOutputCoreDiscard;
    let verification = scan_zed_native_snapshot(
        &snapshot.connection,
        &snapshot.physical_locator,
        &snapshot.snapshot_revision,
        &mut discard,
    )
    .map_err(output_source_error)?;
    if verification.capability_digest != expected_capability_digest
        || verification.source_integrity_digest != expected_source_integrity_digest
        || verification.core_generation_digest != expected_core_generation_digest
    {
        return Err(ProOutputSinkError::new(
            "zed_output_revision_mismatch",
            "Zed output replay bytes do not match the committed Core generation",
        ));
    }

    let evidence = Connection::open_with_flags(
        // This provider-private index contains decoded thread identities only.
        // Successful output bytes remain solely in the immutable source snapshot.
        evidence_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(output_sqlite_error)?;
    let hydration_sql =
        "SELECT rowid, id, updated_at, data_type, data FROM threads WHERE id = ?1 COLLATE BINARY";
    let mut hydrate = snapshot
        .connection
        .prepare(&hydration_sql)
        .map_err(output_sqlite_error)?;
    let mut identities = evidence
        .prepare(
            "SELECT id FROM output_threads
             ORDER BY id COLLATE BINARY",
        )
        .map_err(output_sqlite_error)?;
    let ids = identities
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(output_sqlite_error)?;
    for id in ids {
        let id = id.map_err(output_sqlite_error)?;
        let row = hydrate
            .query_row([&id], |row| {
                Ok(super::super::thread::ZedThreadRow {
                    rowid: row.get(0)?,
                    id: row.get(1)?,
                    updated_at: row.get(2)?,
                    data_type: row.get(3)?,
                    data: row.get(4)?,
                })
            })
            .map_err(output_sqlite_error)?;
        let (parent_thread_id, root_thread_id) = staging
            .session_relationship(&id)
            .map_err(output_source_error)?
            .ok_or_else(|| {
                ProOutputSinkError::new(
                    "zed_output_relationship",
                    "committed Zed session is absent from exact staged authority",
                )
            })?;
        replay_thread(
            path,
            source_revision,
            canonical_source_identity,
            parent_thread_id,
            root_thread_id,
            row,
            sink,
        )?;
    }
    if !snapshot
        .observed
        .revalidate(path)
        .map_err(output_source_error)?
    {
        return Err(ProOutputSinkError::new(
            "zed_output_source_changed",
            "Zed output source changed before replay completed",
        ));
    }
    Ok(())
}

struct ZedOutputCoreDiscard;

impl ZedNativeSink for ZedOutputCoreDiscard {
    fn push_page(&mut self, _page: ZedNativePage) -> ZedNativeResult<()> {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn replay_thread(
    path: &Path,
    source_revision: &str,
    canonical_source_identity: &str,
    parent_thread_id: Option<String>,
    root_thread_id: String,
    row: super::super::thread::ZedThreadRow,
    sink: &dyn ProOutputSink,
) -> Result<(), ProOutputSinkError> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Zed.as_str().to_owned(),
        namespace_id: canonical_source_identity.to_owned(),
        source_id: row.id.clone(),
    };
    let progress = sink.observe_source(&output_source).map_err(|error| error)?;
    if progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == source_revision
            && progress.parser_revision == ZED_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.terminal
    }) {
        return Ok(());
    }
    let mut state = output_state(output_source, progress, source_revision, sink)?;
    let decoded = super::super::event::decode_zed_thread_events(&row)
        .map_err(|error| ProOutputSinkError::new("zed_output_decode", error.to_string()))?;
    let mut observations = Vec::new();
    let mut page_bytes = ZED_OUTPUT_PAGE_FIXED_BYTES;
    let mut output_ordinal = 0_u64;
    for native in decoded.native_events(&row.id) {
        if native.event_type != EventType::ToolOutput {
            continue;
        }
        let current_ordinal = output_ordinal;
        output_ordinal = output_ordinal.checked_add(1).ok_or_else(|| {
            ProOutputSinkError::new("zed_output_frontier", "Zed output ordinal overflowed")
        })?;
        if current_ordinal < state.next_output_ordinal {
            continue;
        }
        let observation = output_observation(
            path,
            &row,
            &root_thread_id,
            parent_thread_id.as_deref(),
            current_ordinal,
            &native,
            decoded.event_occurred_at().timestamp_millis(),
        )?;
        let observation_bytes = estimated_observation_bytes(&observation);
        if observation_bytes.saturating_add(ZED_OUTPUT_PAGE_FIXED_BYTES)
            > NATIVE_INGESTION_PAGE_MAX_BYTES
        {
            return Err(ProOutputSinkError::new(
                "zed_output_page_bound",
                "one Zed output exceeds the bounded output replay page",
            ));
        }
        if !observations.is_empty()
            && (observations.len() >= NATIVE_INGESTION_PAGE_MAX_UNITS
                || page_bytes.saturating_add(observation_bytes) > NATIVE_INGESTION_PAGE_MAX_BYTES)
        {
            publish_output_page(
                canonical_source_identity,
                source_revision,
                false,
                page_bytes,
                std::mem::take(&mut observations),
                &mut state,
                sink,
            )?;
            page_bytes = ZED_OUTPUT_PAGE_FIXED_BYTES;
        }
        page_bytes = page_bytes.saturating_add(observation_bytes);
        observations.push(observation);
        state.next_output_ordinal = output_ordinal;
    }
    publish_output_page(
        canonical_source_identity,
        source_revision,
        true,
        page_bytes,
        observations,
        &mut state,
        sink,
    )
}

fn output_state(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    source_revision: &str,
    sink: &dyn ProOutputSink,
) -> Result<ZedOutputState, ProOutputSinkError> {
    let Some(progress) = progress else {
        return Ok(ZedOutputState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            next_output_ordinal: 0,
        });
    };
    let prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| ProOutputSinkError::new("zed_output_frontier", error.to_string()))?;
    let decoded = progress
        .cursor
        .as_ref()
        .filter(|cursor| cursor.version == ZED_OUTPUT_CURSOR_VERSION)
        .and_then(|cursor| serde_json::from_slice::<ZedOutputFrontier>(&cursor.payload).ok())
        .filter(|cursor| cursor.version == ZED_OUTPUT_CURSOR_VERSION);
    let can_resume = progress.observed_revision == source_revision
        && progress.parser_revision == ZED_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && decoded.is_some();
    if can_resume {
        return Ok(ZedOutputState {
            source,
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_frontier,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            next_output_ordinal: decoded.map_or(0, |cursor| cursor.next_output_ordinal),
        });
    }
    Ok(ZedOutputState {
        source,
        source_epoch: progress.source_epoch.checked_add(1).ok_or_else(|| {
            ProOutputSinkError::new("zed_output_epoch", "Zed output source epoch overflowed")
        })?,
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: prior_frontier,
        disposition: ProOutputSourceDisposition::Rewrite,
        next_output_ordinal: 0,
    })
}

fn publish_output_page(
    canonical_source_identity: &str,
    source_revision: &str,
    terminal: bool,
    page_bytes: usize,
    observations: Vec<ProOutputObservation>,
    state: &mut ZedOutputState,
    sink: &dyn ProOutputSink,
) -> Result<(), ProOutputSinkError> {
    let next_frontier = safe_frontier(state.next_output_ordinal)?;
    let expected_frontier = if state.disposition == ProOutputSourceDisposition::Rewrite {
        safe_frontier(0)?
    } else {
        state
            .expected_sink_frontier
            .clone()
            .unwrap_or(safe_frontier(0)?)
    };
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: source_revision.to_owned(),
        parser_revision: ZED_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let accounting = NativePageAccounting {
        logical_units: output.observations.len(),
        conservative_serialized_bytes: page_bytes,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Zed.as_str(), canonical_source_identity),
        expected_frontier,
        next_frontier.clone(),
        terminal,
        accounting,
        output,
    )
    .map_err(|error| ProOutputSinkError::new("zed_output_page_invalid", error.to_string()))?;
    if process_pro_replay_only(replay, sink).is_err() {
        // The shared output coordinator already marked this sink behind. Keep
        // Core successful and leave this exact frontier available for replay.
        return Ok(());
    }
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn output_observation(
    path: &Path,
    row: &super::super::thread::ZedThreadRow,
    root_thread_id: &str,
    parent_thread_id: Option<&str>,
    output_ordinal: u64,
    native: &super::super::event::ZedNativeEvent<'_>,
    occurred_at_unix_ms: i64,
) -> Result<ProOutputObservation, ProOutputSinkError> {
    let cursor = native.cursor();
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("zed:{}:output:{output_ordinal:010}", row.id),
            native_sequence: output_ordinal,
            native_record_id: Some(cursor.clone()),
            source_record_ordinal: u64::try_from(row.rowid).ok(),
            source_record_subrecord_index: Some(native.source_record_subrecord_index),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(occurred_at_unix_ms),
        associations: OutputAssociations {
            direct_session_id: row.id.clone(),
            root_session_id: root_thread_id.to_owned(),
            parent_session_id: parent_thread_id.map(str::to_owned),
            provider_session_id: Some(row.id.clone()),
            agent_id: Some("zed".to_owned()),
            repository: None,
        },
        call_id: super::super::event::zed_result_call_id(native.message),
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome: match super::super::event::zed_result_is_error(native.message) {
                Some(true) => OutputOutcome::Failure,
                Some(false) => OutputOutcome::Success,
                None => OutputOutcome::Unknown,
            },
            exit_code: None,
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "zed-sqlite-thread-result-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": path,
                "rowid": row.rowid,
                "thread_id": row.id,
                "cursor": cursor,
            }))
            .map_err(|error| ProOutputSinkError::new("zed_output_locator", error.to_string()))?,
        },
        content: super::super::event::zed_result_content(native.message)
            .unwrap_or_default()
            .into_bytes(),
    })
}

fn safe_frontier(next_output_ordinal: u64) -> Result<NativeSafeFrontier, ProOutputSinkError> {
    let bytes = serde_json::to_vec(&ZedOutputFrontier {
        version: ZED_OUTPUT_CURSOR_VERSION,
        next_output_ordinal,
    })
    .map_err(|error| ProOutputSinkError::new("zed_output_frontier", error.to_string()))?;
    NativeSafeFrontier::new(ZED_OUTPUT_CURSOR_VERSION, bytes)
        .map_err(|error| ProOutputSinkError::new("zed_output_frontier", error.to_string()))
}

fn estimated_observation_bytes(observation: &ProOutputObservation) -> usize {
    observation
        .content
        .len()
        .saturating_add(observation.locator.payload.len())
        .saturating_add(observation.coordinate.unit_key.len())
        .saturating_add(
            observation
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(512)
}

fn output_source_error(error: impl std::fmt::Display) -> ProOutputSinkError {
    ProOutputSinkError::new("zed_output_source", error.to_string())
}

fn output_sqlite_error(error: rusqlite::Error) -> ProOutputSinkError {
    ProOutputSinkError::new("zed_output_sqlite", error.to_string())
}

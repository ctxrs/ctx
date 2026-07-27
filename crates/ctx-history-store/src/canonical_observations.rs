//! Canonical payload construction for the local projection journal.
//!
//! This is intentionally not a detector result or a public version of a private
//! evidence envelope. It exposes only exact rows, actor relationships, source
//! observations, and citation coordinates already owned by the canonical Store.

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use ctx_history_core::{Confidence, FileChangeKind, VcsChangeKind};

mod helpers;
mod projection;
pub(crate) use helpers::canonical_semantic_digest;
use helpers::{
    json_byte_range, json_u64, nonnegative_u64, optional_uuid_column, parse_json_column,
    parse_uuid_column,
};
pub(crate) use projection::strip_local_complete_content_metadata;
use projection::take_result_evidence;
pub use projection::{
    CanonicalResultEvidence, CanonicalResultEvidenceKind, CanonicalResultIdentifier,
    CanonicalResultOutcome,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalByteRange {
    pub start: u64,
    pub end_exclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSourceObservation {
    pub byte_size: u64,
    pub modified_at_ms: i64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSource {
    pub id: Uuid,
    pub provider: String,
    pub path: Option<String>,
    pub format: Option<String>,
    pub root: Option<String>,
    pub identity: Option<String>,
    pub cwd: Option<String>,
    pub imported_observation: Option<CanonicalSourceObservation>,
    /// Source bytes the helper may inspect after revalidating the observation.
    pub permitted_bytes: Option<CanonicalByteRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalActor {
    /// The session that directly emitted the event.
    pub direct_session_id: Uuid,
    /// The owning top-level session, kept separate from the direct actor.
    pub root_session_id: Uuid,
    pub parent_session_id: Option<Uuid>,
    pub external_session_id: Option<String>,
    pub external_agent_id: Option<String>,
    pub agent_type: String,
    pub role_hint: Option<String>,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRun {
    pub id: Uuid,
    pub run_type: String,
    pub status: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub cwd: Option<String>,
    pub command_preview: Option<String>,
}

/// Canonical typed semantics carried by an event in addition to its exact payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalTypedEventKind {
    FileTouched,
    VcsChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalObservationKind {
    Event,
    FileTouch,
    VcsChange,
}

/// One normalized file effect linked to its exact canonical event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalFileTouch {
    pub id: Uuid,
    pub history_record_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub vcs_workspace_id: Option<Uuid>,
    pub path: String,
    pub change_kind: Option<FileChangeKind>,
    pub old_path: Option<String>,
    pub line_count_delta: Option<i64>,
    pub confidence: Confidence,
    pub source_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalVcsChange {
    pub id: Uuid,
    pub vcs_workspace_id: Uuid,
    pub kind: VcsChangeKind,
    pub change_id: String,
    pub parent_change_ids: Vec<String>,
    pub branch_or_bookmark: Option<String>,
    pub tree_hash: Option<String>,
    pub author_time_ms: Option<i64>,
    pub confidence: Confidence,
    pub source_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalCitation {
    pub observation_id: Uuid,
    pub observation_seq: u64,
    pub observation_kind: CanonicalObservationKind,
    pub event_id: Option<Uuid>,
    pub event_seq: Option<u64>,
    pub source_path: Option<String>,
    pub fixture_line: Option<u64>,
    pub source_record_ordinal: Option<u64>,
    pub source_record_subrecord_index: Option<u32>,
    pub byte_range: Option<CanonicalByteRange>,
    pub source_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalObservation {
    pub observation_id: Uuid,
    pub observation_seq: u64,
    pub observation_kind: CanonicalObservationKind,
    pub event_id: Option<Uuid>,
    pub event_seq: Option<u64>,
    pub occurred_at_ms: i64,
    pub history_record_id: Option<Uuid>,
    pub event_type: String,
    pub role: Option<String>,
    pub payload: Value,
    pub metadata: Value,
    /// Provider-independent bounded outcome and identifier evidence.
    pub result: CanonicalResultEvidence,
    pub actor: Option<CanonicalActor>,
    pub run: Option<CanonicalRun>,
    pub source: Option<CanonicalSource>,
    /// Explicit typed meaning for event kinds whose payload is provider-shaped.
    pub typed_event: Option<CanonicalTypedEventKind>,
    /// Stable normalized file records linked to this event, ordered by record ID.
    pub file_touch: Option<CanonicalFileTouch>,
    pub vcs_change: Option<CanonicalVcsChange>,
    pub citation: CanonicalCitation,
    /// Immutable identity/content digest used by incremental consumers.
    pub semantic_digest: String,
}

fn canonical_observations_select_sql(include_deleted: bool) -> &'static str {
    if include_deleted {
        r#"
        SELECT
            e.id, e.seq, e.occurred_at_ms,
            COALESCE(e.history_record_id, s.history_record_id, r.history_record_id),
            e.event_type, e.role, e.payload_json, e.metadata_json,
            s.id, s.parent_session_id, s.root_session_id,
            s.external_session_id, s.external_agent_id, s.agent_type,
            s.role_hint, s.is_primary,
            r.id, r.run_type, r.status, r.started_at_ms, r.ended_at_ms,
            r.exit_code, r.cwd, r.command_preview,
            cs.id, cs.provider, cs.raw_source_path, cs.source_format,
            cs.source_root, cs.source_identity, cs.cwd
        FROM events e
        LEFT JOIN runs r ON r.id = e.run_id AND r.deleted_at_ms IS NULL
        LEFT JOIN sessions s
          ON s.id = COALESCE(e.session_id, r.session_id)
         AND s.deleted_at_ms IS NULL
        LEFT JOIN capture_sources cs
          ON cs.id = COALESCE(e.capture_source_id, s.capture_source_id, r.source_id)
        WHERE e.id = ?1
    "#
    } else {
        r#"
        SELECT
            e.id, e.seq, e.occurred_at_ms,
            COALESCE(e.history_record_id, s.history_record_id, r.history_record_id),
            e.event_type, e.role, e.payload_json, e.metadata_json,
            s.id, s.parent_session_id, s.root_session_id,
            s.external_session_id, s.external_agent_id, s.agent_type,
            s.role_hint, s.is_primary,
            r.id, r.run_type, r.status, r.started_at_ms, r.ended_at_ms,
            r.exit_code, r.cwd, r.command_preview,
            cs.id, cs.provider, cs.raw_source_path, cs.source_format,
            cs.source_root, cs.source_identity, cs.cwd
        FROM events e
        LEFT JOIN runs r ON r.id = e.run_id AND r.deleted_at_ms IS NULL
        LEFT JOIN sessions s
          ON s.id = COALESCE(e.session_id, r.session_id)
         AND s.deleted_at_ms IS NULL
        LEFT JOIN capture_sources cs
          ON cs.id = COALESCE(e.capture_source_id, s.capture_source_id, r.source_id)
        WHERE e.deleted_at_ms IS NULL AND e.id = ?1
    "#
    }
}

fn canonical_event_observation_from_row(
    row: &Row<'_>,
    observation_seq: u64,
) -> rusqlite::Result<CanonicalObservation> {
    let event_id = parse_uuid_column(row, 0)?;
    let event_seq = nonnegative_u64(row.get(1)?)?;
    let event_type = row.get::<_, String>(4)?;
    let mut payload = parse_json_column(row, 6)?;
    let mut metadata = parse_json_column(row, 7)?;
    if matches!(event_type.as_str(), "tool_output" | "command_output") {
        // Result bodies and previews are source-backed. The durable canonical
        // event carries only bounded identity/timing metadata, its typed result
        // contract, and citation.
        payload = ctx_history_core::compact_result_payload(&payload);
    }
    let result = take_result_evidence(&mut payload);
    strip_local_complete_content_metadata(&mut metadata);

    let source_path = row.get::<_, Option<String>>(26)?;
    let source = match optional_uuid_column(row, 24)? {
        Some(id) => Some(CanonicalSource {
            id,
            provider: row.get(25)?,
            path: source_path.clone(),
            format: row.get(27)?,
            root: row.get(28)?,
            identity: row.get(29)?,
            cwd: row.get(30)?,
            permitted_bytes: None,
            imported_observation: None,
        }),
        None => None,
    };

    let direct_session_id = optional_uuid_column(row, 8)?;
    let root_session_id = optional_uuid_column(row, 10)?;
    let actor = match direct_session_id {
        Some(direct_session_id) => Some(CanonicalActor {
            direct_session_id,
            root_session_id: root_session_id.unwrap_or(direct_session_id),
            parent_session_id: optional_uuid_column(row, 9)?,
            external_session_id: row.get(11)?,
            external_agent_id: row.get(12)?,
            agent_type: row.get(13)?,
            role_hint: row.get(14)?,
            is_primary: row.get::<_, i64>(15)? != 0,
        }),
        None => None,
    };

    let run = match optional_uuid_column(row, 16)? {
        Some(id) => Some(CanonicalRun {
            id,
            run_type: row.get(17)?,
            status: row.get(18)?,
            started_at_ms: row.get(19)?,
            ended_at_ms: row.get(20)?,
            exit_code: row.get(21)?,
            cwd: row.get(22)?,
            command_preview: row.get(23)?,
        }),
        None => None,
    };

    let fixture_line = json_u64(&metadata, &["fixture_line"]);
    let source_record_ordinal = json_u64(&metadata, &["source_record_ordinal"])
        .or_else(|| fixture_line.and_then(|line| line.checked_sub(1)));
    let source_record_subrecord_index = json_u64(&metadata, &["source_record_subrecord_index"])
        .and_then(|value| u32::try_from(value).ok());
    let byte_range = json_byte_range(&metadata);
    let typed_event = match event_type.as_str() {
        "file_touched" => Some(CanonicalTypedEventKind::FileTouched),
        "vcs_change" => Some(CanonicalTypedEventKind::VcsChange),
        _ => None,
    };
    Ok(CanonicalObservation {
        observation_id: event_id,
        observation_seq,
        observation_kind: CanonicalObservationKind::Event,
        event_id: Some(event_id),
        event_seq: Some(event_seq),
        occurred_at_ms: row.get(2)?,
        history_record_id: optional_uuid_column(row, 3)?,
        event_type,
        role: row.get(5)?,
        payload,
        metadata,
        result,
        actor,
        run,
        source,
        typed_event,
        file_touch: None,
        vcs_change: None,
        citation: CanonicalCitation {
            observation_id: event_id,
            observation_seq,
            observation_kind: CanonicalObservationKind::Event,
            event_id: Some(event_id),
            event_seq: Some(event_seq),
            source_path,
            fixture_line,
            source_record_ordinal,
            source_record_subrecord_index,
            byte_range,
            source_sha256: None,
        },
        semantic_digest: String::new(),
    })
}

pub(crate) fn canonical_observation_by_coordinate(
    conn: &rusqlite::Connection,
    observation_seq: u64,
    kind: &str,
    id: Uuid,
) -> rusqlite::Result<CanonicalObservation> {
    canonical_observation_by_coordinate_with_deleted(conn, observation_seq, kind, id, false)
}

pub(crate) fn canonical_observation_by_coordinate_including_deleted(
    conn: &rusqlite::Connection,
    observation_seq: u64,
    kind: &str,
    id: Uuid,
) -> rusqlite::Result<CanonicalObservation> {
    canonical_observation_by_coordinate_with_deleted(conn, observation_seq, kind, id, true)
}

fn canonical_observation_by_coordinate_with_deleted(
    conn: &rusqlite::Connection,
    observation_seq: u64,
    kind: &str,
    id: Uuid,
    include_deleted: bool,
) -> rusqlite::Result<CanonicalObservation> {
    match kind {
        "event" => conn.query_row(
            canonical_observations_select_sql(include_deleted),
            [id.to_string()],
            |row| canonical_event_observation_from_row(row, observation_seq),
        ),
        "file_touch" => {
            canonical_file_touch_observation(conn, observation_seq, id, include_deleted)
        }
        "vcs_change" => {
            canonical_vcs_change_observation(conn, observation_seq, id, include_deleted)
        }
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn canonical_file_touch_observation(
    conn: &rusqlite::Connection,
    observation_seq: u64,
    id: Uuid,
    include_deleted: bool,
) -> rusqlite::Result<CanonicalObservation> {
    let sql = if include_deleted {
        "SELECT ft.id, ft.history_record_id, ft.run_id, ft.event_id, ft.vcs_workspace_id,
                ft.path, ft.change_kind, ft.old_path, ft.line_count_delta, ft.confidence,
                ft.created_at_ms, ft.source_id, ft.metadata_json, e.seq
         FROM files_touched ft LEFT JOIN events e ON e.id = ft.event_id
         WHERE ft.id = ?1"
    } else {
        "SELECT ft.id, ft.history_record_id, ft.run_id, ft.event_id, ft.vcs_workspace_id,
                ft.path, ft.change_kind, ft.old_path, ft.line_count_delta, ft.confidence,
                ft.created_at_ms, ft.source_id, ft.metadata_json, e.seq
         FROM files_touched ft LEFT JOIN events e ON e.id = ft.event_id
         WHERE ft.id = ?1 AND ft.deleted_at_ms IS NULL"
    };
    conn.query_row(sql, [id.to_string()], |row| {
        let metadata = parse_json_column(row, 12)?;
        let event_id = optional_uuid_column(row, 3)?;
        let event_seq = row
            .get::<_, Option<i64>>(13)?
            .map(nonnegative_u64)
            .transpose()?;
        let source_id = optional_uuid_column(row, 11)?;
        let source = source_id
            .map(|source_id| canonical_source_by_id(conn, source_id))
            .transpose()?
            .flatten();
        let source_path = source.as_ref().and_then(|value| value.path.clone());
        let run_id = optional_uuid_column(row, 2)?;
        let actor_session_id = conn.query_row(
            "SELECT COALESCE(
                    (SELECT session_id FROM events WHERE id = ?1),
                    (SELECT session_id FROM runs WHERE id = ?2),
                    (SELECT id FROM sessions WHERE capture_source_id = ?3
                     AND deleted_at_ms IS NULL ORDER BY is_primary DESC, id LIMIT 1)
                 )",
            params![
                event_id.map(|value| value.to_string()),
                run_id.map(|value| value.to_string()),
                source_id.map(|value| value.to_string())
            ],
            |actor_row| optional_uuid_column(actor_row, 0),
        )?;
        let actor = actor_session_id
            .map(|session_id| canonical_actor_by_id(conn, session_id))
            .transpose()?
            .flatten();
        let touch = CanonicalFileTouch {
            id: parse_uuid_column(row, 0)?,
            history_record_id: optional_uuid_column(row, 1)?,
            run_id,
            event_id,
            vcs_workspace_id: optional_uuid_column(row, 4)?,
            path: row.get(5)?,
            change_kind: row
                .get::<_, Option<String>>(6)?
                .map(crate::connection::parse_text_enum)
                .transpose()?,
            old_path: row.get(7)?,
            line_count_delta: row.get(8)?,
            confidence: crate::connection::parse_text_enum(row.get(9)?)?,
            source_id,
        };
        Ok(CanonicalObservation {
            observation_id: id,
            observation_seq,
            observation_kind: CanonicalObservationKind::FileTouch,
            event_id,
            event_seq,
            occurred_at_ms: row.get(10)?,
            history_record_id: touch.history_record_id,
            event_type: "file_touched".to_owned(),
            role: None,
            payload: serde_json::json!({}),
            metadata: metadata.clone(),
            result: CanonicalResultEvidence::default(),
            actor,
            run: None,
            source,
            typed_event: Some(CanonicalTypedEventKind::FileTouched),
            file_touch: Some(touch),
            vcs_change: None,
            citation: canonical_sidecar_citation(
                id,
                observation_seq,
                CanonicalObservationKind::FileTouch,
                event_id,
                event_seq,
                source_path,
                &metadata,
            ),
            semantic_digest: String::new(),
        })
    })
}

fn canonical_vcs_change_observation(
    conn: &rusqlite::Connection,
    observation_seq: u64,
    id: Uuid,
    include_deleted: bool,
) -> rusqlite::Result<CanonicalObservation> {
    let sql = if include_deleted {
        "SELECT id, vcs_workspace_id, kind, change_id, parent_change_ids_json,
                branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms,
                source_id, metadata_json FROM vcs_changes WHERE id = ?1"
    } else {
        "SELECT id, vcs_workspace_id, kind, change_id, parent_change_ids_json,
                branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms,
                source_id, metadata_json FROM vcs_changes
         WHERE id = ?1 AND deleted_at_ms IS NULL"
    };
    conn.query_row(sql, [id.to_string()], |row| {
        let metadata = parse_json_column(row, 11)?;
        let source_id = optional_uuid_column(row, 10)?;
        let source = source_id
            .map(|source_id| canonical_source_by_id(conn, source_id))
            .transpose()?
            .flatten();
        let source_path = source.as_ref().and_then(|value| value.path.clone());
        let (history_record_id, actor_session_id) = conn.query_row(
            "SELECT
                    (SELECT history_record_id FROM history_record_links
                     WHERE target_type = 'vcs_change' AND target_id = ?1
                       AND deleted_at_ms IS NULL ORDER BY id LIMIT 1),
                    COALESCE(
                        (SELECT id FROM sessions WHERE capture_source_id = ?2
                         AND deleted_at_ms IS NULL ORDER BY is_primary DESC, id LIMIT 1),
                        (SELECT s.id FROM sessions s JOIN history_record_links l
                         ON l.history_record_id = s.history_record_id
                         WHERE l.target_type = 'vcs_change' AND l.target_id = ?1
                           AND l.deleted_at_ms IS NULL AND s.deleted_at_ms IS NULL
                         ORDER BY s.is_primary DESC, s.id LIMIT 1)
                    )",
            params![id.to_string(), source_id.map(|value| value.to_string())],
            |link_row| {
                Ok((
                    optional_uuid_column(link_row, 0)?,
                    optional_uuid_column(link_row, 1)?,
                ))
            },
        )?;
        let actor = actor_session_id
            .map(|session_id| canonical_actor_by_id(conn, session_id))
            .transpose()?
            .flatten();
        let change = CanonicalVcsChange {
            id: parse_uuid_column(row, 0)?,
            vcs_workspace_id: parse_uuid_column(row, 1)?,
            kind: crate::connection::parse_text_enum(row.get(2)?)?,
            change_id: row.get(3)?,
            parent_change_ids: parse_json_column(row, 4).and_then(|value| {
                serde_json::from_value(value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })?,
            branch_or_bookmark: row.get(5)?,
            tree_hash: row.get(6)?,
            author_time_ms: row.get(7)?,
            confidence: crate::connection::parse_text_enum(row.get(8)?)?,
            source_id,
        };
        Ok(CanonicalObservation {
            observation_id: id,
            observation_seq,
            observation_kind: CanonicalObservationKind::VcsChange,
            event_id: None,
            event_seq: None,
            occurred_at_ms: row.get(9)?,
            history_record_id,
            event_type: "vcs_change".to_owned(),
            role: None,
            payload: serde_json::json!({}),
            metadata: metadata.clone(),
            result: CanonicalResultEvidence::default(),
            actor,
            run: None,
            source,
            typed_event: Some(CanonicalTypedEventKind::VcsChange),
            file_touch: None,
            vcs_change: Some(change),
            citation: canonical_sidecar_citation(
                id,
                observation_seq,
                CanonicalObservationKind::VcsChange,
                None,
                None,
                source_path,
                &metadata,
            ),
            semantic_digest: String::new(),
        })
    })
}

fn canonical_source_by_id(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> rusqlite::Result<Option<CanonicalSource>> {
    conn.query_row(
        "SELECT provider, raw_source_path, source_format, source_root, source_identity, cwd
         FROM capture_sources WHERE id = ?1",
        [id.to_string()],
        |row| {
            Ok(CanonicalSource {
                id,
                provider: row.get(0)?,
                path: row.get(1)?,
                format: row.get(2)?,
                root: row.get(3)?,
                identity: row.get(4)?,
                cwd: row.get(5)?,
                imported_observation: None,
                permitted_bytes: None,
            })
        },
    )
    .optional()
}

fn canonical_actor_by_id(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> rusqlite::Result<Option<CanonicalActor>> {
    conn.query_row(
        "SELECT parent_session_id, root_session_id, external_session_id, external_agent_id,
                agent_type, role_hint, is_primary FROM sessions
         WHERE id = ?1 AND deleted_at_ms IS NULL",
        [id.to_string()],
        |row| {
            Ok(CanonicalActor {
                direct_session_id: id,
                root_session_id: optional_uuid_column(row, 1)?.unwrap_or(id),
                parent_session_id: optional_uuid_column(row, 0)?,
                external_session_id: row.get(2)?,
                external_agent_id: row.get(3)?,
                agent_type: row.get(4)?,
                role_hint: row.get(5)?,
                is_primary: row.get::<_, i64>(6)? != 0,
            })
        },
    )
    .optional()
}

fn canonical_sidecar_citation(
    observation_id: Uuid,
    observation_seq: u64,
    observation_kind: CanonicalObservationKind,
    event_id: Option<Uuid>,
    event_seq: Option<u64>,
    source_path: Option<String>,
    metadata: &Value,
) -> CanonicalCitation {
    let fixture_line = json_u64(metadata, &["fixture_line"]);
    CanonicalCitation {
        observation_id,
        observation_seq,
        observation_kind,
        event_id,
        event_seq,
        source_path,
        fixture_line,
        source_record_ordinal: json_u64(metadata, &["source_record_ordinal"])
            .or_else(|| fixture_line.and_then(|line| line.checked_sub(1))),
        source_record_subrecord_index: json_u64(metadata, &["source_record_subrecord_index"])
            .and_then(|value| u32::try_from(value).ok()),
        byte_range: json_byte_range(metadata),
        source_sha256: None,
    }
}

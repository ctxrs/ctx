use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::encoding::{
    canonical_json, content_digest, record_chain_digest, sha256_hex, RecordDigestFields,
};
use super::pages::insert_record_chunk;
use super::protocol_validity::{parse_uuid, sanitize_canonical_observation};
use super::support::{nonnegative_u64, to_i64};
use super::{
    active_state, JournalEntityKind, JournalEvidenceIdentity, JournalOperation,
    JournalProvenanceIdentity, ProjectionJournalRecord, EMPTY_SHA256, PROJECTION_CONTRACT_VERSION,
    PROJECTION_JOURNAL_MAX_PAGE_BYTES, PROJECTION_JOURNAL_RECORD_MAX_BYTES,
};
use crate::canonical_observations::{
    canonical_observation_by_coordinate, canonical_observation_by_coordinate_including_deleted,
    canonical_semantic_digest, CanonicalObservation,
};
use crate::{Result, StoreError};

#[derive(Debug)]
struct PreparedEntity {
    operation: JournalOperation,
    canonical_payload_json: Option<String>,
    payload_sha256: String,
    evidence_json: String,
    provenance_json: String,
    content_digest: String,
}

fn live_entity_ids(conn: &rusqlite::Connection, kind: JournalEntityKind) -> Result<Vec<Uuid>> {
    let sql = match kind {
        // Baselines must preserve the canonical provider order. Private
        // command/result correlation is deliberately bounded across journal
        // pages, so UUID order can otherwise separate a result from its
        // producing command by the entire corpus.
        JournalEntityKind::Event => {
            "SELECT id FROM events WHERE deleted_at_ms IS NULL
             ORDER BY COALESCE(capture_source_id, ''),
                      CASE
                        WHEN json_type(metadata_json, '$.source_record_ordinal') = 'integer'
                         AND json_extract(metadata_json, '$.source_record_ordinal') >= 0
                        THEN json_extract(metadata_json, '$.source_record_ordinal')
                        ELSE 9223372036854775807
                      END,
                      CASE
                        WHEN json_type(metadata_json, '$.source_record_subrecord_index') = 'integer'
                         AND json_extract(metadata_json, '$.source_record_subrecord_index') >= 0
                        THEN json_extract(metadata_json, '$.source_record_subrecord_index')
                        ELSE 9223372036854775807
                      END,
                      occurred_at_ms, seq, id"
        }
        JournalEntityKind::FileTouch => {
            "SELECT id FROM files_touched WHERE deleted_at_ms IS NULL ORDER BY id"
        }
        JournalEntityKind::VcsChange => {
            "SELECT id FROM vcs_changes WHERE deleted_at_ms IS NULL ORDER BY id"
        }
    };
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(parse_uuid(&row?)?);
    }
    Ok(ids)
}

pub(super) fn append_entity(
    conn: &rusqlite::Connection,
    kind: JournalEntityKind,
    id: Uuid,
) -> Result<()> {
    let Some(state) = active_state(conn)? else {
        return Ok(());
    };
    let prepared = prepare_entity(conn, kind, id)?;
    let previous = conn
        .query_row(
            "SELECT entity_revision, content_digest FROM projection_journal_entities
             WHERE generation = ?1 AND entity_kind = ?2 AND stable_entity_id = ?3",
            params![
                to_i64(state.generation, "generation")?,
                kind.as_str(),
                id.to_string()
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|(_, digest)| digest == &prepared.content_digest)
    {
        return Ok(());
    }
    if previous.is_none() && prepared.operation == JournalOperation::Delete {
        return Ok(());
    }
    let revision = match previous {
        Some((revision, _)) => nonnegative_u64(revision, "revision")?
            .checked_add(1)
            .ok_or_else(|| {
                StoreError::InvalidProjectionJournalData("entity revision overflow".to_owned())
            })?,
        None => 1,
    };
    let sequence = state.high_water_sequence.checked_add(1).ok_or_else(|| {
        StoreError::InvalidProjectionJournalData("journal sequence overflow".to_owned())
    })?;
    let cumulative_digest = record_chain_digest(
        &state.cumulative_digest,
        state.generation,
        sequence,
        kind,
        id,
        revision,
        RecordDigestFields {
            operation: prepared.operation,
            payload_sha256: &prepared.payload_sha256,
            evidence_json: &prepared.evidence_json,
            provenance_json: &prepared.provenance_json,
        },
    )?;
    let record = projection_record(
        state.generation,
        sequence,
        kind,
        id,
        revision,
        &prepared,
        cumulative_digest.clone(),
    )?;
    insert_record_chunk(conn, std::slice::from_ref(&record))?;
    persist_entity_state(conn, state.generation, kind, id, revision, &prepared)?;
    conn.execute(
        "UPDATE projection_journal_state
         SET high_water_sequence = ?1, cumulative_digest = ?2 WHERE singleton = 1",
        params![to_i64(sequence, "sequence")?, cumulative_digest],
    )?;
    Ok(())
}

pub(super) fn append_baseline(conn: &rusqlite::Connection) -> Result<()> {
    let mut state = active_state(conn)?.ok_or(StoreError::ProjectionJournalInactive)?;
    let mut chunk = Vec::new();
    let mut chunk_record_bytes = 0_usize;
    for kind in [
        JournalEntityKind::Event,
        JournalEntityKind::FileTouch,
        JournalEntityKind::VcsChange,
    ] {
        for id in live_entity_ids(conn, kind)? {
            let prepared = prepare_entity(conn, kind, id)?;
            let sequence = state.high_water_sequence.checked_add(1).ok_or_else(|| {
                StoreError::InvalidProjectionJournalData("journal sequence overflow".to_owned())
            })?;
            let cumulative_digest = record_chain_digest(
                &state.cumulative_digest,
                state.generation,
                sequence,
                kind,
                id,
                1,
                RecordDigestFields {
                    operation: prepared.operation,
                    payload_sha256: &prepared.payload_sha256,
                    evidence_json: &prepared.evidence_json,
                    provenance_json: &prepared.provenance_json,
                },
            )?;
            let record = projection_record(
                state.generation,
                sequence,
                kind,
                id,
                1,
                &prepared,
                cumulative_digest.clone(),
            )?;
            let record_bytes = serde_json::to_vec(&record)?.len();
            let next_chunk_bytes = 2_usize
                .saturating_add(chunk_record_bytes)
                .saturating_add(record_bytes)
                .saturating_add(chunk.len());
            if !chunk.is_empty()
                && (chunk.len() == super::pages::PROJECTION_JOURNAL_CHUNK_SIZE
                    || next_chunk_bytes > PROJECTION_JOURNAL_MAX_PAGE_BYTES)
            {
                insert_record_chunk(conn, &chunk)?;
                chunk.clear();
                chunk_record_bytes = 0;
            }
            persist_entity_state(conn, state.generation, kind, id, 1, &prepared)?;
            chunk_record_bytes = chunk_record_bytes.saturating_add(record_bytes);
            chunk.push(record);
            state.high_water_sequence = sequence;
            state.cumulative_digest = cumulative_digest;
        }
    }
    if !chunk.is_empty() {
        insert_record_chunk(conn, &chunk)?;
    }
    conn.execute(
        "UPDATE projection_journal_state
         SET high_water_sequence = ?1, cumulative_digest = ?2 WHERE singleton = 1",
        params![
            to_i64(state.high_water_sequence, "sequence")?,
            state.cumulative_digest
        ],
    )?;
    Ok(())
}

fn persist_entity_state(
    conn: &rusqlite::Connection,
    generation: u64,
    kind: JournalEntityKind,
    id: Uuid,
    revision: u64,
    prepared: &PreparedEntity,
) -> Result<()> {
    conn.execute(
        "INSERT INTO projection_journal_entities
             (generation, entity_kind, stable_entity_id, entity_revision,
              content_digest)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(generation, entity_kind, stable_entity_id) DO UPDATE SET
             entity_revision = excluded.entity_revision,
             content_digest = excluded.content_digest",
        params![
            to_i64(generation, "generation")?,
            kind.as_str(),
            id.to_string(),
            to_i64(revision, "revision")?,
            prepared.content_digest,
        ],
    )?;
    Ok(())
}

fn projection_record(
    generation: u64,
    sequence: u64,
    kind: JournalEntityKind,
    id: Uuid,
    revision: u64,
    prepared: &PreparedEntity,
    cumulative_digest: String,
) -> Result<ProjectionJournalRecord> {
    Ok(ProjectionJournalRecord {
        generation,
        sequence,
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        entity_kind: kind,
        stable_entity_id: id,
        entity_revision: revision,
        operation: prepared.operation,
        canonical_payload: prepared
            .canonical_payload_json
            .as_ref()
            .map(|payload| serde_json::from_str(payload))
            .transpose()?,
        payload_sha256: prepared.payload_sha256.clone(),
        evidence: serde_json::from_str(&prepared.evidence_json)?,
        provenance: serde_json::from_str(&prepared.provenance_json)?,
        cumulative_digest,
    })
}

fn prepare_entity(
    conn: &rusqlite::Connection,
    kind: JournalEntityKind,
    id: Uuid,
) -> Result<PreparedEntity> {
    let kind_name = kind.as_str();
    let current = canonical_observation_by_coordinate(conn, 0, kind_name, id).optional()?;
    let Some(mut observation) = current else {
        let previous =
            canonical_observation_by_coordinate_including_deleted(conn, 0, kind_name, id)
                .optional()?;
        let (evidence_json, provenance_json) = if let Some(mut previous) = previous {
            sanitize_observation(&mut previous)?;
            (
                canonical_json(&evidence_identities(&previous))?,
                canonical_json(&provenance_identity(kind, id, &previous))?,
            )
        } else {
            let provenance = JournalProvenanceIdentity {
                entity_kind: kind,
                stable_entity_id: id,
                capture_source_id: None,
                provider: None,
                provider_external_id: None,
            };
            ("[]".to_owned(), canonical_json(&provenance)?)
        };
        let content_digest = content_digest(
            JournalOperation::Delete,
            EMPTY_SHA256,
            &evidence_json,
            &provenance_json,
        );
        return Ok(PreparedEntity {
            operation: JournalOperation::Delete,
            canonical_payload_json: None,
            payload_sha256: EMPTY_SHA256.to_owned(),
            evidence_json,
            provenance_json,
            content_digest,
        });
    };

    sanitize_observation(&mut observation)?;

    let evidence = evidence_identities(&observation);
    let provenance = provenance_identity(kind, id, &observation);
    let canonical_payload_json = canonical_json(&observation)?;
    let encoded_bytes = canonical_payload_json.len();
    if encoded_bytes > PROJECTION_JOURNAL_RECORD_MAX_BYTES {
        return Err(StoreError::ProjectionJournalPayloadTooLarge {
            entity_kind: kind.as_str(),
            entity_id: id,
            encoded_bytes,
            max_bytes: PROJECTION_JOURNAL_RECORD_MAX_BYTES,
        });
    }
    let payload_sha256 = sha256_hex(canonical_payload_json.as_bytes());
    let evidence_json = canonical_json(&evidence)?;
    let provenance_json = canonical_json(&provenance)?;
    let content_digest = content_digest(
        JournalOperation::Upsert,
        &payload_sha256,
        &evidence_json,
        &provenance_json,
    );
    Ok(PreparedEntity {
        operation: JournalOperation::Upsert,
        canonical_payload_json: Some(canonical_payload_json),
        payload_sha256,
        evidence_json,
        provenance_json,
        content_digest,
    })
}

fn sanitize_observation(observation: &mut CanonicalObservation) -> Result<()> {
    // Repository authorization candidates remain ephemeral policy. The durable
    // journal keeps provider evidence locators, but not working/repository roots.
    if let Some(source) = &mut observation.source {
        source.root = None;
        source.cwd = None;
    }
    if let Some(run) = &mut observation.run {
        run.cwd = None;
    }
    sanitize_canonical_observation(observation)?;
    observation.observation_seq = 0;
    observation.citation.observation_seq = 0;
    observation.semantic_digest = canonical_semantic_digest(observation)?;
    Ok(())
}

fn evidence_identities(observation: &CanonicalObservation) -> Vec<JournalEvidenceIdentity> {
    let citation = &observation.citation;
    let Some(event_id) = citation.event_id else {
        return Vec::new();
    };
    let source_id = observation.source.as_ref().map(|source| source.id);
    vec![JournalEvidenceIdentity {
        event_id,
        source_id,
        source_path: citation.source_path.clone(),
        source_record_ordinal: citation.source_record_ordinal,
        source_record_subrecord_index: citation.source_record_subrecord_index,
        byte_start: citation.byte_range.as_ref().map(|range| range.start),
        byte_end_exclusive: citation
            .byte_range
            .as_ref()
            .map(|range| range.end_exclusive),
    }]
}

fn provenance_identity(
    kind: JournalEntityKind,
    id: Uuid,
    observation: &CanonicalObservation,
) -> JournalProvenanceIdentity {
    JournalProvenanceIdentity {
        entity_kind: kind,
        stable_entity_id: id,
        capture_source_id: observation.source.as_ref().map(|source| source.id),
        provider: observation
            .source
            .as_ref()
            .map(|source| source.provider.clone()),
        provider_external_id: observation
            .actor
            .as_ref()
            .and_then(|actor| actor.external_session_id.clone()),
    }
}

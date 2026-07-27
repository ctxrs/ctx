use std::cell::RefCell;

use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::encoding::{
    canonical_json, content_digest, record_chain_digest, sha256_hex, RecordDigestFields,
};
use super::pages::{insert_record_chunk, PROJECTION_JOURNAL_CHUNK_SIZE};
use super::protocol_validity::{parse_uuid, sanitize_canonical_observation};
use super::support::{nonnegative_u64, to_i64};
use super::{
    active_state, JournalEntityKind, JournalEvidenceIdentity, JournalOperation,
    JournalProvenanceIdentity, ProjectionJournalRecord, EMPTY_SHA256, PROJECTION_CONTRACT_VERSION,
    PROJECTION_JOURNAL_MAX_PAGE_BYTES, PROJECTION_JOURNAL_RECORD_MAX_BYTES,
};
use crate::canonical_observations::{
    canonical_observation_by_coordinate, canonical_observation_by_coordinate_including_deleted,
    canonical_semantic_digest, visit_live_canonical_event_observations, CanonicalObservation,
};
use crate::{Result, StoreError};

/// Packs a publication group's journal records into physical chunks and writes
/// each chunk as soon as it is complete.
///
/// The collector exists only to pack records densely into the schema's chunk
/// shape; it is not a staging area for the whole group. A chunk is inserted
/// into `projection_journal_chunks` the moment the next record would exceed
/// the persisted chunk bound, so the collector never retains more than one
/// in-flight chunk (at most `PROJECTION_JOURNAL_CHUNK_SIZE` records and
/// `PROJECTION_JOURNAL_MAX_PAGE_BYTES` encoded bytes). This is the same
/// streaming shape `append_baseline` already uses.
///
/// Every insert happens inside the caller's still-open publication
/// transaction, so an abandoned group rolls the chunks back with the Core rows
/// that produced them.
#[derive(Debug, Default)]
pub(crate) struct GroupJournalCollector {
    chunk: Vec<ProjectionJournalRecord>,
    chunk_bytes: usize,
    record_count: usize,
    uncompressed_bytes: usize,
    sealed: bool,
}

impl GroupJournalCollector {
    fn push(&mut self, conn: &rusqlite::Connection, record: ProjectionJournalRecord) -> Result<()> {
        if self.sealed {
            return Err(StoreError::NativePathJournalSealed);
        }
        let record_bytes = serde_json::to_vec(&record)?.len();
        let append_to_current = !self.chunk.is_empty()
            && self.chunk.len() < PROJECTION_JOURNAL_CHUNK_SIZE
            && self
                .chunk_bytes
                .saturating_add(1)
                .saturating_add(record_bytes)
                <= PROJECTION_JOURNAL_MAX_PAGE_BYTES;
        if !append_to_current && !self.chunk.is_empty() {
            self.flush_chunk(conn)?;
        }
        if append_to_current {
            self.chunk_bytes = self
                .chunk_bytes
                .saturating_add(1)
                .saturating_add(record_bytes);
            self.uncompressed_bytes = self
                .uncompressed_bytes
                .saturating_add(1)
                .saturating_add(record_bytes);
        } else {
            self.chunk_bytes = 2_usize.saturating_add(record_bytes);
            self.uncompressed_bytes = self
                .uncompressed_bytes
                .saturating_add(2)
                .saturating_add(record_bytes);
        }
        self.chunk.push(record);
        self.record_count = self.record_count.saturating_add(1);
        Ok(())
    }

    fn flush_chunk(&mut self, conn: &rusqlite::Connection) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        insert_record_chunk(conn, &self.chunk)?;
        self.chunk.clear();
        self.chunk_bytes = 0;
        Ok(())
    }

    pub(crate) fn seal_and_flush(&mut self, conn: &rusqlite::Connection) -> Result<(usize, usize)> {
        if !self.sealed {
            self.flush_chunk(conn)?;
            self.sealed = true;
        }
        Ok((self.record_count, self.uncompressed_bytes))
    }
}

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
        // correlation is deliberately bounded across journal pages, so UUID
        // order can otherwise separate related non-output observations by the
        // entire corpus. Outputs use the Pro-only per-source stream.
        JournalEntityKind::Event => {
            "SELECT id FROM events
             WHERE deleted_at_ms IS NULL
               AND event_type NOT IN ('tool_output', 'command_output')
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
    collector: Option<&RefCell<Option<GroupJournalCollector>>>,
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
    match collector {
        Some(collector) => {
            let mut collector = collector.borrow_mut();
            match collector.as_mut() {
                Some(collector) => collector.push(conn, record)?,
                None => insert_record_chunk(conn, std::slice::from_ref(&record))?,
            }
        }
        None => insert_record_chunk(conn, std::slice::from_ref(&record))?,
    }
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
    visit_live_canonical_event_observations(conn, |observation| {
        let id = observation.observation_id;
        let prepared = prepare_live_observation(JournalEntityKind::Event, id, observation)?;
        append_baseline_record(
            conn,
            &mut state,
            JournalEntityKind::Event,
            id,
            prepared,
            &mut chunk,
            &mut chunk_record_bytes,
        )
    })?;
    for kind in [JournalEntityKind::FileTouch, JournalEntityKind::VcsChange] {
        for id in live_entity_ids(conn, kind)? {
            let prepared = prepare_entity(conn, kind, id)?;
            append_baseline_record(
                conn,
                &mut state,
                kind,
                id,
                prepared,
                &mut chunk,
                &mut chunk_record_bytes,
            )?;
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

fn append_baseline_record(
    conn: &rusqlite::Connection,
    state: &mut super::ActiveState,
    kind: JournalEntityKind,
    id: Uuid,
    prepared: PreparedEntity,
    chunk: &mut Vec<ProjectionJournalRecord>,
    chunk_record_bytes: &mut usize,
) -> Result<()> {
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
        .saturating_add(*chunk_record_bytes)
        .saturating_add(record_bytes)
        .saturating_add(chunk.len());
    if !chunk.is_empty()
        && (chunk.len() == PROJECTION_JOURNAL_CHUNK_SIZE
            || next_chunk_bytes > PROJECTION_JOURNAL_MAX_PAGE_BYTES)
    {
        insert_record_chunk(conn, chunk)?;
        chunk.clear();
        *chunk_record_bytes = 0;
    }
    persist_entity_state(conn, state.generation, kind, id, 1, &prepared)?;
    *chunk_record_bytes = chunk_record_bytes.saturating_add(record_bytes);
    chunk.push(record);
    state.high_water_sequence = sequence;
    state.cumulative_digest = cumulative_digest;
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
    conn.prepare_cached(
        "INSERT INTO projection_journal_entities
             (generation, entity_kind, stable_entity_id, entity_revision,
              content_digest)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(generation, entity_kind, stable_entity_id) DO UPDATE SET
             entity_revision = excluded.entity_revision,
             content_digest = excluded.content_digest",
    )?
    .execute(params![
        to_i64(generation, "generation")?,
        kind.as_str(),
        id.to_string(),
        to_i64(revision, "revision")?,
        prepared.content_digest,
    ])?;
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
    let Some(observation) = current else {
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

    prepare_live_observation(kind, id, observation)
}

fn prepare_live_observation(
    kind: JournalEntityKind,
    id: Uuid,
    mut observation: CanonicalObservation,
) -> Result<PreparedEntity> {
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

#[cfg(test)]
mod group_collector_tests {
    use serde_json::json;

    use super::*;

    fn record(sequence: u64, payload_bytes: usize) -> ProjectionJournalRecord {
        ProjectionJournalRecord {
            generation: 1,
            sequence,
            projection_contract_version: PROJECTION_CONTRACT_VERSION,
            entity_kind: JournalEntityKind::Event,
            stable_entity_id: Uuid::from_u128(u128::from(sequence)),
            entity_revision: 1,
            operation: JournalOperation::Upsert,
            canonical_payload: Some(json!({"body": "x".repeat(payload_bytes)})),
            payload_sha256: "a".repeat(64),
            evidence: Vec::new(),
            provenance: JournalProvenanceIdentity {
                entity_kind: JournalEntityKind::Event,
                stable_entity_id: Uuid::from_u128(u128::from(sequence)),
                capture_source_id: None,
                provider: None,
                provider_external_id: None,
            },
            cumulative_digest: "b".repeat(64),
        }
    }

    fn chunk_table() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE projection_journal_chunks (
                 generation INTEGER NOT NULL CHECK (generation > 0),
                 first_sequence INTEGER NOT NULL CHECK (first_sequence > 0),
                 last_sequence INTEGER NOT NULL CHECK (last_sequence >= first_sequence),
                 record_count INTEGER NOT NULL CHECK (record_count > 0 AND record_count <= 64),
                 uncompressed_bytes INTEGER NOT NULL CHECK (
                     uncompressed_bytes > 0 AND uncompressed_bytes <= 8388608
                 ),
                 records_zstd BLOB NOT NULL CHECK (length(records_zstd) > 0),
                 PRIMARY KEY (generation, first_sequence),
                 UNIQUE (generation, last_sequence),
                 CHECK (last_sequence - first_sequence + 1 = record_count)
             );",
        )
        .unwrap();
        conn
    }

    fn stored_chunks(conn: &rusqlite::Connection) -> Vec<(i64, i64)> {
        conn.prepare(
            "SELECT record_count, uncompressed_bytes FROM projection_journal_chunks
             ORDER BY first_sequence",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    }

    // A group's journal volume is derived from how many canonical observations
    // its mutations changed, which no coordinator can size in advance. The
    // collector must therefore accept an unbounded record stream while keeping
    // its own retained memory inside one physical chunk.
    #[test]
    fn group_collector_streams_past_the_former_group_byte_ceiling() {
        const FORMER_GROUP_BYTE_CEILING: usize = 8 * 1024 * 1024;
        let conn = chunk_table();
        let mut collector = GroupJournalCollector::default();
        let mut pushed = 0_usize;
        let mut sequence = 1_u64;
        while collector.uncompressed_bytes <= 3 * FORMER_GROUP_BYTE_CEILING {
            collector.push(&conn, record(sequence, 40_000)).unwrap();
            assert!(collector.chunk.len() <= PROJECTION_JOURNAL_CHUNK_SIZE);
            assert!(collector.chunk_bytes <= PROJECTION_JOURNAL_MAX_PAGE_BYTES);
            sequence += 1;
            pushed += 1;
        }
        assert!(collector.uncompressed_bytes > FORMER_GROUP_BYTE_CEILING);
        assert!(
            !stored_chunks(&conn).is_empty(),
            "chunks flush while packing"
        );

        let (records, bytes) = collector.seal_and_flush(&conn).unwrap();
        assert_eq!(records, pushed);
        let chunks = stored_chunks(&conn);
        assert_eq!(
            chunks.iter().map(|(count, _)| *count).sum::<i64>(),
            pushed as i64
        );
        assert_eq!(
            chunks.iter().map(|(_, chunk)| *chunk).sum::<i64>(),
            bytes as i64
        );
        assert!(chunks
            .iter()
            .all(|(count, _)| *count <= PROJECTION_JOURNAL_CHUNK_SIZE as i64));
    }

    // A record that would carry the retained chunk past the persisted page
    // bound closes that chunk first instead of growing the buffer.
    #[test]
    fn group_collector_starts_a_new_chunk_before_exceeding_the_page_bound() {
        let half_page = PROJECTION_JOURNAL_MAX_PAGE_BYTES / 2;
        let half_record_bytes = serde_json::to_vec(&record(1, half_page)).unwrap().len();
        assert!(
            2 * half_record_bytes > PROJECTION_JOURNAL_MAX_PAGE_BYTES,
            "two half-page records must not fit one chunk"
        );

        let conn = chunk_table();
        let mut collector = GroupJournalCollector::default();
        collector.push(&conn, record(1, half_page)).unwrap();
        assert!(stored_chunks(&conn).is_empty());
        collector.push(&conn, record(2, half_page)).unwrap();

        assert_eq!(
            stored_chunks(&conn),
            vec![(1, (2 + half_record_bytes) as i64)]
        );
        assert_eq!(collector.chunk.len(), 1);
        assert!(collector.chunk_bytes <= PROJECTION_JOURNAL_MAX_PAGE_BYTES);

        collector.seal_and_flush(&conn).unwrap();
        assert_eq!(stored_chunks(&conn).len(), 2);
    }

    // The persisted chunk shape also caps a chunk at 64 records.
    #[test]
    fn group_collector_closes_a_chunk_at_the_persisted_record_bound() {
        let conn = chunk_table();
        let mut collector = GroupJournalCollector::default();
        for sequence in 1..=u64::try_from(PROJECTION_JOURNAL_CHUNK_SIZE).unwrap() {
            collector.push(&conn, record(sequence, 8)).unwrap();
        }
        assert!(stored_chunks(&conn).is_empty());

        collector
            .push(
                &conn,
                record(u64::try_from(PROJECTION_JOURNAL_CHUNK_SIZE).unwrap() + 1, 8),
            )
            .unwrap();
        assert_eq!(
            stored_chunks(&conn)
                .iter()
                .map(|(count, _)| *count)
                .collect::<Vec<_>>(),
            vec![PROJECTION_JOURNAL_CHUNK_SIZE as i64]
        );
        assert_eq!(collector.chunk.len(), 1);
    }

    #[test]
    fn sealed_group_collector_refuses_further_records() {
        let conn = chunk_table();
        let mut collector = GroupJournalCollector::default();
        collector.push(&conn, record(1, 8)).unwrap();
        collector.seal_and_flush(&conn).unwrap();
        assert!(matches!(
            collector.push(&conn, record(2, 8)),
            Err(StoreError::NativePathJournalSealed)
        ));
    }
}

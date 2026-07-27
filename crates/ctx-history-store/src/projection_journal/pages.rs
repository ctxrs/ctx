use rusqlite::{params, OptionalExtension};

use super::encoding::{
    canonical_json, generation_digest, record_chain_digest, sha256_hex, RecordDigestFields,
};
use super::protocol_validity::{
    authorized_repository_roots, validate_stored_evidence, validate_stored_provenance,
};
use super::support::{nonnegative_u64, to_i64};
use super::{
    active_state, checkpoint, ActiveState, JournalOperation, JournalPosition,
    ProjectionJournalRecord, ProjectionJournalSnapshot, EMPTY_SHA256, PROJECTION_CONTRACT_VERSION,
    PROJECTION_JOURNAL_MAX_PAGE_BYTES, PROJECTION_JOURNAL_PAGE_SIZE,
};
use crate::{Result, Store, StoreError, CANONICAL_PROJECTION_SCHEMA_IDENTITY, SCHEMA_VERSION};

// Schema 47's persisted chunk constraint is 64. Group collection batches to
// that physical bound (which remains within the publication protocol's
// <=512-record chunk ceiling) without changing the frozen public schema.
pub(super) const PROJECTION_JOURNAL_CHUNK_SIZE: usize = 64;

impl Store {
    /// Reads one immutable, count- and byte-bounded page through one frozen high-water.
    pub fn projection_journal_snapshot(
        &self,
        after: Option<JournalPosition>,
    ) -> Result<ProjectionJournalSnapshot> {
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.conn.execute_batch("BEGIN")?;
        }
        let result = snapshot(&self.conn, after);
        if owns_transaction {
            match &result {
                Ok(_) => self.conn.execute_batch("COMMIT")?,
                Err(_) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                }
            }
        }
        result
    }
}

fn snapshot(
    conn: &rusqlite::Connection,
    after: Option<JournalPosition>,
) -> Result<ProjectionJournalSnapshot> {
    let state = active_state(conn)?.ok_or(StoreError::ProjectionJournalInactive)?;
    let after = after.unwrap_or(JournalPosition {
        generation: state.generation,
        sequence: state.acknowledged_sequence,
    });
    if after.generation != state.generation
        || after.sequence < state.acknowledged_sequence
        || after.sequence > state.high_water_sequence
    {
        return Err(StoreError::StaleProjectionJournalPosition {
            generation: after.generation,
            sequence: after.sequence,
            active_generation: state.generation,
        });
    }
    let records = read_records(conn, &state, after.sequence)?;
    let next_position = records
        .last()
        .map(|record| JournalPosition {
            generation: record.generation,
            sequence: record.sequence,
        })
        .unwrap_or(after);
    Ok(ProjectionJournalSnapshot {
        canonical_schema_version: u32::try_from(SCHEMA_VERSION).map_err(|_| {
            StoreError::InvalidProjectionJournalData("negative canonical schema version".to_owned())
        })?,
        canonical_schema_identity: CANONICAL_PROJECTION_SCHEMA_IDENTITY.to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        frozen_through: checkpoint(&state),
        authorized_repository_roots: authorized_repository_roots(conn)?,
        has_more: next_position.sequence < state.high_water_sequence,
        records,
        next_position,
    })
}

pub(super) fn digest_at_position(
    conn: &rusqlite::Connection,
    state: &ActiveState,
    sequence: u64,
) -> Result<String> {
    if sequence == state.acknowledged_sequence {
        return Ok(state.acknowledged_cumulative_digest.clone());
    }
    if sequence == state.high_water_sequence {
        return Ok(state.cumulative_digest.clone());
    }
    let encoded = conn
        .query_row(
            "SELECT records_zstd FROM projection_journal_chunks
             WHERE generation = ?1 AND first_sequence <= ?2 AND last_sequence >= ?2",
            params![
                to_i64(state.generation, "generation")?,
                to_i64(sequence, "sequence")?
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::InvalidProjectionJournalData(format!(
                "missing retained journal chunk at {}/{}",
                state.generation, sequence
            ))
        })?;
    decode_record_chunk(&encoded)?
        .into_iter()
        .find(|record| record.sequence == sequence)
        .map(|record| record.cumulative_digest)
        .ok_or_else(|| {
            StoreError::InvalidProjectionJournalData(format!(
                "missing retained journal digest at {}/{}",
                state.generation, sequence
            ))
        })
}

fn read_records(
    conn: &rusqlite::Connection,
    state: &ActiveState,
    after: u64,
) -> Result<Vec<ProjectionJournalRecord>> {
    let mut prior_digest = if after == state.acknowledged_sequence {
        state.acknowledged_cumulative_digest.clone()
    } else if after == 0 {
        generation_digest(state.generation, &state.contract_fingerprint)
    } else {
        retained_digest(conn, state.generation, after)?.ok_or(
            StoreError::StaleProjectionJournalPosition {
                generation: state.generation,
                sequence: after,
                active_generation: state.generation,
            },
        )?
    };
    let mut statement = conn.prepare(
        "SELECT first_sequence, last_sequence, record_count, uncompressed_bytes, records_zstd
         FROM projection_journal_chunks
         WHERE generation = ?1 AND last_sequence > ?2 AND first_sequence <= ?3
         ORDER BY first_sequence",
    )?;
    let rows = statement.query_map(
        params![
            to_i64(state.generation, "generation")?,
            to_i64(after, "sequence")?,
            to_i64(state.high_water_sequence, "sequence")?,
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        },
    )?;
    let mut records = Vec::new();
    let mut encoded_bytes = 0_usize;
    let mut expected_sequence = after.saturating_add(1);
    let mut page_limited = false;
    'chunks: for row in rows {
        let (first, last, count, uncompressed_bytes, encoded) = row?;
        let first = nonnegative_u64(first, "chunk first sequence")?;
        let last = nonnegative_u64(last, "chunk last sequence")?;
        let count = nonnegative_u64(count, "chunk record count")?;
        let uncompressed_bytes = nonnegative_u64(uncompressed_bytes, "chunk bytes")?;
        let chunk_records = decode_record_chunk(&encoded)?;
        if chunk_records.len() as u64 != count
            || serde_json::to_vec(&chunk_records)?.len() as u64 != uncompressed_bytes
            || chunk_records.first().map(|record| record.sequence) != Some(first)
            || chunk_records.last().map(|record| record.sequence) != Some(last)
        {
            return Err(StoreError::InvalidProjectionJournalData(format!(
                "journal chunk metadata mismatch at {}/{first}-{last}",
                state.generation
            )));
        }
        for record in chunk_records {
            if record.sequence <= after {
                continue;
            }
            if record.sequence > state.high_water_sequence {
                break 'chunks;
            }
            let record_bytes = serde_json::to_vec(&record)?.len();
            if records.len() == PROJECTION_JOURNAL_PAGE_SIZE
                || encoded_bytes.saturating_add(record_bytes) > PROJECTION_JOURNAL_MAX_PAGE_BYTES
            {
                page_limited = true;
                break 'chunks;
            }
            validate_persisted_record(&record, state.generation, expected_sequence, &prior_digest)?;
            encoded_bytes += record_bytes;
            prior_digest = record.cumulative_digest.clone();
            records.push(record);
            expected_sequence = expected_sequence.saturating_add(1);
        }
    }
    if !page_limited && expected_sequence <= state.high_water_sequence {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "journal ended before frozen high-water {}/{}",
            state.generation, state.high_water_sequence
        )));
    }
    if !page_limited
        && expected_sequence > state.high_water_sequence
        && prior_digest != state.cumulative_digest
    {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "frozen cumulative digest mismatch at {}/{}",
            state.generation, state.high_water_sequence
        )));
    }
    Ok(records)
}

fn retained_digest(
    conn: &rusqlite::Connection,
    generation: u64,
    sequence: u64,
) -> Result<Option<String>> {
    let encoded = conn
        .query_row(
            "SELECT records_zstd FROM projection_journal_chunks
             WHERE generation = ?1 AND first_sequence <= ?2 AND last_sequence >= ?2",
            params![
                to_i64(generation, "generation")?,
                to_i64(sequence, "sequence")?
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    encoded
        .map(|encoded| {
            Ok(decode_record_chunk(&encoded)?
                .into_iter()
                .find(|record| record.sequence == sequence)
                .map(|record| record.cumulative_digest))
        })
        .transpose()
        .map(Option::flatten)
}

fn validate_persisted_record(
    record: &ProjectionJournalRecord,
    expected_generation: u64,
    expected_sequence: u64,
    prior_digest: &str,
) -> Result<()> {
    if record.generation != expected_generation || record.sequence != expected_sequence {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "non-contiguous journal sequence: expected {expected_generation}/{expected_sequence}, got {}/{}",
            record.generation, record.sequence
        )));
    }
    if record.projection_contract_version != PROJECTION_CONTRACT_VERSION {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "unsupported projection contract version {} at {}/{}",
            record.projection_contract_version, record.generation, record.sequence
        )));
    }
    let canonical_payload_json = record
        .canonical_payload
        .as_ref()
        .map(canonical_json)
        .transpose()?;
    match (record.operation, canonical_payload_json.as_deref()) {
        (JournalOperation::Upsert, Some(payload))
            if sha256_hex(payload.as_bytes()) == record.payload_sha256 => {}
        (JournalOperation::Delete, None) if record.payload_sha256 == EMPTY_SHA256 => {}
        _ => {
            return Err(StoreError::InvalidProjectionJournalData(format!(
                "operation/payload digest mismatch at {}/{}",
                record.generation, record.sequence
            )));
        }
    }
    if record.evidence.len() > 32
        || record.provenance.entity_kind != record.entity_kind
        || record.provenance.stable_entity_id != record.stable_entity_id
    {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "evidence or provenance mismatch at {}/{}",
            record.generation, record.sequence
        )));
    }
    validate_stored_evidence(&record.evidence)?;
    validate_stored_provenance(&record.provenance)?;
    let evidence_json = canonical_json(&record.evidence)?;
    let provenance_json = canonical_json(&record.provenance)?;
    let expected_digest = record_chain_digest(
        prior_digest,
        record.generation,
        record.sequence,
        record.entity_kind,
        record.stable_entity_id,
        record.entity_revision,
        RecordDigestFields {
            operation: record.operation,
            payload_sha256: &record.payload_sha256,
            evidence_json: &evidence_json,
            provenance_json: &provenance_json,
        },
    )?;
    if expected_digest != record.cumulative_digest {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "cumulative digest mismatch at {}/{}",
            record.generation, record.sequence
        )));
    }
    Ok(())
}

pub(super) fn insert_record_chunk(
    conn: &rusqlite::Connection,
    records: &[ProjectionJournalRecord],
) -> Result<()> {
    let Some(first) = records.first() else {
        return Err(StoreError::InvalidProjectionJournalData(
            "cannot persist an empty journal chunk".to_owned(),
        ));
    };
    if records.len() > PROJECTION_JOURNAL_CHUNK_SIZE {
        return Err(StoreError::InvalidProjectionJournalData(
            "journal chunk exceeds the record bound".to_owned(),
        ));
    }
    for (offset, record) in records.iter().enumerate() {
        if record.generation != first.generation
            || record.sequence != first.sequence.saturating_add(offset as u64)
        {
            return Err(StoreError::InvalidProjectionJournalData(
                "journal chunk records are not contiguous".to_owned(),
            ));
        }
    }
    let encoded = serde_json::to_vec(records)?;
    if encoded.len() > PROJECTION_JOURNAL_MAX_PAGE_BYTES {
        return Err(StoreError::InvalidProjectionJournalData(
            "journal chunk exceeds the encoded byte bound".to_owned(),
        ));
    }
    let compressed = zstd::bulk::compress(&encoded, 1).map_err(|error| {
        StoreError::InvalidProjectionJournalData(format!("cannot compress journal chunk: {error}"))
    })?;
    let last = records.last().ok_or_else(|| {
        StoreError::InvalidProjectionJournalData("journal chunk lost its tail".to_owned())
    })?;
    conn.execute(
        "INSERT INTO projection_journal_chunks
             (generation, first_sequence, last_sequence, record_count,
              uncompressed_bytes, records_zstd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            to_i64(first.generation, "generation")?,
            to_i64(first.sequence, "first sequence")?,
            to_i64(last.sequence, "last sequence")?,
            i64::try_from(records.len()).map_err(|_| {
                StoreError::InvalidProjectionJournalData(
                    "journal chunk record count exceeds SQLite INTEGER".to_owned(),
                )
            })?,
            i64::try_from(encoded.len()).map_err(|_| {
                StoreError::InvalidProjectionJournalData(
                    "journal chunk byte count exceeds SQLite INTEGER".to_owned(),
                )
            })?,
            compressed,
        ],
    )?;
    Ok(())
}

pub(super) fn decode_record_chunk(value: &[u8]) -> Result<Vec<ProjectionJournalRecord>> {
    let bytes =
        zstd::bulk::decompress(value, PROJECTION_JOURNAL_MAX_PAGE_BYTES).map_err(|error| {
            StoreError::InvalidProjectionJournalData(format!(
                "cannot decompress journal chunk: {error}"
            ))
        })?;
    let records: Vec<ProjectionJournalRecord> = serde_json::from_slice(&bytes)?;
    if records.is_empty() || records.len() > PROJECTION_JOURNAL_CHUNK_SIZE {
        return Err(StoreError::InvalidProjectionJournalData(
            "decoded journal chunk violates its record bound".to_owned(),
        ));
    }
    Ok(records)
}

pub(super) fn prune_chunks_through(
    conn: &rusqlite::Connection,
    generation: u64,
    acknowledged_sequence: u64,
) -> Result<()> {
    if acknowledged_sequence == 0 {
        return Ok(());
    }
    let overlap = conn
        .query_row(
            "SELECT first_sequence, records_zstd FROM projection_journal_chunks
             WHERE generation = ?1 AND first_sequence <= ?2 AND last_sequence > ?2",
            params![
                to_i64(generation, "generation")?,
                to_i64(acknowledged_sequence, "acknowledged sequence")?
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let suffix = overlap
        .as_ref()
        .map(|(_, encoded)| decode_record_chunk(encoded))
        .transpose()?
        .map(|records| {
            records
                .into_iter()
                .filter(|record| record.sequence > acknowledged_sequence)
                .collect::<Vec<_>>()
        });
    conn.execute(
        "DELETE FROM projection_journal_chunks
         WHERE generation = ?1 AND last_sequence <= ?2",
        params![
            to_i64(generation, "generation")?,
            to_i64(acknowledged_sequence, "acknowledged sequence")?
        ],
    )?;
    if let Some((first, _)) = overlap {
        conn.execute(
            "DELETE FROM projection_journal_chunks
             WHERE generation = ?1 AND first_sequence = ?2",
            params![to_i64(generation, "generation")?, first],
        )?;
        let suffix = suffix.ok_or_else(|| {
            StoreError::InvalidProjectionJournalData(
                "overlapping journal chunk lost its retained suffix".to_owned(),
            )
        })?;
        if suffix.is_empty() {
            return Err(StoreError::InvalidProjectionJournalData(
                "overlapping journal chunk has no retained suffix".to_owned(),
            ));
        }
        insert_record_chunk(conn, &suffix)?;
    }
    Ok(())
}

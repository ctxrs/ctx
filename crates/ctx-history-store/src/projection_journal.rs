use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[cfg(test)]
use crate::FINAL_SCHEMA_IDENTITY;
use crate::{Result, Store, StoreError};

mod archive;
mod dependencies;
mod encoding;
mod fence;
mod impact;
mod pages;
mod protocol_validity;
mod records;
mod support;

pub(crate) use archive::{
    capture_archive_journal_dependencies, journal_archive_dependencies, journal_archive_mutations,
};
use encoding::{generation_digest, validate_contract_fingerprint, validate_digest};
pub(crate) use fence::ensure_projection_writer_fence;
use fence::{drop_projection_writer_fence, install_projection_writer_fence};
#[cfg(test)]
use pages::{decode_record_chunk, insert_record_chunk, json_array_encoded_bytes_after_push};
use pages::{
    digest_at_position, projection_context_available, prune_chunks_through, read_context_window,
};
#[cfg(test)]
use protocol_validity::MAX_AUTHORIZED_REPOSITORY_ROOTS;
use records::append_baseline;
pub(crate) use records::GroupJournalCollector;
use support::{current_time_ms, nonnegative_u64, to_i64};

/// Drops the replaceable handoff before a canonical same-version schema rewrite.
/// Canonical rows remain authoritative and the next Pro setup creates a fresh
/// generation under the current protocol fingerprint.
pub(crate) fn reset_for_canonical_schema_rewrite(conn: &rusqlite::Connection) -> Result<()> {
    drop_projection_writer_fence(conn)?;
    conn.execute("DELETE FROM projection_journal_entities", [])?;
    conn.execute("DELETE FROM projection_journal_chunks", [])?;
    conn.execute(
        "UPDATE projection_journal_state SET active = 0,
             contract_fingerprint = NULL, high_water_sequence = 0,
             cumulative_digest = ?1, acknowledged_sequence = 0,
             acknowledged_cumulative_digest = ?1, activated_at_ms = NULL
         WHERE singleton = 1",
        [ZERO_DIGEST],
    )?;
    Ok(())
}

pub const PROJECTION_CONTRACT_VERSION: u32 = 1;
pub const PROJECTION_JOURNAL_PAGE_SIZE: usize = 512;
pub const PROJECTION_JOURNAL_CONTEXT_RECORDS: usize = 64;
pub const PROJECTION_JOURNAL_CONTEXT_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const PROJECTION_JOURNAL_RECORD_MAX_BYTES: usize = 3 * 1024 * 1024;
pub const PROJECTION_JOURNAL_MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntityKind {
    Event,
    FileTouch,
    VcsChange,
}

impl JournalEntityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::FileTouch => "file_touch",
            Self::VcsChange => "vcs_change",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperation {
    Upsert,
    Delete,
}

impl JournalOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalPosition {
    pub generation: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalCheckpoint {
    pub position: JournalPosition,
    pub contract_fingerprint: String,
    pub cumulative_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEvidenceIdentity {
    pub event_id: Uuid,
    pub source_id: Option<Uuid>,
    pub source_path: Option<String>,
    pub source_record_ordinal: Option<u64>,
    pub source_record_subrecord_index: Option<u32>,
    pub byte_start: Option<u64>,
    pub byte_end_exclusive: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalProvenanceIdentity {
    pub entity_kind: JournalEntityKind,
    pub stable_entity_id: Uuid,
    pub capture_source_id: Option<Uuid>,
    pub provider: Option<String>,
    pub provider_external_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionJournalRecord {
    pub generation: u64,
    pub sequence: u64,
    pub projection_contract_version: u32,
    pub entity_kind: JournalEntityKind,
    pub stable_entity_id: Uuid,
    pub entity_revision: u64,
    pub operation: JournalOperation,
    pub canonical_payload: Option<Value>,
    pub payload_sha256: String,
    pub evidence: Vec<JournalEvidenceIdentity>,
    pub provenance: JournalProvenanceIdentity,
    pub cumulative_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionJournalSnapshot {
    pub canonical_schema_version: u32,
    pub canonical_schema_identity: String,
    pub projection_contract_version: u32,
    pub frozen_through: JournalCheckpoint,
    pub context: ProjectionJournalContextWindow,
    /// Bounded, activity-observed repository candidates. The private helper
    /// must still revalidate each locator as a Git repository before reading.
    pub authorized_repository_roots: Vec<String>,
    pub records: Vec<ProjectionJournalRecord>,
    pub next_position: JournalPosition,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionJournalContextWindow {
    pub base_checkpoint: JournalCheckpoint,
    pub records: Vec<ProjectionJournalRecord>,
}

#[derive(Debug)]
struct ActiveState {
    generation: u64,
    contract_fingerprint: String,
    high_water_sequence: u64,
    cumulative_digest: String,
    acknowledged_sequence: u64,
    acknowledged_cumulative_digest: String,
}

impl Store {
    /// Activates the local Pro handoff and atomically writes a deterministic baseline.
    /// Repeating activation for the same active contract is a no-op. A changed contract
    /// starts a new generation; older immutable generations remain available for diagnosis.
    pub fn activate_projection_journal(
        &self,
        contract_fingerprint: &str,
    ) -> Result<JournalCheckpoint> {
        self.reject_projection_journal_lifecycle_in_native_path_group()?;
        validate_contract_fingerprint(contract_fingerprint)?;
        let result = self.with_atomic_write(|| {
            install_projection_writer_fence(&self.conn)?;
            if let Some(state) = active_state(&self.conn)? {
                if state.contract_fingerprint == contract_fingerprint {
                    return Ok(checkpoint(&state));
                }
            }
            start_generation(&self.conn, contract_fingerprint)
        });
        if result.is_ok() {
            self.remember_projection_journal_activity(true);
        }
        result
    }

    /// Reconciles the Store's retained suffix with the helper's durable checkpoint.
    /// A missing or stale helper after acknowledged records were pruned starts a new
    /// deterministic baseline generation from the canonical Store.
    pub fn reconcile_projection_journal(
        &self,
        helper_checkpoint: Option<&JournalCheckpoint>,
    ) -> Result<JournalCheckpoint> {
        self.reject_projection_journal_lifecycle_in_native_path_group()?;
        self.with_atomic_write(|| {
            let state = active_state(&self.conn)?.ok_or(StoreError::ProjectionJournalInactive)?;
            let Some(helper) = helper_checkpoint else {
                return if state.acknowledged_sequence == 0 {
                    Ok(checkpoint(&state))
                } else {
                    start_generation(&self.conn, &state.contract_fingerprint)
                };
            };
            validate_contract_fingerprint(&helper.contract_fingerprint)?;
            validate_digest(&helper.cumulative_digest, "helper cumulative digest")?;
            if helper.contract_fingerprint != state.contract_fingerprint
                || helper.position.generation != state.generation
                || helper.position.sequence > state.high_water_sequence
                || helper.position.sequence < state.acknowledged_sequence
            {
                return start_generation(&self.conn, &state.contract_fingerprint);
            }
            if !projection_context_available(&self.conn, &state, helper.position.sequence)? {
                return start_generation(&self.conn, &state.contract_fingerprint);
            }
            acknowledge_checkpoint(&self.conn, &state, helper)?;
            let current = active_state(&self.conn)?.ok_or(StoreError::ProjectionJournalInactive)?;
            Ok(checkpoint(&current))
        })
    }

    /// Publishes a helper acknowledgement and prunes the committed prefix except
    /// for the bounded look-behind needed by the next helper request. The
    /// checkpoint update and deletion share one SQLite transaction.
    pub fn acknowledge_projection_journal(&self, acknowledged: &JournalCheckpoint) -> Result<()> {
        self.reject_projection_journal_lifecycle_in_native_path_group()?;
        self.with_atomic_write(|| {
            let state = active_state(&self.conn)?.ok_or(StoreError::ProjectionJournalInactive)?;
            acknowledge_checkpoint(&self.conn, &state, acknowledged)
        })
    }

    /// Deletes every derived handoff record while retaining only the next-generation counter.
    pub fn disable_projection_journal(&self) -> Result<()> {
        self.reject_projection_journal_lifecycle_in_native_path_group()?;
        let result = self.with_atomic_write(|| {
            drop_projection_writer_fence(&self.conn)?;
            self.conn
                .execute("DELETE FROM projection_journal_entities", [])?;
            self.conn
                .execute("DELETE FROM projection_journal_chunks", [])?;
            self.conn.execute(
                "UPDATE projection_journal_state SET active = 0,
                     contract_fingerprint = NULL, high_water_sequence = 0,
                     cumulative_digest = ?1, acknowledged_sequence = 0,
                     acknowledged_cumulative_digest = ?1, activated_at_ms = NULL
                 WHERE singleton = 1",
                [ZERO_DIGEST],
            )?;
            Ok(())
        });
        if result.is_ok() {
            self.remember_projection_journal_activity(false);
        }
        result
    }

    pub(crate) fn projection_journal_active_for_mutation(&self) -> Result<bool> {
        if self.batch_depth.get() == 0 {
            return Ok(active_state(&self.conn)?.is_some());
        }
        if let Some(active) = self.projection_journal_active_in_batch.get() {
            return Ok(active);
        }
        let active = active_state(&self.conn)?.is_some();
        self.projection_journal_active_in_batch.set(Some(active));
        Ok(active)
    }

    fn remember_projection_journal_activity(&self, active: bool) {
        if self.batch_depth.get() > 0 {
            self.projection_journal_active_in_batch.set(Some(active));
        }
    }

    fn reject_projection_journal_lifecycle_in_native_path_group(&self) -> Result<()> {
        if self.native_path_group_token.get().is_some() {
            self.poison_native_path_group();
            return Err(StoreError::NativePathJournalLifecycleDuringGroup);
        }
        Ok(())
    }

    pub(crate) fn projection_journal_checkpoint_in_transaction(
        &self,
    ) -> Result<Option<JournalCheckpoint>> {
        active_state(&self.conn).map(|state| state.as_ref().map(checkpoint))
    }

    /// Returns the exact active high-water checkpoint without materializing
    /// retained journal records.
    pub fn projection_journal_checkpoint(&self) -> Result<JournalCheckpoint> {
        self.reject_projection_journal_lifecycle_in_native_path_group()?;
        active_state(&self.conn)?
            .as_ref()
            .map(checkpoint)
            .ok_or(StoreError::ProjectionJournalInactive)
    }

    /// Verifies one checkpoint against the active retained journal without
    /// materializing a journal page.
    ///
    /// High-water and acknowledged checkpoints are resolved from the exact
    /// journal state row. Retained interior checkpoints decode only their one
    /// containing bounded chunk.
    pub fn verify_projection_journal_checkpoint(
        &self,
        candidate: &JournalCheckpoint,
    ) -> Result<bool> {
        self.reject_projection_journal_lifecycle_in_native_path_group()?;
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.conn.execute_batch("BEGIN")?;
        }
        let result = self.verify_projection_journal_checkpoint_in_transaction(Some(candidate));
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

    pub(crate) fn verify_projection_journal_checkpoint_in_transaction(
        &self,
        candidate: Option<&JournalCheckpoint>,
    ) -> Result<bool> {
        let Some(state) = active_state(&self.conn)? else {
            return Ok(candidate.is_none());
        };
        let Some(candidate) = candidate else {
            return Ok(false);
        };
        if candidate.contract_fingerprint != state.contract_fingerprint
            || candidate.position.generation != state.generation
            || candidate.position.sequence < state.acknowledged_sequence
            || candidate.position.sequence > state.high_water_sequence
        {
            return Ok(false);
        }
        Ok(
            digest_at_position(&self.conn, &state, candidate.position.sequence)?
                == candidate.cumulative_digest,
        )
    }
}

fn start_generation(
    conn: &rusqlite::Connection,
    contract_fingerprint: &str,
) -> Result<JournalCheckpoint> {
    let previous_generation = conn.query_row(
        "SELECT generation FROM projection_journal_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let generation = nonnegative_u64(previous_generation, "generation")?
        .checked_add(1)
        .ok_or_else(|| {
            StoreError::InvalidProjectionJournalData("journal generation overflow".to_owned())
        })?;
    let genesis = generation_digest(generation, contract_fingerprint);

    // Only one replaceable handoff generation is retained. Canonical Store rows
    // remain authoritative and can always regenerate this baseline.
    conn.execute("DELETE FROM projection_journal_entities", [])?;
    conn.execute("DELETE FROM projection_journal_chunks", [])?;
    conn.execute(
        "UPDATE projection_journal_state SET
             active = 1, generation = ?1, projection_contract_version = ?2,
             contract_fingerprint = ?3, high_water_sequence = 0,
             cumulative_digest = ?4, acknowledged_sequence = 0,
             acknowledged_cumulative_digest = ?4, activated_at_ms = ?5
         WHERE singleton = 1",
        params![
            to_i64(generation, "generation")?,
            i64::from(PROJECTION_CONTRACT_VERSION),
            contract_fingerprint,
            genesis,
            current_time_ms(),
        ],
    )?;

    append_baseline(conn)?;
    let state = active_state(conn)?.ok_or(StoreError::ProjectionJournalInactive)?;
    Ok(checkpoint(&state))
}

fn acknowledge_checkpoint(
    conn: &rusqlite::Connection,
    state: &ActiveState,
    acknowledged: &JournalCheckpoint,
) -> Result<()> {
    validate_contract_fingerprint(&acknowledged.contract_fingerprint)?;
    validate_digest(
        &acknowledged.cumulative_digest,
        "acknowledged cumulative digest",
    )?;
    if acknowledged.contract_fingerprint != state.contract_fingerprint
        || acknowledged.position.generation != state.generation
        || acknowledged.position.sequence < state.acknowledged_sequence
        || acknowledged.position.sequence > state.high_water_sequence
    {
        return Err(StoreError::StaleProjectionJournalPosition {
            generation: acknowledged.position.generation,
            sequence: acknowledged.position.sequence,
            active_generation: state.generation,
        });
    }
    let expected_digest = digest_at_position(conn, state, acknowledged.position.sequence)?;
    if expected_digest != acknowledged.cumulative_digest {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "acknowledgement digest mismatch at {}/{}",
            acknowledged.position.generation, acknowledged.position.sequence
        )));
    }
    if acknowledged.position.sequence == state.acknowledged_sequence {
        return Ok(());
    }
    let retained_context = read_context_window(conn, state, acknowledged.position.sequence)?;
    conn.execute(
        "UPDATE projection_journal_state
         SET acknowledged_sequence = ?1, acknowledged_cumulative_digest = ?2
         WHERE singleton = 1",
        params![
            to_i64(acknowledged.position.sequence, "acknowledged sequence")?,
            acknowledged.cumulative_digest,
        ],
    )?;
    // The transmitted context is the largest suffix satisfying both its count
    // and byte bounds. Retain exactly that suffix plus its immediate physical
    // predecessor when nonzero so the next request can recover the base digest.
    // All records after the acknowledgement are above this pruning threshold.
    let prune_through = retained_context
        .base_checkpoint
        .position
        .sequence
        .saturating_sub(1);
    prune_chunks_through(conn, state.generation, prune_through)?;
    Ok(())
}

fn active_state(conn: &rusqlite::Connection) -> Result<Option<ActiveState>> {
    conn.query_row(
        "SELECT generation, projection_contract_version, contract_fingerprint,
                high_water_sequence, cumulative_digest, acknowledged_sequence,
                acknowledged_cumulative_digest
         FROM projection_journal_state WHERE singleton = 1 AND active = 1",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        },
    )
    .optional()?
    .map(
        |(
            generation,
            version,
            contract_fingerprint,
            high_water_sequence,
            cumulative_digest,
            acknowledged_sequence,
            acknowledged_cumulative_digest,
        )| {
            if version != i64::from(PROJECTION_CONTRACT_VERSION) {
                return Err(StoreError::InvalidProjectionJournalData(format!(
                    "unsupported projection contract version {version}"
                )));
            }
            validate_contract_fingerprint(&contract_fingerprint)?;
            validate_digest(&cumulative_digest, "state cumulative digest")?;
            validate_digest(
                &acknowledged_cumulative_digest,
                "state acknowledged cumulative digest",
            )?;
            let high_water_sequence = nonnegative_u64(high_water_sequence, "sequence")?;
            let acknowledged_sequence =
                nonnegative_u64(acknowledged_sequence, "acknowledged sequence")?;
            if acknowledged_sequence > high_water_sequence {
                return Err(StoreError::InvalidProjectionJournalData(
                    "acknowledged sequence exceeds journal high-water".to_owned(),
                ));
            }
            Ok(ActiveState {
                generation: nonnegative_u64(generation, "generation")?,
                contract_fingerprint,
                high_water_sequence,
                cumulative_digest,
                acknowledged_sequence,
                acknowledged_cumulative_digest,
            })
        },
    )
    .transpose()
}

fn checkpoint(state: &ActiveState) -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: state.generation,
            sequence: state.high_water_sequence,
        },
        contract_fingerprint: state.contract_fingerprint.clone(),
        cumulative_digest: state.cumulative_digest.clone(),
    }
}

#[cfg(test)]
mod adversarial_tests;
#[cfg(test)]
mod tests;

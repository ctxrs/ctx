use std::collections::{BTreeMap, BTreeSet};

use super::{
    manifest::ValidatedGeneration,
    materialization::{materialize_records, validate_projected_generation},
    sqlite_u64, CommittedCoreGeneration, RelationalProjectionError, RelationalProjectionPlan,
    RelationalProjectionReceipt, RelationalProjectionRecord, RelationalProjectionStatus, Result,
    SourceBackedRelationalProjection, RELATIONAL_MATERIALIZER_REVISION,
    RELATIONAL_PROJECTION_SCHEMA_VERSION,
};
use rusqlite::{params, Connection, TransactionBehavior};

const MAX_FAILURE_DETAIL_CHARS: usize = 2_048;

#[derive(Debug, Clone, Copy)]
enum BuildMode {
    Rebuild,
    CatchUp,
}

impl SourceBackedRelationalProjection {
    /// Computes the exact work required without opening Core pages.
    pub fn plan_generation(
        &self,
        generation: &CommittedCoreGeneration,
    ) -> Result<RelationalProjectionPlan> {
        let validated = ValidatedGeneration::from_commit(generation)?;
        let metadata = self.metadata()?;
        if metadata.status == RelationalProjectionStatus::Ready
            && metadata.active_core_generation_id.as_deref() == Some(&generation.generation_id)
            && metadata.active_manifest_version == Some(generation.manifest_version)
            && metadata.active_materializer_revision == Some(RELATIONAL_MATERIALIZER_REVISION)
            && metadata.active_core_record_version == Some(generation.core_record_version)
            && metadata.active_core_record_contract_fingerprint.as_deref()
                == Some(&generation.core_record_contract_fingerprint)
            && metadata.active_lexical_schema_version == Some(generation.lexical_schema_version)
            && metadata.active_policy_schema_hash.as_deref() == Some(&generation.policy_schema_hash)
            && metadata.target_core_generation_id.is_none()
            && usize::try_from(metadata.source_count) == Ok(generation.sources.len())
            && metadata.event_count == generation.indexed_documents
        {
            return Ok(RelationalProjectionPlan::NoOp(receipt_from_metadata(
                &generation.generation_id,
                &metadata,
            )));
        }
        if metadata.status == RelationalProjectionStatus::Empty
            || metadata.active_materializer_revision != Some(RELATIONAL_MATERIALIZER_REVISION)
            || metadata.active_core_record_version != Some(generation.core_record_version)
            || metadata.active_core_record_contract_fingerprint.as_deref()
                != Some(&generation.core_record_contract_fingerprint)
        {
            return Ok(RelationalProjectionPlan::Rebuild);
        }

        let prior = stored_source_revisions(&self.conn)?;
        let changed_source_ids = validated
            .sources
            .iter()
            .filter(|(source_id, source)| prior.get(*source_id) != Some(&source.revision_digest))
            .map(|(_, source)| source.source.identity().as_uuid())
            .collect();
        Ok(RelationalProjectionPlan::CatchUp { changed_source_ids })
    }

    pub fn rebuild<I>(
        &mut self,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = RelationalProjectionRecord>,
    {
        self.rebuild_stream(generation, records.into_iter().map(Ok))
    }

    pub fn rebuild_stream<I>(
        &mut self,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = Result<RelationalProjectionRecord>>,
    {
        self.apply_generation(BuildMode::Rebuild, generation, records)
    }

    pub fn catch_up<I>(
        &mut self,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = RelationalProjectionRecord>,
    {
        self.catch_up_stream(generation, records.into_iter().map(Ok))
    }

    pub fn catch_up_stream<I>(
        &mut self,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = Result<RelationalProjectionRecord>>,
    {
        self.apply_generation(BuildMode::CatchUp, generation, records)
    }

    fn apply_generation<I>(
        &mut self,
        requested_mode: BuildMode,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = Result<RelationalProjectionRecord>>,
    {
        if self.read_only {
            return Err(RelationalProjectionError::InvalidStreamOrder(
                "a read-only SQL projection cannot publish a generation".to_owned(),
            ));
        }
        let plan = self.plan_generation(generation)?;
        if let RelationalProjectionPlan::NoOp(receipt) = plan {
            return Ok(receipt);
        }
        let mode = match (requested_mode, plan) {
            (_, RelationalProjectionPlan::Rebuild) | (BuildMode::Rebuild, _) => BuildMode::Rebuild,
            (BuildMode::CatchUp, RelationalProjectionPlan::CatchUp { .. }) => BuildMode::CatchUp,
            (_, RelationalProjectionPlan::NoOp(receipt)) => return Ok(receipt),
        };
        let validated = ValidatedGeneration::from_commit(generation)?;
        let result = apply_transaction(
            &mut self.conn,
            mode,
            generation,
            &validated,
            records.into_iter(),
        );
        if let Err(error) = &result {
            note_failed_target(&self.conn, &generation.generation_id, error);
        }
        result
    }
}

fn apply_transaction<I>(
    conn: &mut Connection,
    mode: BuildMode,
    generation: &CommittedCoreGeneration,
    validated: &ValidatedGeneration,
    records: I,
) -> Result<RelationalProjectionReceipt>
where
    I: Iterator<Item = Result<RelationalProjectionRecord>>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior = stored_source_revisions(&tx)?;
    let generation_ids = validated.sources.keys().cloned().collect::<BTreeSet<_>>();
    let expected = match mode {
        BuildMode::Rebuild => {
            tx.execute("DELETE FROM core_sources", [])?;
            generation_ids.clone()
        }
        BuildMode::CatchUp => {
            let changed = validated
                .sources
                .iter()
                .filter(|(source_id, source)| {
                    prior.get(*source_id) != Some(&source.revision_digest)
                })
                .map(|(source_id, _)| source_id.clone())
                .collect::<BTreeSet<_>>();
            let removed = prior
                .keys()
                .filter(|source_id| !generation_ids.contains(*source_id))
                .cloned()
                .collect::<BTreeSet<_>>();
            for source_id in changed.union(&removed) {
                tx.execute("DELETE FROM core_sources WHERE source_id = ?1", [source_id])?;
            }
            changed
        }
    };

    materialize_records(&tx, expected, validated, records)?;
    let counts = validate_projected_generation(&tx, validated)?;
    let prior_build: i64 = tx.query_row(
        "SELECT build_generation FROM core_relational_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let build_generation =
        prior_build
            .checked_add(1)
            .ok_or(RelationalProjectionError::CountOverflow(
                "projection build generation",
            ))?;
    tx.execute(
        "UPDATE core_relational_state
         SET build_generation = ?1,
             active_generation_id = ?2,
             active_manifest_version = ?3,
             active_core_record_version = ?4,
             active_core_record_contract_fingerprint = ?5,
             active_lexical_schema_version = ?6,
             active_policy_schema_hash = ?7,
             active_materializer_revision = ?8,
             target_generation_id = NULL,
             status = 'ready',
             source_count = ?9,
             session_count = ?10,
             event_count = ?11,
             repository_binding_count = ?12,
             file_observation_count = ?13,
             vcs_observation_count = ?14,
             last_error = NULL
         WHERE singleton = 1",
        params![
            build_generation,
            generation.generation_id,
            i64::from(generation.manifest_version),
            i64::from(generation.core_record_version),
            generation.core_record_contract_fingerprint,
            i64::from(generation.lexical_schema_version),
            generation.policy_schema_hash,
            i64::from(RELATIONAL_MATERIALIZER_REVISION),
            counts.sources,
            counts.sessions,
            counts.events,
            counts.repository_bindings,
            counts.file_observations,
            counts.vcs_observations,
        ],
    )?;
    tx.commit()?;
    Ok(RelationalProjectionReceipt {
        core_generation_id: generation.generation_id.clone(),
        relational_schema_version: RELATIONAL_PROJECTION_SCHEMA_VERSION,
        materializer_revision: RELATIONAL_MATERIALIZER_REVISION,
        build_generation: sqlite_u64(build_generation, "build generation")?,
        source_count: sqlite_u64(counts.sources, "source count")?,
        session_count: sqlite_u64(counts.sessions, "session count")?,
        event_count: sqlite_u64(counts.events, "event count")?,
        repository_binding_count: sqlite_u64(
            counts.repository_bindings,
            "repository binding count",
        )?,
        file_touch_count: sqlite_u64(counts.file_observations, "file observation count")?,
        vcs_observation_count: sqlite_u64(counts.vcs_observations, "VCS observation count")?,
    })
}

fn stored_source_revisions(conn: &Connection) -> Result<BTreeMap<String, [u8; 32]>> {
    let mut statement = conn.prepare("SELECT source_id, revision_digest FROM core_sources")?;
    let mut rows = statement.query([])?;
    let mut revisions = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let source_id: String = row.get(0)?;
        let digest: Vec<u8> = row.get(1)?;
        let digest = digest.try_into().map_err(|_| {
            RelationalProjectionError::IncompatibleState(
                "stored source revision digest is malformed".to_owned(),
            )
        })?;
        revisions.insert(source_id, digest);
    }
    Ok(revisions)
}

fn receipt_from_metadata(
    core_generation_id: &str,
    metadata: &super::RelationalProjectionMetadata,
) -> RelationalProjectionReceipt {
    RelationalProjectionReceipt {
        core_generation_id: core_generation_id.to_owned(),
        relational_schema_version: RELATIONAL_PROJECTION_SCHEMA_VERSION,
        materializer_revision: RELATIONAL_MATERIALIZER_REVISION,
        build_generation: metadata.build_generation,
        source_count: metadata.source_count,
        session_count: metadata.session_count,
        event_count: metadata.event_count,
        repository_binding_count: metadata.repository_binding_count,
        file_touch_count: metadata.file_touch_count,
        vcs_observation_count: metadata.vcs_observation_count,
    }
}

fn note_failed_target(conn: &Connection, generation_id: &str, error: &RelationalProjectionError) {
    let detail = error
        .to_string()
        .chars()
        .take(MAX_FAILURE_DETAIL_CHARS)
        .collect::<String>();
    let _ = conn.execute(
        "UPDATE core_relational_state
         SET target_generation_id = ?1, status = 'behind', last_error = ?2
         WHERE singleton = 1",
        params![generation_id, detail],
    );
}

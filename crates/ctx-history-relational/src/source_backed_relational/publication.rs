use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, TransactionBehavior};

use super::{
    manifest::{invalid_generation, ValidatedManifest},
    materialization::{materialize_records, projection_counts, validate_projected_generation},
    sqlite_u64, CommittedCoreGeneration, RelationalProjectionError, RelationalProjectionReceipt,
    RelationalProjectionRecord, Result, SourceBackedRelationalProjection,
    GENERATION_MANIFEST_VERSION, REQUIRED_LEXICAL_SCHEMA_VERSION,
};

const MAX_FAILURE_DETAIL_CHARS: usize = 2_048;

#[derive(Debug, Clone, Copy)]
enum BuildMode {
    Rebuild,
    CatchUp,
}

impl SourceBackedRelationalProjection {
    /// Replaces the complete relational projection from one Core generation.
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

    /// Replaces the complete relational projection from a fallible record
    /// stream. A producer error rolls back the generation transaction.
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

    /// Advances only changed sources and retires sources omitted by the new
    /// certified manifest.
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

    /// Advances changed sources from a fallible record stream. A producer
    /// error rolls back the generation transaction.
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
        mode: BuildMode,
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
        let manifest = ValidatedManifest::from_commit(generation)?;
        let result = apply_transaction(
            &mut self.conn,
            mode,
            generation,
            &manifest,
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
    manifest: &ValidatedManifest,
    records: I,
) -> Result<RelationalProjectionReceipt>
where
    I: Iterator<Item = Result<RelationalProjectionRecord>>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior = stored_certificate_digests(&tx)?;
    let manifest_ids = manifest.sources.keys().cloned().collect::<BTreeSet<_>>();
    let expected = match mode {
        BuildMode::Rebuild => {
            tx.execute("DELETE FROM source_backed_sources", [])?;
            manifest_ids.clone()
        }
        BuildMode::CatchUp => {
            let changed = manifest
                .sources
                .iter()
                .filter(|&(source_id, source)| {
                    prior.get(source_id) != Some(&source.certificate_digest)
                })
                .map(|(source_id, _)| source_id.clone())
                .collect::<BTreeSet<_>>();
            for source_id in prior.keys().filter(|id| !manifest_ids.contains(*id)) {
                if !manifest.removal_ids.contains(source_id) {
                    return invalid_generation(format!(
                        "source {source_id} is omitted without durable certified deletion evidence"
                    ));
                }
                tx.execute(
                    "DELETE FROM source_backed_sources WHERE source_id = ?1",
                    [source_id],
                )?;
            }
            for source_id in &changed {
                tx.execute(
                    "DELETE FROM source_backed_sources WHERE source_id = ?1",
                    [source_id],
                )?;
            }
            changed
        }
    };

    materialize_records(&tx, expected, manifest, records)?;
    validate_projected_generation(&tx, manifest)?;
    let counts = projection_counts(&tx)?;
    let prior_build: i64 = tx.query_row(
        "SELECT build_generation FROM source_backed_relational_state WHERE singleton = 1",
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
        "UPDATE source_backed_relational_state
         SET build_generation = ?1,
             active_generation_id = ?2,
             active_manifest_digest = ?3,
             active_manifest_version = ?4,
             active_lexical_schema_version = ?5,
             active_policy_schema_hash = ?6,
             target_generation_id = NULL,
             status = 'ready',
             source_count = ?7,
             session_count = ?8,
             event_count = ?9,
             file_touch_count = ?10,
             last_error = NULL
         WHERE singleton = 1",
        params![
            build_generation,
            generation.generation_id,
            manifest.digest.as_slice(),
            GENERATION_MANIFEST_VERSION,
            REQUIRED_LEXICAL_SCHEMA_VERSION,
            manifest.policy_schema_hash,
            counts.sources,
            counts.sessions,
            counts.events,
            counts.file_touches,
        ],
    )?;
    tx.commit()?;
    Ok(RelationalProjectionReceipt {
        core_generation_id: generation.generation_id.clone(),
        build_generation: sqlite_u64(build_generation, "build_generation")?,
        source_count: sqlite_u64(counts.sources, "source_count")?,
        session_count: sqlite_u64(counts.sessions, "session_count")?,
        event_count: sqlite_u64(counts.events, "event_count")?,
        file_touch_count: sqlite_u64(counts.file_touches, "file_touch_count")?,
    })
}

fn stored_certificate_digests(conn: &Connection) -> Result<BTreeMap<String, [u8; 32]>> {
    let mut stmt =
        conn.prepare("SELECT source_id, certificate_digest FROM source_backed_sources")?;
    let mut rows = stmt.query([])?;
    let mut output = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let source_id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        let digest: [u8; 32] = bytes.try_into().map_err(|_| {
            RelationalProjectionError::InvalidRecord(
                "stored source certificate digest is malformed".to_owned(),
            )
        })?;
        output.insert(source_id, digest);
    }
    Ok(output)
}

fn note_failed_target(conn: &Connection, generation_id: &str, error: &RelationalProjectionError) {
    let detail = error
        .to_string()
        .chars()
        .take(MAX_FAILURE_DETAIL_CHARS)
        .collect::<String>();
    let _ = conn.execute(
        "UPDATE source_backed_relational_state
         SET target_generation_id = ?1, status = 'behind', last_error = ?2
         WHERE singleton = 1",
        params![generation_id, detail],
    );
}

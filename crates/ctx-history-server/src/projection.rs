use crate::{
    auth::authorize_any,
    publication::{visit_payload, Descriptor},
    types::collection_id,
    *,
};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, ScannedSourceCounts,
    SourceInventoryObservation, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationStateEnvelope, GenerationWriter, VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

const COVERAGE_FORMAT: &str = "ctx.hosted-coverage.v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Coverage {
    collection: String,
    sequence: u64,
}

struct RevisionWork {
    source: SourceKey,
    payload: UploadSpec,
    descriptor: Descriptor,
    live: bool,
}

impl HistoryServer {
    /// Index a bounded committed prefix. The generation-owned coverage is the
    /// only searchable frontier; pending SQL rows can safely survive a crash
    /// after generation activation and are reconciled on the next call.
    pub fn index_pending(
        &self,
        collection: &str,
        maximum_operations: u64,
    ) -> Result<CollectionStatus> {
        let started = std::time::Instant::now();
        let mut facts = ServerIndexFacts::default();
        let result = self.index_pending_observed(collection, maximum_operations, &mut facts);
        self.observe(ServerObservation::Index {
            duration: started.elapsed(),
            failure: result.as_ref().err().map(ServerFailure::from),
            facts,
        });
        result
    }

    fn index_pending_observed(
        &self,
        collection: &str,
        maximum_operations: u64,
        facts: &mut ServerIndexFacts,
    ) -> Result<CollectionStatus> {
        collection_id(collection)?;
        if maximum_operations == 0 || maximum_operations > 256 {
            return Err(Error::Invalid("index operation budget must be 1..256"));
        }
        let _writer = self.projection.lock().map_err(|_| Error::Unavailable)?;
        let connection = self.lock()?;
        let status = self.status_locked(&connection, collection)?;
        facts.coverage_lag = status
            .stored_sequence
            .checked_sub(status.searchable_sequence);
        facts.reads_available = Some(status.reads_available);
        if status.searchable_sequence == status.stored_sequence {
            connection.execute(
                "DELETE FROM pending WHERE collection=?1 AND sequence<=?2",
                params![collection, status.searchable_sequence],
            )?;
            facts.processed_operations = Some(0);
            facts.records = Some(0);
            facts.bytes = Some(0);
            return Ok(status);
        }
        let target = status.stored_sequence.min(
            status
                .searchable_sequence
                .saturating_add(maximum_operations),
        );
        // At most two revisions per accepted operation. Receipts own the
        // transition history, including A -> B -> A; immutable revision rows
        // and event locators are reused rather than assigned one lifetime.
        let work = revision_work(&connection, collection, status.searchable_sequence, target)?;
        facts.records = Some(0);
        facts.bytes = Some(0);
        drop(connection);
        #[cfg(test)]
        self.hooks.run(crate::tests::repair::Stage::Projection);
        let index_root = self.collection_root(collection).join("index");
        let mut writer = GenerationWriter::open_with_expected_base_generation(
            &index_root,
            WriterOptions {
                indexer_threads: 1,
                memory_bytes: self.config.index_memory_bytes,
            },
            status.generation.as_deref(),
        )?
        .into_writer()
        .map_err(|_| Error::Unavailable)?;
        for RevisionWork {
            source,
            payload,
            descriptor,
            live,
        } in work
        {
            if !live {
                let observation = SourceInventoryObservation::new(
                    "archive",
                    "hosted-revision",
                    TypedKey::bytes(source.identity().digest().to_vec())?,
                    "hosted-sequence",
                    target.to_be_bytes().to_vec(),
                )?;
                let inventory = CertifiedSourceInventory::certify(
                    observation.clone(),
                    observation,
                    "hosted-v1",
                    vec![],
                )?;
                writer.delete_source(
                    CertifiedSourceDeletion::from_inventory(source, &inventory)?,
                    inventory,
                )?;
            } else {
                writer.begin_source(source.clone())?;
                let path = self
                    .collection_root(collection)
                    .join("payloads")
                    .join(&payload.sha256);
                let visited = visit_payload(&path, &descriptor.member, |record| {
                    writer.add_core_record(ctx_history_archive::map_record(
                        &descriptor.binding,
                        &descriptor.member,
                        record,
                    )?)?;
                    facts.records = facts.records.map(|count| count.saturating_add(1));
                    Ok(())
                });
                if visited.is_err() {
                    facts.bytes = None;
                }
                visited?;
                facts.bytes = facts.bytes.map(|bytes| bytes.saturating_add(payload.bytes));
                let digest: [u8; 32] = hex::decode(&payload.sha256)
                    .map_err(|_| Error::Unavailable)?
                    .try_into()
                    .map_err(|_| Error::Unavailable)?;
                let observation =
                    SourceObservation::new(source, "archive-member-sha256-v1", digest.to_vec())?;
                writer.certify_source(CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "ctx-archive-v1",
                    digest,
                    ScannedSourceCounts {
                        complete_records: descriptor.member.records,
                        retained_records: descriptor.member.records,
                        indexed_documents: descriptor.member.records,
                        certified_bytes: payload.bytes,
                        ..ScannedSourceCounts::default()
                    },
                )?)?;
            }
        }
        let state = GenerationStateEnvelope::new(
            COVERAGE_FORMAT,
            serde_json::to_vec(&Coverage {
                collection: collection.into(),
                sequence: target,
            })?,
        )?;
        // Activation publishes only this captured prefix. A later withdrawal
        // advances unsafe_sequence under authority, so every reader rejects
        // this generation until coverage includes that withdrawal. No writer
        // can race another activation/rebuild through the projection lock.
        writer.commit_with_generation_state(|_| true, |_| true, |_| Ok(state), |_| Ok(()))?;
        facts.activated = true;
        facts.processed_operations = target.checked_sub(status.searchable_sequence);
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        tx.execute(
            "DELETE FROM pending WHERE collection=?1 AND sequence<=?2",
            params![collection, target],
        )?;
        tx.commit()?;
        let status = self.status_locked(&connection, collection)?;
        facts.coverage_lag = status
            .stored_sequence
            .checked_sub(status.searchable_sequence);
        facts.reads_available = Some(status.reads_available);
        Ok(status)
    }

    /// Rebuild only derived Core state. Accepted payloads, byte locators,
    /// receipts, access policy and tombstones remain in the SQLite authority.
    pub fn rebuild_collection(&self, collection: &str) -> Result<()> {
        collection_id(collection)?;
        let _writer = self.projection.lock().map_err(|_| Error::Unavailable)?;
        let connection = self.lock()?;
        self.status_numbers(&connection, collection)?;
        drop(connection);
        let path = self.collection_root(collection).join("index");
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        let connection = self.lock()?;
        connection.execute("INSERT OR IGNORE INTO pending SELECT collection,sequence FROM operations WHERE terminal='accepted' AND collection=?1",[collection])?;
        Ok(())
    }

    pub fn status(&self, token: &str, collection: &str) -> Result<CollectionStatus> {
        let connection = self.lock()?;
        let principal = authorize_any(
            &connection,
            token,
            collection,
            &[Access::Read, Access::Publish, Access::Manage],
        )?;
        let mut status = self.status_locked(&connection, collection)?;
        status.principal = principal;
        Ok(status)
    }

    pub(crate) fn safe_index(
        &self,
        connection: &Connection,
        collection: &str,
    ) -> Result<VerifiedIndex> {
        let (stored, unsafe_sequence) = self.status_numbers(connection, collection)?;
        let index = VerifiedIndex::open_pinned(self.collection_root(collection).join("index"))?;
        let covered = coverage(&index, collection)?;
        if covered < unsafe_sequence || covered > stored {
            return Err(Error::Unavailable);
        }
        Ok(index)
    }

    pub(crate) fn read_gate(&self, connection: &Connection, collection: &str) -> Result<()> {
        let status = self.status_locked(connection, collection)?;
        if status.reads_available {
            Ok(())
        } else {
            Err(Error::Unavailable)
        }
    }

    fn status_numbers(&self, connection: &Connection, collection: &str) -> Result<(u64, u64)> {
        use rusqlite::OptionalExtension;
        connection
            .query_row(
                "SELECT sequence,unsafe_sequence FROM collections WHERE id=?1",
                [collection],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)
    }

    pub(crate) fn status_locked(
        &self,
        connection: &Connection,
        collection: &str,
    ) -> Result<CollectionStatus> {
        let (stored, unsafe_sequence) = self.status_numbers(connection, collection)?;
        let root = self.collection_root(collection).join("index");
        let (sequence, generation) =
            if root.exists() && VerifiedIndex::active_generation_id(&root)?.is_some() {
                let index = VerifiedIndex::open_pinned(&root)?;
                (
                    coverage(&index, collection)?,
                    Some(index.generation_id().to_owned()),
                )
            } else {
                (0, None)
            };
        Ok(CollectionStatus {
            principal: String::new(),
            collection: collection.into(),
            stored_sequence: stored,
            searchable_sequence: sequence,
            generation,
            reads_available: sequence >= unsafe_sequence
                && sequence <= stored
                && (sequence > 0 || stored == 0),
            off_host_checkpoint: None,
        })
    }

    fn pending_collections(&self, after: &str) -> Result<Vec<String>> {
        let connection = self.lock()?;
        let mut statement =
            connection.prepare("SELECT DISTINCT collection FROM pending WHERE collection>?1 ORDER BY collection LIMIT 64")?;
        let values = statement
            .query_map([after], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(values)
    }

    pub(crate) fn index_sweep(&self, after: &mut String) -> Result<()> {
        let mut failed = false;
        let collections = self.pending_collections(after)?;
        // The existing single worker keeps a volatile keyset cursor. Persistent
        // failures in the first page cannot become a 64-collection lifetime cap.
        if collections.len() == 64 {
            after.clone_from(collections.last().ok_or(Error::Unavailable)?);
        } else {
            after.clear();
        }
        for collection in collections {
            // One corrupt collection must not starve other accepted work.
            failed |= self.index_pending(&collection, 16).is_err();
        }
        if failed {
            Err(Error::Unavailable)
        } else {
            Ok(())
        }
    }
}

fn revision_work(
    connection: &Connection,
    collection: &str,
    after: u64,
    target: u64,
) -> Result<Vec<RevisionWork>> {
    let mut changes: BTreeMap<String, (BTreeSet<String>, Option<String>)> = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT receipt FROM operations WHERE terminal='accepted' AND collection=?1 AND sequence>?2 AND sequence<=?3 ORDER BY sequence",
    )?;
    let mut rows = statement.query(params![collection, after, target])?;
    let mut expected = after + 1;
    while let Some(row) = rows.next()? {
        let receipt: Receipt = serde_json::from_str(&row.get::<_, String>(0)?)?;
        if receipt.collection != collection || receipt.sequence != expected {
            return Err(Error::Unavailable);
        }
        expected += 1;
        let (revisions, live) = changes.entry(receipt.operation.publication).or_default();
        if let Some(previous) = receipt.operation.expected_revision {
            revisions.insert(previous);
        }
        *live = match receipt.kind.as_str() {
            "publish" => {
                revisions.insert(receipt.operation.revision.clone());
                Some(receipt.operation.revision)
            }
            "withdraw" | "remove" => None,
            _ => return Err(Error::Unavailable),
        };
    }
    if expected != target + 1 {
        return Err(Error::Unavailable);
    }
    let mut work = Vec::new();
    for (publication, (revisions, live)) in changes {
        for revision in revisions {
            let (source, payload, descriptor): (String, String, String) = connection.query_row(
                "SELECT source,payload,descriptor FROM revisions WHERE collection=?1 AND publication=?2 AND revision=?3",
                params![collection, publication, revision],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            work.push(RevisionWork {
                source: serde_json::from_str(&source)?,
                payload: serde_json::from_str(&payload)?,
                descriptor: serde_json::from_str(&descriptor)?,
                live: live.as_ref() == Some(&revision),
            });
        }
    }
    Ok(work)
}

fn coverage(index: &VerifiedIndex, collection: &str) -> Result<u64> {
    let state = index
        .manifest()
        .generation_state()
        .ok_or(Error::Unavailable)?;
    if state.format() != COVERAGE_FORMAT {
        return Err(Error::Unavailable);
    }
    let coverage: Coverage = serde_json::from_slice(state.canonical_bytes())?;
    if coverage.collection != collection {
        return Err(Error::Unavailable);
    }
    Ok(coverage.sequence)
}

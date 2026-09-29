use crate::{
    admission::{check_complete, PreparedUpload},
    auth::authorize,
    catalog,
    operations::{fingerprint, record_receipt, retry},
    types::{identifier, now},
    *,
};
use ctx_history_archive::{ArchiveIdentity, ImportBinding, SessionMember};
use ctx_history_core::CoreRecord;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::atomic::Ordering};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub operation: Operation,
    pub identity: ArchiveIdentity,
    pub member: SessionMember,
    pub upload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WithdrawRequest {
    pub operation: Operation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Descriptor {
    pub binding: ImportBinding,
    pub member: SessionMember,
    pub publisher: String,
}

impl HistoryServer {
    pub fn publish(
        &self,
        token: &str,
        collection: &str,
        request: PublishRequest,
    ) -> Result<Receipt> {
        validate_operation(&request.operation)?;
        request.identity.validate().map_err(archive_error)?;
        request.member.validate().map_err(archive_error)?;
        if request.operation.revision != request.member.sha256 {
            return Err(Error::Invalid("revision must be the member SHA-256"));
        }
        let connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        let fingerprint = publish_fingerprint(
            &principal,
            &request.operation,
            &request.identity,
            &request.member,
        )?;
        if let Some(receipt) = retry(
            &connection,
            collection,
            &principal,
            &request.operation,
            &fingerprint,
            "publish",
        )? {
            return Ok(receipt);
        }
        let identity = serde_json::to_string(&(&request.identity, &request.member.path))?;
        check_owner(
            &connection,
            collection,
            &principal,
            &request.operation,
            Some(&identity),
        )?;
        let spec = UploadSpec {
            sha256: request.member.sha256.clone(),
            bytes: request.member.bytes,
        };
        let root = self.collection_root(collection);
        let prepared = PreparedUpload::pin(
            &root,
            &connection,
            collection,
            &principal,
            &request.upload,
            &spec,
        )?;
        drop(connection);
        let binding = binding(
            collection,
            &principal,
            &request.operation,
            &request.identity,
        )?;
        let source =
            ctx_history_archive::mapped_source(&binding, &request.member).map_err(archive_error)?;
        let descriptor = Descriptor {
            binding,
            member: request.member.clone(),
            publisher: principal.clone(),
        };
        prepared.validate(self, &root, &spec, &descriptor)?;
        let descriptor_json = serde_json::to_string(&descriptor)?;
        let payload_json = serde_json::to_string(&spec)?;
        let source_json = serde_json::to_string(&source)?;
        let mut connection = self.lock()?;
        // The validating request admitted no authority. Revoke, expiry, a
        // competing successor, or an identical accepted retry may have won.
        authorize(&connection, token, collection, Access::Publish)?;
        if let Some(receipt) = retry(
            &connection,
            collection,
            &principal,
            &request.operation,
            &fingerprint,
            "publish",
        )? {
            return Ok(receipt);
        }
        check_owner(
            &connection,
            collection,
            &principal,
            &request.operation,
            Some(&identity),
        )?;
        check_complete(&connection, collection, &principal, &request.upload, &spec)?;
        let prior: Option<String> = connection.query_row(
            "SELECT descriptor FROM revisions WHERE collection=?1 AND publication=?2 AND revision=?3",
            params![collection,request.operation.publication,request.operation.revision],
            |r| r.get(0),
        ).optional()?;
        if prior
            .as_ref()
            .is_some_and(|saved| saved != &descriptor_json)
        {
            return Err(Error::Conflict);
        }
        connection.execute(
            "ATTACH DATABASE ?1 AS validated",
            [prepared
                .references()
                .to_str()
                .ok_or(Error::Invalid("validation path must be UTF-8"))?],
        )?;
        let retained = root.join("payloads").join(&spec.sha256);
        let mut promoted = false;
        let result: Result<Receipt> = (|| {
            let tx = connection.transaction()?;
            // A policy advance rejects older writer consent even when the
            // credential stays active. Unlike content acceptance, it must
            // survive recovery from an earlier checkpoint.
            let policy_advanced: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM publications WHERE collection=?1 AND publication=?2 AND policy<?3)",
                params![collection, request.operation.publication, request.operation.policy_revision],
                |r| r.get(0),
            )?;
            let sequence = next_sequence(
                &tx,
                collection,
                request.operation.expected_revision.is_some(),
            )?;
            tx.execute("INSERT INTO publications(collection,publication,owner,epoch,policy,revision,identity,sequence) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(collection,publication) DO UPDATE SET policy=excluded.policy,revision=excluded.revision,sequence=excluded.sequence",
                params![collection,request.operation.publication,principal,request.operation.writer_epoch,request.operation.policy_revision,request.operation.revision,identity,sequence])?;
            if prior.is_none() {
                tx.execute(
                    "INSERT INTO revisions VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        collection,
                        request.operation.publication,
                        request.operation.revision,
                        source_json,
                        payload_json,
                        descriptor_json
                    ],
                )?;
                // Copy only verified body-free metadata. No transcript decoding,
                // hashing or per-record mapping occurs under authority.
                tx.execute("INSERT INTO event_refs SELECT ?1,?2,?3,event,session,sequence,offset,bytes,digest FROM validated.event_refs",
                    params![collection,request.operation.publication,request.operation.revision])?;
            }
            promoted = prepared.promote(&retained)?;
            fs::File::open(&retained)?.sync_all()?;
            catalog::sync_directory(&root.join("payloads"))?;
            check_complete(&tx, collection, &principal, &request.upload, &spec)?;
            let receipt = Receipt {
                collection: collection.into(),
                publisher: principal.clone(),
                operation: request.operation,
                sequence,
                kind: "publish".into(),
                payload: Some(spec),
                accepted_at: now()?,
            };
            record_receipt(&tx, &receipt, &fingerprint)?;
            tx.execute("DELETE FROM uploads WHERE id=?1", [&request.upload])?;
            catalog::audit(&tx, "publish", Some(&principal), Some(collection))?;
            // Check expiry after metadata insertion/fsync and all catalog
            // writes. Grants/predecessors cannot change under this lock.
            authorize(&tx, token, collection, Access::Publish)?;
            if policy_advanced {
                self.commit_authority(tx)?;
            } else {
                // Immutable revisions remain readable by exact citation until
                // withdrawal. Losing later content/receipts is the declared
                // checkpoint data-loss window, not restored access authority.
                tx.commit()?;
            }
            Ok(receipt)
        })();
        if result.is_err() && promoted {
            // A floor write can fail after SQLite committed. Never unlink a
            // payload referenced by that commit (or another publication).
            let referenced = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM revisions WHERE collection=?1 AND payload=?2)",
                params![collection, payload_json],
                |r| r.get::<_, bool>(0),
            );
            if matches!(referenced, Ok(false)) {
                let _ = fs::remove_file(&retained);
                let _ = catalog::sync_directory(&root.join("payloads"));
            }
        }
        if connection
            .execute_batch("DETACH DATABASE validated")
            .is_err()
        {
            self.authority_unavailable.store(true, Ordering::Release);
            return Err(Error::Unavailable);
        }
        let receipt = result?;
        // Acceptance is already durable. Scratch cleanup cannot turn success
        // into a false failure or invalidate a committed immutable payload.
        let _ = fs::remove_file(
            self.collection_root(collection)
                .join("staging")
                .join(&request.upload),
        );
        Ok(receipt)
    }

    pub fn withdraw(
        &self,
        token: &str,
        collection: &str,
        request: WithdrawRequest,
    ) -> Result<Receipt> {
        validate_operation(&request.operation)?;
        let mut connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        let fingerprint = fingerprint(&("withdraw", &principal, &request.operation))?;
        if let Some(receipt) = retry(
            &connection,
            collection,
            &principal,
            &request.operation,
            &fingerprint,
            "withdraw",
        )? {
            return Ok(receipt);
        }
        if request.operation.expected_revision.is_none() {
            return Err(Error::Conflict);
        }
        check_owner(
            &connection,
            collection,
            &principal,
            &request.operation,
            None,
        )?;
        let tx = connection.transaction()?;
        authorize(&tx, token, collection, Access::Publish)?;
        let sequence = next_sequence(&tx, collection, true)?;
        tx.execute("UPDATE publications SET withdrawn=1,revision=?1,policy=?2,sequence=?5 WHERE collection=?3 AND publication=?4",
            params![request.operation.revision,request.operation.policy_revision,collection,request.operation.publication,sequence])?;
        let receipt = Receipt {
            collection: collection.into(),
            publisher: principal.clone(),
            operation: request.operation,
            sequence,
            kind: "withdraw".into(),
            payload: None,
            accepted_at: now()?,
        };
        record_receipt(&tx, &receipt, &fingerprint)?;
        catalog::audit(&tx, "withdraw", Some(&principal), Some(collection))?;
        // Authority stayed locked since the predecessor check; recheck expiry
        // at acceptance, as on the publish path.
        authorize(&tx, token, collection, Access::Publish)?;
        self.commit_authority(tx)?;
        Ok(receipt)
    }

    /// Operator removal uses the same durable tombstone and search gate even
    /// when the departed publisher's credential is no longer active.
    pub fn remove_publication(
        &self,
        token: &str,
        collection: &str,
        request: WithdrawRequest,
    ) -> Result<Receipt> {
        validate_operation(&request.operation)?;
        let mut connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Manage)?;
        let fingerprint = fingerprint(&("remove", &principal, &request.operation))?;
        if let Some(receipt) = retry(
            &connection,
            collection,
            &principal,
            &request.operation,
            &fingerprint,
            "remove",
        )? {
            return Ok(receipt);
        }
        let current: (String,u64,bool,u64) = connection.query_row("SELECT revision,epoch,withdrawn,sequence FROM publications WHERE collection=?1 AND publication=?2",
            params![collection,request.operation.publication],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or(Error::NotFound)?;
        if request.operation.expected_revision.as_ref() != Some(&current.0)
            || request.operation.writer_epoch != current.1
            || current.2
            || request.operation.expected_sequence != Some(current.3)
        {
            return Err(Error::Conflict);
        }
        let tx = connection.transaction()?;
        let sequence = next_sequence(&tx, collection, true)?;
        tx.execute("UPDATE publications SET withdrawn=1,revision=?1,sequence=?4 WHERE collection=?2 AND publication=?3", params![request.operation.revision,collection,request.operation.publication,sequence])?;
        let receipt = Receipt {
            collection: collection.into(),
            publisher: principal.clone(),
            operation: request.operation,
            sequence,
            kind: "remove".into(),
            payload: None,
            accepted_at: now()?,
        };
        record_receipt(&tx, &receipt, &fingerprint)?;
        catalog::audit(&tx, "remove", Some(&principal), Some(collection))?;
        authorize(&tx, token, collection, Access::Manage)?;
        self.commit_authority(tx)?;
        Ok(receipt)
    }
}

pub(crate) fn validate_operation(op: &Operation) -> Result<()> {
    if op.expected_revision.is_some() != op.expected_sequence.is_some() {
        return Err(Error::Conflict);
    }
    if op
        .expected_sequence
        .is_some_and(|sequence| sequence == 0 || sequence > i64::MAX as u64)
    {
        return Err(Error::Invalid("expected publication sequence"));
    }
    for value in [&op.idempotency_key, &op.publication, &op.revision] {
        identifier(value)?;
    }
    if let Some(prior) = &op.expected_revision {
        identifier(prior)?;
        if prior == &op.revision {
            return Err(Error::Conflict);
        }
    }
    if op.writer_epoch == 0
        || op.policy_revision == 0
        || op.writer_epoch > i64::MAX as u64
        || op.policy_revision > i64::MAX as u64
    {
        return Err(Error::Invalid("writer/policy revision"));
    }
    Ok(())
}

fn check_owner(
    connection: &Connection,
    collection: &str,
    principal: &str,
    op: &Operation,
    identity: Option<&str>,
) -> Result<()> {
    let owner: Option<(String,u64,u64,String,String,bool,u64)> = connection.query_row(
        "SELECT owner,epoch,policy,revision,identity,withdrawn,sequence FROM publications WHERE collection=?1 AND publication=?2",
        params![collection,op.publication],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    match owner {
        Some((owner, epoch, policy, revision, bound, withdrawn, sequence)) => {
            if owner != principal
                || epoch != op.writer_epoch
                || policy > op.policy_revision
                || withdrawn
                || op.expected_revision.as_ref() != Some(&revision)
                || op.expected_sequence != Some(sequence)
                || identity.is_some_and(|i| i != bound)
            {
                return Err(Error::Conflict);
            }
        }
        None => {
            if op.expected_revision.is_some()
                || op.expected_sequence.is_some()
                || op.writer_epoch != 1
                || identity.is_none()
            {
                return Err(Error::Conflict);
            }
            let claimed: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM publications WHERE collection=?1 AND identity=?2)",
                params![collection, identity],
                |r| r.get(0),
            )?;
            if claimed {
                return Err(Error::Conflict);
            }
        }
    }
    Ok(())
}

fn next_sequence(connection: &Connection, collection: &str, unsafe_read: bool) -> Result<u64> {
    connection.execute("UPDATE collections SET sequence=sequence+1,unsafe_sequence=CASE WHEN ?1 THEN sequence+1 ELSE unsafe_sequence END WHERE id=?2",params![unsafe_read,collection])?;
    Ok(connection.query_row(
        "SELECT sequence FROM collections WHERE id=?1",
        [collection],
        |r| r.get(0),
    )?)
}

fn binding(
    collection: &str,
    principal: &str,
    op: &Operation,
    identity: &ArchiveIdentity,
) -> Result<ImportBinding> {
    // The length-framed JSON tuple avoids ambiguous namespace concatenation.
    Ok(ImportBinding {
        namespace: format!(
            "hosted-v1:{}",
            hex::encode(Sha256::digest(serde_json::to_vec(&(
                collection,
                principal,
                &op.publication,
                &op.revision
            ))?))
        ),
        identity: identity.clone(),
    })
}

pub(crate) fn visit_payload(
    path: &Path,
    member: &SessionMember,
    visit: impl FnMut(CoreRecord) -> ctx_history_archive::Result<()>,
) -> Result<()> {
    ctx_history_archive::visit_member_records(path, member, visit).map_err(archive_error)
}

pub(crate) fn archive_error(error: ctx_history_archive::ArchiveError) -> Error {
    Error::Archive(error.to_string())
}

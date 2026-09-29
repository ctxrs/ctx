//! Terminal operation outcomes and explicit publish settlement.
use crate::{
    auth::{authorize, publication_state_locked},
    publication::validate_operation,
    *,
};
use ctx_history_archive::{ArchiveIdentity, SessionMember};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelPublishRequest {
    pub publisher: String,
    pub operation: Operation,
    /// Lowercase hexadecimal SHA-256 from publish_fingerprint.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CancelPublishOutcome {
    Accepted {
        receipt: Receipt,
    },
    Cancelled {
        publisher: String,
        operation: Operation,
        fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelPublishResponse {
    pub outcome: CancelPublishOutcome,
    pub publication: Option<PublicationState>,
}

/// Exact publish identity, shared with clients settling saved pending work.
/// Upload handles are transient and are intentionally absent from this digest.
pub fn publish_fingerprint(
    publisher: &str,
    operation: &Operation,
    identity: &ArchiveIdentity,
    member: &SessionMember,
) -> Result<String> {
    fingerprint(&("publish", publisher, operation, identity, member))
}

pub(crate) fn fingerprint(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

impl HistoryServer {
    /// Settle an uncertain publish without resending its payload. Cancellation
    /// preserves any accepted content; withdrawal is a separate operation.
    /// First settlement fences either outcome against stale checkpoint restore.
    pub fn cancel_publish(
        &self,
        token: &str,
        collection: &str,
        request: CancelPublishRequest,
    ) -> Result<CancelPublishResponse> {
        validate_operation(&request.operation)?;
        if request.fingerprint.len() != 64
            || !request
                .fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("publish fingerprint"));
        }
        let mut connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        if principal != request.publisher {
            return Err(Error::Forbidden);
        }
        let tx = connection.transaction()?;
        let stored = lookup(
            &tx,
            collection,
            &principal,
            &request.operation.idempotency_key,
        )?;
        if let Some(stored) = &stored {
            stored.matches(&request.operation, &request.fingerprint, "publish")?;
        }
        let publication =
            publication_state_locked(&tx, collection, &request.operation.publication)?;
        let changed = stored.as_ref().is_none_or(|stored| !stored.cancel_fenced);
        if changed
            && publication
                .as_ref()
                .is_some_and(|state| state.owner != principal)
        {
            return Err(Error::Forbidden);
        }
        let outcome = if let Some(stored) = stored {
            if changed {
                tx.execute("UPDATE operations SET cancel_fenced=1 WHERE collection=?1 AND principal=?2 AND key=?3",
                    params![collection, principal, request.operation.idempotency_key])?;
            }
            stored.outcome
        } else {
            tx.execute("INSERT INTO operations(collection,key,principal,fingerprint,terminal,operation,cancel_fenced) VALUES (?1,?2,?3,?4,'cancelled',?5,1)",
                params![collection, request.operation.idempotency_key, principal, request.fingerprint, serde_json::to_string(&request.operation)?])?;
            CancelPublishOutcome::Cancelled {
                publisher: principal.clone(),
                operation: request.operation,
                fingerprint: request.fingerprint,
            }
        };
        if changed {
            catalog::audit(&tx, "settle_publish", Some(&principal), Some(collection))?;
        }
        authorize(&tx, token, collection, Access::Publish)?;
        if changed {
            self.commit_authority(tx)?;
        } else {
            tx.commit()?;
        }
        Ok(CancelPublishResponse {
            outcome,
            publication,
        })
    }

    pub fn receipt(&self, token: &str, collection: &str, key: &str) -> Result<Receipt> {
        let connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        match lookup(&connection, collection, &principal, key)?.map(|stored| stored.outcome) {
            Some(CancelPublishOutcome::Accepted { receipt }) => Ok(receipt),
            Some(CancelPublishOutcome::Cancelled { .. }) => Err(Error::OperationCancelled),
            None => Err(Error::NotFound),
        }
    }
}

struct StoredOperation {
    fingerprint: String,
    outcome: CancelPublishOutcome,
    cancel_fenced: bool,
}

impl StoredOperation {
    fn matches(&self, operation: &Operation, fingerprint: &str, kind: &str) -> Result<()> {
        let (saved, saved_kind) = match &self.outcome {
            CancelPublishOutcome::Accepted { receipt } => {
                (&receipt.operation, receipt.kind.as_str())
            }
            CancelPublishOutcome::Cancelled { operation, .. } => (operation, "publish"),
        };
        if self.fingerprint != fingerprint || saved != operation || saved_kind != kind {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}

type OperationRow = (String, String, Option<String>, Option<String>, bool);

fn lookup(
    connection: &Connection,
    collection: &str,
    principal: &str,
    key: &str,
) -> Result<Option<StoredOperation>> {
    let row: Option<OperationRow> = connection.query_row(
        "SELECT fingerprint,terminal,receipt,operation,cancel_fenced FROM operations WHERE collection=?1 AND principal=?2 AND key=?3",
        params![collection,principal,key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    row.map(
        |(fingerprint, terminal, receipt, operation, cancel_fenced)| {
            let outcome = match (terminal.as_str(), receipt, operation) {
                ("accepted", Some(receipt), None) => CancelPublishOutcome::Accepted {
                    receipt: serde_json::from_str(&receipt)?,
                },
                ("cancelled", None, Some(operation)) => CancelPublishOutcome::Cancelled {
                    publisher: principal.into(),
                    operation: serde_json::from_str(&operation)?,
                    fingerprint: fingerprint.clone(),
                },
                _ => return Err(Error::Unavailable),
            };
            Ok(StoredOperation {
                fingerprint,
                outcome,
                cancel_fenced,
            })
        },
    )
    .transpose()
}

pub(crate) fn retry(
    connection: &Connection,
    collection: &str,
    principal: &str,
    operation: &Operation,
    fingerprint: &str,
    kind: &str,
) -> Result<Option<Receipt>> {
    let Some(stored) = lookup(
        connection,
        collection,
        principal,
        &operation.idempotency_key,
    )?
    else {
        return Ok(None);
    };
    stored.matches(operation, fingerprint, kind)?;
    match stored.outcome {
        CancelPublishOutcome::Accepted { receipt } => Ok(Some(receipt)),
        CancelPublishOutcome::Cancelled { .. } => Err(Error::OperationCancelled),
    }
}

pub(crate) fn record_receipt(
    connection: &Connection,
    receipt: &Receipt,
    fingerprint: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO operations(collection,key,principal,fingerprint,terminal,receipt,sequence) VALUES (?1,?2,?3,?4,'accepted',?5,?6)",
        params![receipt.collection,receipt.operation.idempotency_key,receipt.publisher,fingerprint,serde_json::to_string(receipt)?,receipt.sequence],
    )?;
    connection.execute(
        "INSERT INTO pending VALUES (?1,?2)",
        params![receipt.collection, receipt.sequence],
    )?;
    Ok(())
}

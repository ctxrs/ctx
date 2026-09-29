use crate::{
    catalog,
    types::{collection_id, identifier, now},
    *,
};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapInfo {
    pub principal: String,
    pub collection: String,
}

/// Store locally with owner-only permissions. Never put this value in logs or
/// command arguments. Each device can carry a read/publish/manage subset in one credential.
#[derive(Serialize, Deserialize)]
pub struct TokenFile {
    pub principal: String,
    pub collection: String,
    pub credential: IssuedSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteRequest {
    pub name: String,
    pub grants: Grants,
    pub enrollment_ttl_seconds: u64,
    /// Zero (the default) means the device credential is valid until revoked.
    /// Positive values select an explicit lifetime of at most 90 days.
    #[serde(default)]
    pub credential_ttl_seconds: u64,
}

/// Protected invitation descriptor. Clients can discover its collection and
/// offered scope before redeeming the single-use secret; redemption returns
/// the server-authoritative collection and issued credential.
#[derive(Serialize, Deserialize)]
pub struct EnrollmentFile {
    pub principal: String,
    pub collection: String,
    pub enrollment: IssuedSecret,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollRequest {
    pub enrollment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    pub principal: String,
    pub grants: Grants,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationState {
    pub publication: String,
    pub owner: String,
    pub revision: String,
    /// Sequence of this publication's latest accepted operation.
    pub sequence: u64,
    pub writer_epoch: u64,
    pub policy_revision: u64,
    pub withdrawn: bool,
}

impl HistoryServer {
    pub fn bootstrap(&self, name: &str, token_file: &Path) -> Result<BootstrapInfo> {
        identifier(name)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let count: u64 = tx.query_row("SELECT count(*) FROM principals", [], |r| r.get(0))?;
        if count != 0 {
            return Err(Error::Conflict);
        }
        let principal = uuid::Uuid::new_v4().to_string();
        let collection = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO principals(id,name) VALUES (?1,?2)",
            params![principal, name],
        )?;
        tx.execute(
            "INSERT INTO collections(id,name) VALUES (?1,?2)",
            params![collection, name],
        )?;
        tx.execute(
            "INSERT INTO grants VALUES (?1,?2,1,1,1)",
            params![principal, collection],
        )?;
        let tokens = TokenFile {
            principal: principal.clone(),
            collection: collection.clone(),
            credential: insert_credential(
                &tx,
                &principal,
                Grants {
                    read: true,
                    publish: true,
                    manage: true,
                },
                0,
            )?,
        };
        write_token_file(token_file, &tokens)?;
        catalog::audit(&tx, "bootstrap", Some(&principal), Some(&collection))?;
        tx.commit()?;
        Ok(BootstrapInfo {
            principal,
            collection,
        })
    }

    /// Local operator facade. Requires exclusive access to this server root;
    /// remote administration uses the authenticated methods below.
    pub fn create_principal(&self, name: &str) -> Result<String> {
        identifier(name)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO principals(id,name) VALUES (?1,?2)",
            params![id, name],
        )?;
        catalog::audit(&tx, "create_principal", Some(&id), None)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn create_collection(&self, name: &str) -> Result<String> {
        identifier(name)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO collections(id,name) VALUES (?1,?2)",
            params![id, name],
        )?;
        catalog::audit(&tx, "create_collection", None, Some(&id))?;
        tx.commit()?;
        Ok(id)
    }

    pub fn set_grants(&self, principal: &str, collection: &str, grants: Grants) -> Result<()> {
        collection_id(collection)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        set_grants(&tx, principal, collection, grants)?;
        tx.commit()?;
        Ok(())
    }

    pub fn manage_grants(
        &self,
        token: &str,
        principal: &str,
        collection: &str,
        grants: Grants,
    ) -> Result<()> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        authorize(&tx, token, collection, Access::Manage)?;
        set_grants(&tx, principal, collection, grants)?;
        tx.commit()?;
        Ok(())
    }

    /// A zero TTL issues a credential valid until revoked; positive TTLs expire.
    pub fn issue_credential(
        &self,
        principal: &str,
        scope: Grants,
        ttl_seconds: u64,
    ) -> Result<IssuedSecret> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let secret = insert_credential(&tx, principal, scope, ttl_seconds)?;
        catalog::audit(&tx, "issue_credential", Some(principal), None)?;
        tx.commit()?;
        Ok(secret)
    }

    pub fn issue_enrollment(
        &self,
        principal: &str,
        collection: &str,
        scope: Grants,
        ttl_seconds: u64,
        credential_ttl_seconds: u64,
    ) -> Result<IssuedSecret> {
        collection_id(collection)?;
        if ttl_seconds == 0 || ttl_seconds > 3600 || credential_ttl_seconds > 90 * 24 * 3600 {
            return Err(Error::Invalid("invalid enrollment lifetime"));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        active_principal(&tx, principal)?;
        let secret = new_secret(ttl_seconds, scope)?;
        tx.execute(
            "INSERT INTO enrollments VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                secret.id,
                hash_secret(&secret.secret),
                principal,
                scope.bits(),
                secret.expires_at,
                credential_ttl_seconds,
                collection
            ],
        )?;
        catalog::audit(&tx, "issue_enrollment", Some(principal), None)?;
        tx.commit()?;
        Ok(secret)
    }

    pub fn redeem(&self, enrollment: &str) -> Result<TokenFile> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let (principal,scope,ttl,collection): (String,u8,u64,String) = tx.query_row(
            "SELECT principal,scope,credential_ttl,collection FROM enrollments WHERE digest=?1 AND expires>?2",
            params![hash_secret(enrollment),now()?], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
            .optional()?.ok_or(Error::Unauthorized)?;
        let scope = Grants::from_bits(scope);
        let secret = insert_credential(&tx, &principal, scope, ttl)?;
        tx.execute(
            "DELETE FROM enrollments WHERE digest=?1",
            [hash_secret(enrollment)],
        )?;
        catalog::audit(&tx, "redeem_enrollment", Some(&principal), None)?;
        tx.commit()?;
        Ok(TokenFile {
            principal,
            collection,
            credential: secret,
        })
    }

    pub fn invite(
        &self,
        token: &str,
        collection: &str,
        request: InviteRequest,
    ) -> Result<EnrollmentFile> {
        identifier(&request.name)?;
        if request.grants.bits() == 0
            || request.enrollment_ttl_seconds == 0
            || request.enrollment_ttl_seconds > 3600
            || request.credential_ttl_seconds > 90 * 24 * 3600
        {
            return Err(Error::Invalid("invite scope or lifetime"));
        }
        let mut connection = self.lock()?;
        let manager = authorize(&connection, token, collection, Access::Manage)?;
        let tx = connection.transaction()?;
        let principal = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO principals(id,name) VALUES (?1,?2)",
            params![principal, request.name],
        )?;
        set_grants(&tx, &principal, collection, request.grants)?;
        let secret = new_secret(request.enrollment_ttl_seconds, request.grants)?;
        tx.execute(
            "INSERT INTO enrollments VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                secret.id,
                hash_secret(&secret.secret),
                principal,
                request.grants.bits(),
                secret.expires_at,
                request.credential_ttl_seconds,
                collection
            ],
        )?;
        catalog::audit(&tx, "invite", Some(&manager), Some(collection))?;
        tx.commit()?;
        Ok(EnrollmentFile {
            principal,
            collection: collection.into(),
            enrollment: secret,
        })
    }

    pub fn revoke_member(&self, token: &str, collection: &str, principal: &str) -> Result<()> {
        self.manage_grants(token, principal, collection, Grants::default())
    }

    pub fn publication_state(
        &self,
        token: &str,
        collection: &str,
        publication: &str,
    ) -> Result<PublicationState> {
        let connection = self.lock()?;
        authorize_any(
            &connection,
            token,
            collection,
            &[Access::Read, Access::Publish, Access::Manage],
        )?;
        publication_state_locked(&connection, collection, publication)?.ok_or(Error::NotFound)
    }

    pub fn revoke_credential(&self, credential_id: &str) -> Result<()> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        tx.execute(
            "UPDATE credentials SET revoked=1 WHERE id=?1",
            [credential_id],
        )?;
        catalog::audit(&tx, "revoke_credential", None, None)?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_principal(&self, principal: &str) -> Result<()> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        tx.execute("UPDATE principals SET revoked=1 WHERE id=?1", [principal])?;
        tx.execute("DELETE FROM enrollments WHERE principal=?1", [principal])?;
        catalog::audit(&tx, "revoke_principal", Some(principal), None)?;
        tx.commit()?;
        Ok(())
    }

    pub fn local_health(&self) -> Result<LocalHealth> {
        let connection = self.lock()?;
        Ok(LocalHealth {
            collections: connection
                .query_row("SELECT count(*) FROM collections", [], |r| r.get(0))?,
            pending_operations: connection
                .query_row("SELECT count(*) FROM pending", [], |r| r.get(0))?,
            staged_uploads: connection
                .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))?,
        })
    }

    pub fn audit(&self, after: u64, limit: usize) -> Result<Vec<AuditEntry>> {
        if limit == 0 || limit > 1000 {
            return Err(Error::Invalid("audit page limit"));
        }
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT sequence,at,action,principal,collection FROM audit WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let entries = statement
            .query_map(params![after, limit as u64], |r| {
                Ok(AuditEntry {
                    sequence: r.get(0)?,
                    at: r.get(1)?,
                    action: r.get(2)?,
                    principal: r.get(3)?,
                    collection: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }
}

pub(crate) fn publication_state_locked(
    connection: &Connection,
    collection: &str,
    publication: &str,
) -> Result<Option<PublicationState>> {
    Ok(connection.query_row("SELECT owner,revision,epoch,policy,withdrawn,sequence FROM publications WHERE collection=?1 AND publication=?2",params![collection,publication],|r|Ok(PublicationState {
        publication:publication.into(),owner:r.get(0)?,revision:r.get(1)?,writer_epoch:r.get(2)?,policy_revision:r.get(3)?,withdrawn:r.get(4)?,sequence:r.get(5)?
    })).optional()?)
}

fn set_grants(
    connection: &Connection,
    principal: &str,
    collection: &str,
    grants: Grants,
) -> Result<()> {
    active_principal(connection, principal)?;
    connection.execute("INSERT INTO grants VALUES (?1,?2,?3,?4,?5) ON CONFLICT(principal,collection) DO UPDATE SET read=excluded.read,publish=excluded.publish,manage=excluded.manage",
        params![principal,collection,grants.read,grants.publish,grants.manage])?;
    catalog::audit(connection, "set_grants", Some(principal), Some(collection))
}

fn active_principal(connection: &Connection, principal: &str) -> Result<()> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM principals WHERE id=?1 AND revoked=0)",
        [principal],
        |r| r.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(Error::Unauthorized)
    }
}

pub(crate) fn authorize(
    connection: &Connection,
    token: &str,
    collection: &str,
    required: Access,
) -> Result<String> {
    authorize_at(connection, token, collection, required, now()?)
}

pub(crate) fn authorize_at(
    connection: &Connection,
    token: &str,
    collection: &str,
    required: Access,
    at: u64,
) -> Result<String> {
    collection_id(collection)?;
    let principal: Option<String> = connection.query_row(
        "SELECT c.principal FROM credentials c JOIN principals p ON p.id=c.principal JOIN grants g ON g.principal=p.id WHERE c.digest=?1 AND c.revoked=0 AND (c.expires=0 OR c.expires>?2) AND p.revoked=0 AND (c.scope & ?3) != 0 AND g.collection=?4 AND CASE ?3 WHEN 1 THEN g.read WHEN 2 THEN g.publish WHEN 4 THEN g.manage ELSE 0 END=1",
        params![hash_secret(token),at,required.bit(),collection], |r| r.get(0)).optional()?;
    principal.ok_or(Error::Forbidden)
}

pub(crate) fn authorize_any(
    connection: &Connection,
    token: &str,
    collection: &str,
    rights: &[Access],
) -> Result<String> {
    for right in rights {
        match authorize(connection, token, collection, *right) {
            Ok(principal) => return Ok(principal),
            Err(Error::Forbidden) => (),
            Err(error) => return Err(error),
        }
    }
    Err(Error::Forbidden)
}

fn new_secret(ttl: u64, grants: Grants) -> Result<IssuedSecret> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    Ok(IssuedSecret {
        grants,
        id: uuid::Uuid::new_v4().to_string(),
        secret: hex::encode(bytes),
        expires_at: if ttl == 0 {
            0
        } else {
            now()?
                .checked_add(ttl)
                .ok_or(Error::Invalid("credential lifetime"))?
        },
    })
}

pub(crate) fn insert_credential(
    connection: &Connection,
    principal: &str,
    scope: Grants,
    ttl: u64,
) -> Result<IssuedSecret> {
    if scope.bits() == 0 || ttl > 90 * 24 * 3600 {
        return Err(Error::Invalid(
            "credential needs a scope and a lifetime of zero or at most 90 days",
        ));
    }
    active_principal(connection, principal)?;
    let secret = new_secret(ttl, scope)?;
    connection.execute(
        "INSERT INTO credentials(id,digest,principal,scope,expires) VALUES (?1,?2,?3,?4,?5)",
        params![
            secret.id,
            hash_secret(&secret.secret),
            principal,
            scope.bits(),
            secret.expires_at
        ],
    )?;
    Ok(secret)
}

fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

pub fn write_token_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = ctx_history_platform::platform_security::create_private_file_new(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    catalog::sync_directory(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
}

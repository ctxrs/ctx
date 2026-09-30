#[cfg(test)]
pub(crate) use crate::identity::authorize_at;
pub(crate) use crate::identity::{authorize, authorize_any};
use crate::identity::{identity_at, principal_grants, subset};
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
    #[serde(default)]
    pub enrollment_id: Option<String>,
    pub principal: String,
    pub collection: String,
    pub credential: IssuedSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub principal: Option<String>,
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
            "INSERT INTO principals(id,name,server_owner) VALUES (?1,?2,1)",
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
            enrollment_id: None,
            principal: principal.clone(),
            collection: collection.clone(),
            credential: insert_credential(
                &tx,
                &principal,
                None,
                None,
                Grants {
                    read: true,
                    publish: true,
                    manage: true,
                },
                0,
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
        let actor = identity_at(&tx, token, collection, now()?)?;
        if !actor.server_owner && (!actor.grants.manage || !subset(grants, actor.grants)) {
            return Err(Error::Forbidden);
        }
        set_grants(&tx, principal, collection, grants)?;
        tx.commit()?;
        Ok(())
    }

    /// Issue a local operator-selected collection credential capped by current grants.
    /// A zero TTL is valid until revoked; positive TTLs expire.
    pub fn issue_credential(
        &self,
        principal: &str,
        collection: &str,
        scope: Grants,
        ttl_seconds: u64,
    ) -> Result<IssuedSecret> {
        collection_id(collection)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let secret = insert_credential(
            &tx,
            principal,
            Some(collection),
            None,
            scope,
            ttl_seconds,
            0,
        )?;
        catalog::audit(&tx, "issue_credential", Some(principal), None)?;
        tx.commit()?;
        Ok(secret)
    }

    /// Local root operator may issue another accountwide credential only for
    /// an explicitly recorded owner. Collection manage grants are insufficient.
    pub fn issue_owner_credential(
        &self,
        principal: &str,
        ttl_seconds: u64,
    ) -> Result<IssuedSecret> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let secret = insert_credential(
            &tx,
            principal,
            None,
            None,
            Grants::from_bits(7),
            ttl_seconds,
            0,
        )?;
        catalog::audit(&tx, "issue_owner_credential", Some(principal), None)?;
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
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let secret = insert_enrollment(
            &tx,
            principal,
            collection,
            scope,
            ttl_seconds,
            credential_ttl_seconds,
            0,
        )?;
        catalog::audit(&tx, "issue_enrollment", Some(principal), Some(collection))?;
        tx.commit()?;
        Ok(secret)
    }

    pub fn redeem(&self, enrollment: &str) -> Result<TokenFile> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let (principal,scope,ttl,collection,enrollment_id,expires_ceiling): (String,u8,u64,String,String,u64) = tx.query_row(
            "SELECT principal,scope,credential_ttl,collection,id,credential_expires_ceiling FROM enrollments WHERE digest=?1 AND expires>?2",
            params![hash_secret(enrollment),now()?], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))
            .optional()?.ok_or(Error::Unauthorized)?;
        // Grant removal after invitation also narrows the newly issued token.
        let scope =
            Grants::from_bits(scope & principal_grants(&tx, &principal, &collection)?.bits());
        if scope.bits() == 0 {
            return Err(Error::Forbidden);
        }
        let secret = insert_credential(
            &tx,
            &principal,
            Some(&collection),
            Some(&enrollment_id),
            scope,
            ttl,
            expires_ceiling,
        )?;
        tx.execute(
            "DELETE FROM enrollments WHERE digest=?1",
            [hash_secret(enrollment)],
        )?;
        catalog::audit(&tx, "redeem_enrollment", Some(&principal), None)?;
        tx.commit()?;
        Ok(TokenFile {
            enrollment_id: Some(enrollment_id),
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
        if let Some(name) = &request.name {
            identifier(name)?;
        }
        if request.principal.is_some() && request.name.is_some() {
            return Err(Error::Invalid(
                "existing principal cannot be renamed by enrollment",
            ));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let actor = identity_at(&tx, token, collection, now()?)?;
        if !actor.server_owner && !subset(request.grants, actor.grants) {
            return Err(Error::Forbidden);
        }
        let principal = if let Some(principal) = request.principal {
            if !actor.server_owner && actor.principal != principal {
                return Err(Error::Forbidden);
            }
            // Existing identity receives a device, never a grant mutation.
            principal
        } else {
            if !actor.server_owner && !actor.grants.manage {
                return Err(Error::Forbidden);
            }
            let principal = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO principals(id,name) VALUES (?1,?2)",
                params![principal, request.name.unwrap_or_default()],
            )?;
            set_grants(&tx, &principal, collection, request.grants)?;
            principal
        };
        let expires_ceiling = if actor.server_owner {
            0
        } else {
            tx.query_row(
                "SELECT expires FROM credentials WHERE id=?1",
                [&actor.credential_id],
                |r| r.get(0),
            )?
        };
        let secret = insert_enrollment(
            &tx,
            &principal,
            collection,
            request.grants,
            request.enrollment_ttl_seconds,
            request.credential_ttl_seconds,
            expires_ceiling,
        )?;
        catalog::audit(&tx, "invite", Some(&actor.principal), Some(collection))?;
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
        crate::access::revoke_credential(&tx, credential_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_principal(&self, principal: &str) -> Result<()> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        crate::access::revoke_principal(&tx, principal)?;
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

pub(crate) fn active_principal(connection: &Connection, principal: &str) -> Result<()> {
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

fn insert_enrollment(
    connection: &Connection,
    principal: &str,
    collection: &str,
    scope: Grants,
    ttl: u64,
    credential_ttl: u64,
    expires_ceiling: u64,
) -> Result<IssuedSecret> {
    if scope.bits() == 0 || ttl == 0 || ttl > 3600 || credential_ttl > 90 * 24 * 3600 {
        return Err(Error::Invalid("invite scope or lifetime"));
    }
    active_principal(connection, principal)?;
    if !subset(scope, principal_grants(connection, principal, collection)?) {
        return Err(Error::Forbidden);
    }
    let mut secret = new_secret(ttl, scope)?;
    cap_expiry(&mut secret, expires_ceiling)?;
    connection.execute(
        "INSERT INTO enrollments VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            secret.id,
            hash_secret(&secret.secret),
            principal,
            scope.bits(),
            secret.expires_at,
            credential_ttl,
            collection,
            expires_ceiling
        ],
    )?;
    Ok(secret)
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
    collection: Option<&str>,
    enrollment_id: Option<&str>,
    scope: Grants,
    ttl: u64,
    expires_ceiling: u64,
) -> Result<IssuedSecret> {
    if scope.bits() == 0 || ttl > 90 * 24 * 3600 {
        return Err(Error::Invalid(
            "credential needs a scope and a lifetime of zero or at most 90 days",
        ));
    }
    active_principal(connection, principal)?;
    if let Some(collection) = collection {
        collection_id(collection)?;
        if !subset(scope, principal_grants(connection, principal, collection)?) {
            return Err(Error::Forbidden);
        }
    } else {
        let owner: bool = connection.query_row(
            "SELECT server_owner FROM principals WHERE id=?1",
            [principal],
            |r| r.get(0),
        )?;
        if !owner || scope.bits() != 7 {
            return Err(Error::Forbidden);
        }
    }
    let mut secret = new_secret(ttl, scope)?;
    cap_expiry(&mut secret, expires_ceiling)?;
    connection.execute(
        "INSERT INTO credentials(id,digest,principal,scope,expires,collection,server_owner,enrollment_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            secret.id,
            hash_secret(&secret.secret),
            principal,
            scope.bits(),
            secret.expires_at,
            collection,
            collection.is_none(),
            enrollment_id
        ],
    )?;
    Ok(secret)
}

// Delegation cannot outlive a finite issuer, including after a delayed exchange.
fn cap_expiry(secret: &mut IssuedSecret, ceiling: u64) -> Result<()> {
    if ceiling != 0 {
        if ceiling <= now()? {
            return Err(Error::Forbidden);
        }
        if secret.expires_at == 0 || secret.expires_at > ceiling {
            secret.expires_at = ceiling;
        }
    }
    Ok(())
}

pub(crate) fn hash_secret(secret: &str) -> Vec<u8> {
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

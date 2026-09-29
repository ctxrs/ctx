//! One live authorization path for both collection work and root administration.
use crate::{
    auth::hash_secret,
    types::{collection_id, now},
    *,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionIdentity {
    pub principal: String,
    pub credential_id: String,
    pub collection: String,
    pub grants: Grants,
    /// Authority of this credential, not just ownership of its principal.
    pub server_owner: bool,
    pub enrollment_id: Option<String>,
}

struct Credential {
    principal: String,
    id: String,
    collection: Option<String>,
    scope: Grants,
    server_owner: bool,
    enrollment_id: Option<String>,
}

fn authenticate(connection: &Connection, token: &str, at: u64) -> Result<Credential> {
    connection.query_row(
        "SELECT c.principal,c.id,c.collection,c.scope,c.server_owner AND p.server_owner,c.enrollment_id
         FROM credentials c JOIN principals p ON p.id=c.principal
         WHERE c.digest=?1 AND c.revoked=0 AND p.revoked=0 AND (c.expires=0 OR c.expires>?2)
         AND ((c.server_owner=1 AND p.server_owner=1 AND c.collection IS NULL AND c.scope=7)
              OR (c.server_owner=0 AND c.collection IS NOT NULL))",
        params![hash_secret(token), at], |r| Ok(Credential {
            principal:r.get(0)?, id:r.get(1)?, collection:r.get(2)?,
            scope:Grants::from_bits(r.get(3)?), server_owner:r.get(4)?, enrollment_id:r.get(5)?,
        })).optional()?.ok_or(Error::Forbidden)
}

pub(crate) fn principal_grants(
    connection: &Connection,
    principal: &str,
    collection: &str,
) -> Result<Grants> {
    Ok(connection
        .query_row(
            "SELECT read,publish,manage FROM grants WHERE principal=?1 AND collection=?2",
            params![principal, collection],
            |r| {
                Ok(Grants {
                    read: r.get(0)?,
                    publish: r.get(1)?,
                    manage: r.get(2)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub(crate) fn subset(requested: Grants, allowed: Grants) -> bool {
    requested.bits() & !allowed.bits() == 0
}

pub(crate) fn identity_at(
    connection: &Connection,
    token: &str,
    collection: &str,
    at: u64,
) -> Result<ConnectionIdentity> {
    collection_id(collection)?;
    let credential = authenticate(connection, token, at)?;
    if !credential.server_owner && credential.collection.as_deref() != Some(collection) {
        return Err(Error::Forbidden);
    }
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM collections WHERE id=?1)",
        [collection],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(Error::NotFound);
    }
    let grants = if credential.server_owner {
        credential.scope
    } else {
        Grants::from_bits(
            credential.scope.bits()
                & principal_grants(connection, &credential.principal, collection)?.bits(),
        )
    };
    Ok(ConnectionIdentity {
        principal: credential.principal,
        credential_id: credential.id,
        collection: collection.into(),
        grants,
        server_owner: credential.server_owner,
        enrollment_id: credential.enrollment_id,
    })
}

pub(crate) fn authorize_owner(connection: &Connection, token: &str) -> Result<String> {
    let credential = authenticate(connection, token, now()?)?;
    if !credential.server_owner {
        return Err(Error::Forbidden);
    }
    Ok(credential.principal)
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
    let identity = identity_at(connection, token, collection, at)?;
    if identity.grants.bits() & required.bit() == 0 {
        return Err(Error::Forbidden);
    }
    Ok(identity.principal)
}

pub(crate) fn authorize_any(
    connection: &Connection,
    token: &str,
    collection: &str,
    rights: &[Access],
) -> Result<String> {
    let identity = identity_at(connection, token, collection, now()?)?;
    if !rights
        .iter()
        .any(|right| identity.grants.bits() & right.bit() != 0)
    {
        return Err(Error::Forbidden);
    }
    Ok(identity.principal)
}

impl HistoryServer {
    /// Authenticates the presented device even when all its grants were removed.
    /// Clients must inspect `grants` before claiming usable collection access.
    pub fn whoami(&self, token: &str, collection: &str) -> Result<ConnectionIdentity> {
        let connection = self.lock()?;
        identity_at(&connection, token, collection, now()?)
    }
}

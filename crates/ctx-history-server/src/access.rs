//! Owner-only credential inventory. A credential ID is also its device identity.
use crate::{catalog, identity::authorize_owner, types::identifier, *};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessListRequest {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default = "page_limit")]
    pub limit: usize,
}

fn page_limit() -> usize {
    50
}
impl Default for AccessListRequest {
    fn default() -> Self {
        Self {
            after: None,
            limit: page_limit(),
        }
    }
}
impl AccessListRequest {
    fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > 100 {
            return Err(Error::Invalid("access page limit must be 1..100"));
        }
        if let Some(after) = &self.after {
            identifier(after)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalEntry {
    pub principal: String,
    pub name: Option<String>,
    pub revoked: bool,
    pub server_owner: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalPage {
    pub principals: Vec<PrincipalEntry>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub credential_id: String,
    pub principal: String,
    pub collection: Option<String>,
    /// Issuance ceiling. whoami returns the current effective rights.
    pub grants: Grants,
    pub expires_at: u64,
    pub revoked: bool,
    pub server_owner: bool,
    pub enrollment_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialPage {
    pub credentials: Vec<CredentialEntry>,
    pub next_cursor: Option<String>,
}

impl HistoryServer {
    pub fn list_principals(
        &self,
        token: &str,
        request: AccessListRequest,
    ) -> Result<PrincipalPage> {
        request.validate()?;
        let connection = self.lock()?;
        authorize_owner(&connection, token)?;
        let mut statement = connection.prepare("SELECT id,NULLIF(name,''),revoked,server_owner FROM principals WHERE (?1 IS NULL OR id>?1) ORDER BY id LIMIT ?2")?;
        let mut principals = statement
            .query_map(params![request.after, request.limit as u64 + 1], |r| {
                Ok(PrincipalEntry {
                    principal: r.get(0)?,
                    name: r.get(1)?,
                    revoked: r.get(2)?,
                    server_owner: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next_cursor = if principals.len() > request.limit {
            principals.truncate(request.limit);
            principals.last().map(|p| p.principal.clone())
        } else {
            None
        };
        Ok(PrincipalPage {
            principals,
            next_cursor,
        })
    }

    pub fn list_credentials(
        &self,
        token: &str,
        principal: &str,
        request: AccessListRequest,
    ) -> Result<CredentialPage> {
        identifier(principal)?;
        request.validate()?;
        let connection = self.lock()?;
        authorize_owner(&connection, token)?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM principals WHERE id=?1)",
            [principal],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::NotFound);
        }
        let mut statement = connection.prepare(
            "SELECT c.id,c.principal,c.collection,c.scope,c.expires,c.revoked OR p.revoked,c.server_owner AND p.server_owner,c.enrollment_id
             FROM credentials c JOIN principals p ON p.id=c.principal
             WHERE c.principal=?1 AND (?2 IS NULL OR c.id>?2) ORDER BY c.id LIMIT ?3")?;
        let mut credentials = statement
            .query_map(
                params![principal, request.after, request.limit as u64 + 1],
                |r| {
                    Ok(CredentialEntry {
                        credential_id: r.get(0)?,
                        principal: r.get(1)?,
                        collection: r.get(2)?,
                        grants: Grants::from_bits(r.get(3)?),
                        expires_at: r.get(4)?,
                        revoked: r.get(5)?,
                        server_owner: r.get(6)?,
                        enrollment_id: r.get(7)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next_cursor = if credentials.len() > request.limit {
            credentials.truncate(request.limit);
            credentials.last().map(|c| c.credential_id.clone())
        } else {
            None
        };
        Ok(CredentialPage {
            credentials,
            next_cursor,
        })
    }

    pub fn admin_revoke_credential(&self, token: &str, credential_id: &str) -> Result<()> {
        identifier(credential_id)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        authorize_owner(&tx, token)?;
        revoke_credential(&tx, credential_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn admin_revoke_principal(&self, token: &str, principal: &str) -> Result<()> {
        identifier(principal)?;
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        authorize_owner(&tx, token)?;
        revoke_principal(&tx, principal)?;
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn revoke_credential(connection: &Connection, credential_id: &str) -> Result<()> {
    if connection.execute(
        "UPDATE credentials SET revoked=1 WHERE id=?1",
        [credential_id],
    )? == 0
    {
        return Err(Error::NotFound);
    }
    catalog::audit(connection, "revoke_credential", None, None)
}

pub(crate) fn revoke_principal(connection: &Connection, principal: &str) -> Result<()> {
    if connection.execute("UPDATE principals SET revoked=1 WHERE id=?1", [principal])? == 0 {
        return Err(Error::NotFound);
    }
    connection.execute(
        "UPDATE credentials SET revoked=1 WHERE principal=?1",
        [principal],
    )?;
    connection.execute("DELETE FROM enrollments WHERE principal=?1", [principal])?;
    connection.execute("DELETE FROM grants WHERE principal=?1", [principal])?;
    catalog::audit(connection, "revoke_principal", Some(principal), None)
}

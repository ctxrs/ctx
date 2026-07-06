#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDeviceIdentity {
    pub id: Uuid,
    pub stable_device_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceIdentity {
    pub id: Uuid,
    pub device_id: Uuid,
    pub vcs_workspace_id: Option<Uuid>,
    pub repo_fingerprint: String,
    pub root_path_hash: String,
    pub display_root: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) fn local_device_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<LocalDeviceIdentity> {
    Ok(LocalDeviceIdentity {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        stable_device_id: row.get(1)?,
        created_at: time_ms(row.get(2)?),
        updated_at: time_ms(row.get(3)?),
    })
}

pub(crate) fn local_workspace_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<LocalWorkspaceIdentity> {
    let vcs_workspace_id: Option<String> = row.get(2)?;
    Ok(LocalWorkspaceIdentity {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        device_id: parse_uuid(row.get::<_, String>(1)?)?,
        vcs_workspace_id: vcs_workspace_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        repo_fingerprint: row.get(3)?,
        root_path_hash: row.get(4)?,
        display_root: row.get(5)?,
        created_at: time_ms(row.get(6)?),
        updated_at: time_ms(row.get(7)?),
    })
}

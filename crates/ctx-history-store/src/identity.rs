use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{new_id, utc_now};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::connection::{optional_uuid_string, parse_uuid, time_ms, timestamp_ms};
use crate::object_store::sha256_hex;
use crate::{Result, Store, StoreError};

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

impl Store {
    pub fn get_or_create_local_device(&self) -> Result<LocalDeviceIdentity> {
        if let Some(device) = self.local_device()? {
            return Ok(device);
        }
        let now = utc_now();
        let device = LocalDeviceIdentity {
            id: new_id(),
            stable_device_id: format!("ctx-device-{}", new_id().simple()),
            created_at: now,
            updated_at: now,
        };
        self.conn.execute(
            r#"
                INSERT INTO local_devices
                (id, stable_device_id, created_at_ms, updated_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?3, '{}')
                "#,
            params![
                device.id.to_string(),
                device.stable_device_id.as_str(),
                timestamp_ms(now),
            ],
        )?;
        Ok(device)
    }

    pub fn register_local_workspace(
        &self,
        root_path: impl AsRef<Path>,
        repo_fingerprint: &str,
        vcs_workspace_id: Option<Uuid>,
    ) -> Result<LocalWorkspaceIdentity> {
        let device = self.get_or_create_local_device()?;
        let root = root_path.as_ref();
        let root_path_hash = sha256_hex(root.display().to_string().as_bytes());
        let display_root = root.display().to_string();
        let now = utc_now();
        let id = new_id();
        self.conn.execute(
                r#"
                INSERT INTO local_workspaces
                (
                    id, device_id, vcs_workspace_id, repo_fingerprint, root_path_hash,
                    display_root, created_at_ms, updated_at_ms, metadata_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, '{}')
                ON CONFLICT(device_id, repo_fingerprint, root_path_hash) DO UPDATE SET
                    vcs_workspace_id = COALESCE(excluded.vcs_workspace_id, local_workspaces.vcs_workspace_id),
                    display_root = excluded.display_root,
                    updated_at_ms = excluded.updated_at_ms
                "#,
                params![
                    id.to_string(),
                    device.id.to_string(),
                    optional_uuid_string(vcs_workspace_id),
                    repo_fingerprint,
                    root_path_hash,
                    display_root,
                    timestamp_ms(now),
                ],
            )?;
        self.conn
            .query_row(
                r#"
                    SELECT id, device_id, vcs_workspace_id, repo_fingerprint, root_path_hash,
                           display_root, created_at_ms, updated_at_ms
                    FROM local_workspaces
                    WHERE device_id = ?1 AND repo_fingerprint = ?2 AND root_path_hash = ?3
                    "#,
                params![device.id.to_string(), repo_fingerprint, root_path_hash],
                local_workspace_from_row,
            )
            .map_err(StoreError::from)
    }

    pub fn local_device(&self) -> Result<Option<LocalDeviceIdentity>> {
        self.conn
                .query_row(
                    "SELECT id, stable_device_id, created_at_ms, updated_at_ms FROM local_devices ORDER BY created_at_ms, id LIMIT 1",
                    [],
                    local_device_from_row,
                )
                .optional()
                .map_err(StoreError::from)
    }
}

fn local_device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalDeviceIdentity> {
    Ok(LocalDeviceIdentity {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        stable_device_id: row.get(1)?,
        created_at: time_ms(row.get(2)?),
        updated_at: time_ms(row.get(3)?),
    })
}

fn local_workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalWorkspaceIdentity> {
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

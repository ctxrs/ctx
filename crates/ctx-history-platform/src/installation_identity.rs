//! The opaque installation identity shared by root resolution and CLI creation.
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::Path,
};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationIdentityRecord {
    pub schema_version: u16,
    pub install_id: String,
    pub created_at: String,
}

pub fn validate_installation_record(record: InstallationIdentityRecord) -> io::Result<String> {
    let parsed = Uuid::parse_str(&record.install_id).map_err(invalid)?;
    if record.schema_version != 1
        || parsed.is_nil()
        || parsed.hyphenated().to_string() != record.install_id
        || record.created_at.is_empty()
        || record.created_at.len() > 128
    {
        return Err(invalid("installation identity record is invalid"));
    }
    Ok(record.install_id)
}

/// Read without creating, repairing permissions, or rewriting the identity.
pub fn read_installation_id(root: &Path) -> io::Result<String> {
    let file = crate::managed_root::open_private_file(&root.join("install.json"))?;
    let mut body = Vec::new();
    file.take(1025).read_to_end(&mut body)?;
    if body.len() > 1024 {
        return Err(invalid("installation identity exceeds its size bound"));
    }
    let record = serde_json::from_slice(&body).map_err(invalid)?;
    validate_installation_record(record)
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

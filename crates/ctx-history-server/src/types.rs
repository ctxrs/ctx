use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("publish operation was cancelled")]
    OperationCancelled,
    #[error("authentication required")]
    Unauthorized,
    #[error("collection access denied")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("conflicting operation or predecessor")]
    Conflict,
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("collection is unavailable until its safe search generation is ready")]
    Unavailable,
    #[error("restored authority is recovery-closed")]
    RecoveryClosed,
    #[error("resource capacity exhausted; retry later")]
    Capacity,
    #[error("upload expired; resubmit the complete revision")]
    Expired,
    #[error("storage error")]
    Io(#[from] std::io::Error),
    #[error("catalog error")]
    Sql(#[from] rusqlite::Error),
    #[error("invalid JSON")]
    Json(#[from] serde_json::Error),
    #[error("Core projection failed")]
    Index(#[from] ctx_history_index::IndexError),
    #[error("invalid Core identity")]
    Identity(#[from] ctx_history_core::ProjectionContractError),
    #[error("invalid Core record")]
    Core(#[from] ctx_history_core::CoreRecordError),
    #[error("portable archive is invalid")]
    Archive(String),
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub root: PathBuf,
    /// Independently retained security floor. Configure outside the data root;
    /// it must advance before acknowledging access, withdrawal or writer-policy
    /// changes. It does not track ordinary content acceptance or backup coverage.
    pub authority_file: Option<PathBuf>,
    pub bind: SocketAddr,
    /// Non-loopback HTTP requires an explicitly trusted TLS reverse proxy.
    pub trusted_ingress: bool,
    pub max_in_flight: usize,
    pub max_chunk_bytes: usize,
    pub max_staged_uploads: usize,
    pub staging_ttl_seconds: u64,
    pub minimum_free_bytes: u64,
    pub index_memory_bytes: usize,
}

impl ServerConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            authority_file: None,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7332),
            trusted_ingress: false,
            max_in_flight: 8,
            max_chunk_bytes: 4 * 1024 * 1024,
            max_staged_uploads: 64,
            staging_ttl_seconds: 3600,
            minimum_free_bytes: 64 * 1024 * 1024,
            index_memory_bytes: 64 * 1024 * 1024,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if !self.bind.ip().is_loopback() && !self.trusted_ingress {
            return Err(Error::Invalid(
                "non-loopback bind requires trusted TLS ingress",
            ));
        }
        if self.root.as_os_str().is_empty()
            || self.max_in_flight == 0
            || self.max_chunk_bytes == 0
            || self.max_staged_uploads == 0
            || self.staging_ttl_seconds == 0
        {
            return Err(Error::Invalid("empty root or zero resource budget"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    pub read: bool,
    pub publish: bool,
    pub manage: bool,
}

impl Grants {
    pub(crate) fn bits(self) -> u8 {
        u8::from(self.read) | (u8::from(self.publish) << 1) | (u8::from(self.manage) << 2)
    }
    pub(crate) fn from_bits(value: u8) -> Self {
        Self {
            read: value & 1 != 0,
            publish: value & 2 != 0,
            manage: value & 4 != 0,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Access {
    Read,
    Publish,
    Manage,
}

impl Access {
    pub(crate) fn bit(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Publish => 2,
            Self::Manage => 4,
        }
    }
}

/// Deliberately lacks Debug: returned once; the catalog keeps only its digest.
#[derive(Serialize, Deserialize)]
pub struct IssuedSecret {
    pub grants: Grants,
    pub id: String,
    pub secret: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadSpec {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadStatus {
    pub id: String,
    pub publisher: String,
    pub received_bytes: u64,
    pub expected_bytes: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub idempotency_key: String,
    pub publication: String,
    pub writer_epoch: u64,
    pub policy_revision: u64,
    pub expected_revision: Option<String>,
    /// Last accepted sequence of this publication, not the collection frontier.
    /// Absent together with expected_revision only for the first publication.
    pub expected_sequence: Option<u64>,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub collection: String,
    pub publisher: String,
    pub operation: Operation,
    pub sequence: u64,
    pub kind: String,
    pub payload: Option<UploadSpec>,
    pub accepted_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionStatus {
    pub collection: String,
    pub stored_sequence: u64,
    pub searchable_sequence: u64,
    pub generation: Option<String>,
    pub reads_available: bool,
    pub off_host_checkpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub sequence: u64,
    pub at: u64,
    pub action: String,
    pub principal: Option<String>,
    pub collection: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalHealth {
    pub recovery_closed: bool,
    pub collections: u64,
    pub pending_operations: u64,
    pub staged_uploads: u64,
}

pub(crate) fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| Error::Unavailable)
}

pub(crate) fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(Error::Invalid("invalid identifier"));
    }
    Ok(())
}

pub(crate) fn collection_id(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        Ok(())
    } else {
        Err(Error::NotFound)
    }
}

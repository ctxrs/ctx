//! Portable retained history, independent of acquisition paths and JSONL-v2.
//!
//! Archive v1 stores the complete Core JSON v3/n1/p4/a1/r3 mapping, including
//! absent timestamps, policy dispositions, structured content and activity.
//! A future Core contract requires an explicit archive migration; changing Core
//! serialization alone must not silently change this mapping. Original IDs and
//! claims are preserved in members. They are evidence, never authorization.
//!
//! A manifest closes a normalized-only retained snapshot. Missing members do
//! not authorize destination deletion. Native provider files, credentials,
//! executable instructions and compressed containers are outside this format.

mod export;
mod io;
mod restore;
mod retained;

use std::{collections::BTreeSet, path::Path};

pub use ctx_history_core::{CoreRecord, SourceKey, StableEntityId};
pub use ctx_history_index::VerifiedIndex;
pub use export::{export, export_session, export_with_control};
pub use io::{verify, verify_member, visit_member_records, visit_members, visit_records};
pub use restore::{
    is_archive_root, map_record, mapped_event, mapped_session, mapped_source, prepare_restore_root,
    restore, ImportBinding, RestoreOptions, RestoreReceipt,
};
pub use retained::{export_data_root, ExportReceipt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ARCHIVE_VERSION: u32 = 1;
pub const CORE_MAPPING: &str = "core-json-v3-n1-p4-a1-r3";
pub type Result<T> = std::result::Result<T, ArchiveError>;

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Core(#[from] ctx_history_core::CoreRecordError),
    #[error(transparent)]
    Identity(#[from] ctx_history_core::ProjectionContractError),
    #[error(transparent)]
    Index(#[from] ctx_history_index::IndexError),
    #[error("Core index requires committed migration recovery: {0:?}")]
    MigrationRecovery(ctx_history_index::CommittedPredecessorMigrationRecovery),
    #[error(transparent)]
    Scratch(#[from] rusqlite::Error),
    #[error("invalid archive: {0}")]
    Invalid(String),
    #[error("archive revision conflict for {member}; current revision is {current}")]
    Conflict { member: String, current: String },
}

fn invalid(message: impl Into<String>) -> ArchiveError {
    ArchiveError::Invalid(message.into())
}

/// Durable caller-assigned identities. Neither is a file path or credential.
/// Hosted callers MUST bind these claims to server-owned publication authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveIdentity {
    pub origin: String,
    pub view: String,
}

impl ArchiveIdentity {
    pub fn validate(&self) -> Result<()> {
        for text in [&self.origin, &self.view] {
            if text.is_empty() || text.len() > 1024 || text.chars().any(char::is_control) {
                return Err(invalid(
                    "origin/view must contain 1..1024 non-control UTF-8 bytes",
                ));
            }
        }
        Ok(())
    }
}

/// Empty sets select all. Nonempty sets intersect. Values are full lowercase
/// SHA-256 identity digests, not provider-native IDs or pathname substrings.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub sources: BTreeSet<String>,
    pub sessions: BTreeSet<String>,
}

impl Selection {
    pub fn includes_source(&self, source: &SourceKey) -> bool {
        self.sources.is_empty() || self.sources.contains(&hex(&source.identity().digest()))
    }

    pub fn includes_session(&self, session: StableEntityId) -> bool {
        self.sessions.is_empty() || self.sessions.contains(&hex(&session.digest()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub identity: ArchiveIdentity,
    pub core_mapping: String,
    pub core_contract: String,
    pub generation: String,
    /// Always `retained_normalized`; no harness-resumption/native-body claim.
    pub fidelity: String,
    pub selected_subset: bool,
    pub inventory_sha256: String,
    pub members: u64,
    pub records: u64,
}

impl Manifest {
    /// Content address of this exact snapshot, independent of its location.
    pub fn snapshot_id(&self) -> Result<String> {
        Ok(hex(&Sha256::digest(serde_json::to_vec(self)?)))
    }

    /// Compare with authorization obtained outside the archive, before upload.
    pub fn require_identity(&self, authorized: &ArchiveIdentity) -> Result<()> {
        authorized.validate()?;
        if self.identity != *authorized {
            return Err(invalid(
                "archive origin/view differs from authorized publication",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        use ctx_history_core::*;
        self.identity.validate()?;
        if self.version != ARCHIVE_VERSION
            || self.core_mapping != CORE_MAPPING
            || self.core_contract != core_record_contract_fingerprint()
            || (
                CORE_RECORD_VERSION,
                CORE_NORMALIZATION_REVISION,
                CORE_CONTENT_POLICY_REVISION,
                CORE_ACTIVITY_REVISION,
                CORE_RELATIONSHIP_CONTRACT_REVISION,
            ) != (3, 1, 4, 1, 3)
            || self.fidelity != "retained_normalized"
            || !is_digest(&self.inventory_sha256)
            || self.generation.is_empty()
        {
            return Err(invalid(
                "unsupported archive/Core mapping or malformed manifest",
            ));
        }
        Ok(())
    }
}

/// One complete session revision. `sha256` is the exact JSONL byte revision;
/// `path` depends only on original source/session identity, never the revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMember {
    pub source: SourceKey,
    pub session_id: StableEntityId,
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub records: u64,
}

impl SessionMember {
    pub fn validate(&self) -> Result<()> {
        self.source.validate_contract()?;
        self.session_id.validate_contract()?;
        if self.session_id.entity_kind() != ctx_history_core::StableEntityKind::Session
            || self.session_id.source_digest() != self.source.identity().digest()
            || self.session_id.source_descriptor_digest() != self.source.exact_descriptor_digest()
            || self.path != member_path(&self.source, self.session_id)
            || !is_digest(&self.sha256)
            || self.bytes == 0
            || self.records == 0
        {
            return Err(invalid(
                "invalid session member identity, path, digest or counts",
            ));
        }
        Ok(())
    }
}

pub(crate) fn member_path(source: &SourceKey, session: StableEntityId) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ctx-archive-session-v1\0");
    hash.update(source.identity().digest());
    hash.update(session.digest());
    format!("members/{}.jsonl", hex(&hash.finalize()))
}

pub(crate) fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn checked_add(value: &mut u64, amount: u64) -> Result<()> {
    *value = value
        .checked_add(amount)
        .ok_or_else(|| invalid("archive count overflow"))?;
    Ok(())
}

pub(crate) fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests;

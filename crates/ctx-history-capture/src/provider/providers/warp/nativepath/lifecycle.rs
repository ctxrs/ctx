use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use super::{publication::WarpNativeFrontier, query::WarpNativeEof};
use crate::{CaptureError, Result};

pub(in super::super) const WARP_NATIVE_STATE_SCHEMA_VERSION: u32 = 2;
pub(in super::super) const WARP_NATIVE_PARSER_REVISION: u32 = 1;
pub(in super::super) const WARP_NATIVE_POLICY_REVISION: u32 = 1;
pub(in super::super) const WARP_NATIVE_ROUTE_MAX_BYTES: usize = 4 * 1_024;
pub(in super::super) const WARP_NATIVE_PERSISTED_STATE_MAX_BYTES: usize = 16 * 1_024;

const WARP_FAILURE_DETAIL_MAX_CHARS: usize = 1_024;
const WARP_SNAPSHOT_REVISION_MAX_BYTES: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in super::super) enum WarpNativeSourceIdentity {
    Unix {
        device: u64,
        inode: u64,
    },
    Windows {
        volume_serial: u64,
        file_id: [u8; 16],
    },
    UnsupportedPlatform,
}

impl WarpNativeSourceIdentity {
    pub(super) fn supports_exact_replay(&self) -> bool {
        !matches!(self, Self::UnsupportedPlatform)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in super::super) enum WarpNativeSourceFailureKind {
    NotFound,
    Permission,
    Locked,
    Corrupt,
    SchemaIncompatible,
    InvalidSource,
    SourceChanged,
    SourceDatabase,
    Io,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct WarpNativeSourceFailure {
    pub(in super::super) kind: WarpNativeSourceFailureKind,
    pub(in super::super) canonical_route: PathBuf,
    pub(in super::super) detail: String,
}

impl WarpNativeSourceFailure {
    pub(super) fn from_capture(path: &Path, error: CaptureError, schema_stage: bool) -> Self {
        use rusqlite::ErrorCode;

        let kind = match &error {
            CaptureError::Io(error) => match error.kind() {
                std::io::ErrorKind::NotFound => WarpNativeSourceFailureKind::NotFound,
                std::io::ErrorKind::PermissionDenied => WarpNativeSourceFailureKind::Permission,
                std::io::ErrorKind::WouldBlock => WarpNativeSourceFailureKind::SourceChanged,
                _ => WarpNativeSourceFailureKind::Io,
            },
            CaptureError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => {
                match failure.code {
                    ErrorCode::DatabaseBusy
                    | ErrorCode::DatabaseLocked
                    | ErrorCode::FileLockingProtocolFailed => WarpNativeSourceFailureKind::Locked,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => {
                        WarpNativeSourceFailureKind::Corrupt
                    }
                    ErrorCode::PermissionDenied | ErrorCode::CannotOpen => {
                        WarpNativeSourceFailureKind::Permission
                    }
                    _ => WarpNativeSourceFailureKind::SourceDatabase,
                }
            }
            CaptureError::Sqlite(_) => WarpNativeSourceFailureKind::SourceDatabase,
            CaptureError::InvalidProviderTranscriptPath { .. } => {
                WarpNativeSourceFailureKind::InvalidSource
            }
            CaptureError::SourceChangedDuringCapture => WarpNativeSourceFailureKind::SourceChanged,
            CaptureError::InvalidPayload(_) if schema_stage => {
                WarpNativeSourceFailureKind::SchemaIncompatible
            }
            _ if schema_stage => WarpNativeSourceFailureKind::SchemaIncompatible,
            _ => WarpNativeSourceFailureKind::SourceDatabase,
        };
        Self {
            kind,
            canonical_route: path.to_path_buf(),
            detail: truncate_chars(&error.to_string(), WARP_FAILURE_DETAIL_MAX_CHARS),
        }
    }
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(maximum).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in super::super) enum WarpNativePreparationAction {
    ExactNoOp,
    ResumeExactSnapshot,
    AuthoritativeScan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct WarpNativePreparationInputs {
    pub(in super::super) canonical_route: PathBuf,
    pub(in super::super) source_identity: WarpNativeSourceIdentity,
    pub(in super::super) snapshot_revision: String,
    pub(in super::super) capability_digest: String,
    pub(in super::super) parser_revision: u32,
    pub(in super::super) policy_revision: u32,
    pub(in super::super) action: WarpNativePreparationAction,
    pub(in super::super) resume_frontier: Option<WarpNativeFrontier>,
}

impl WarpNativePreparationInputs {
    /// Persists a sink-acknowledged restart frontier.
    ///
    /// This operation is deliberately nonterminal. Exact EOF authority is
    /// owned by the immutable snapshot scanner and cannot be supplied by a
    /// caller alongside an arbitrary safe frontier.
    pub(in super::super) fn persisted_state_at(
        &self,
        frontier: WarpNativeFrontier,
    ) -> Result<WarpNativePersistedState> {
        self.persisted_state(frontier, false)
    }

    pub(super) fn persisted_state_at_eof(
        &self,
        eof: WarpNativeEof,
    ) -> Result<WarpNativePersistedState> {
        self.persisted_state(eof.into_frontier(), true)
    }

    fn persisted_state(
        &self,
        frontier: WarpNativeFrontier,
        terminal: bool,
    ) -> Result<WarpNativePersistedState> {
        let inventory = WarpNativeGenerationInventory::from_frontier(&frontier);
        let state = WarpNativePersistedState {
            schema_version: WARP_NATIVE_STATE_SCHEMA_VERSION,
            parser_revision: self.parser_revision,
            policy_revision: self.policy_revision,
            canonical_route: self.canonical_route.clone(),
            source_identity: self.source_identity.clone(),
            snapshot_revision: self.snapshot_revision.clone(),
            capability_digest: self.capability_digest.clone(),
            source_integrity_digest: hex_digest(frontier.source_digest),
            core_generation_digest: hex_digest(frontier.core_digest),
            checkpoint: WarpNativeCheckpointProof {
                terminal_authority: terminal,
                frontier,
                inventory,
            },
        };
        state.validate()?;
        Ok(state)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct WarpNativeGenerationInventory {
    pub(in super::super) conversation_rows: u64,
    pub(in super::super) hierarchy_edges: u64,
    pub(in super::super) task_rows: u64,
    pub(in super::super) retained_events: u64,
}

impl WarpNativeGenerationInventory {
    fn from_frontier(frontier: &WarpNativeFrontier) -> Self {
        Self {
            conversation_rows: frontier.completed_conversation_rows,
            hierarchy_edges: frontier.completed_hierarchy_edges,
            task_rows: frontier.completed_task_rows,
            retained_events: frontier.retained_events,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct WarpNativeCheckpointProof {
    /// Trusted only when minted from `WarpNativeEof` in this process.
    #[serde(rename = "terminal")]
    terminal_authority: bool,
    frontier: WarpNativeFrontier,
    inventory: WarpNativeGenerationInventory,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WarpNativeCheckpointProofWire {
    #[serde(rename = "terminal")]
    _terminal_observation: bool,
    frontier: WarpNativeFrontier,
    inventory: WarpNativeGenerationInventory,
}

impl<'de> Deserialize<'de> for WarpNativeCheckpointProof {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WarpNativeCheckpointProofWire::deserialize(deserializer)?;
        Ok(Self {
            // Persistence is an untrusted boundary. Reaching EOF on a newly
            // certified immutable snapshot is the only way to remint this bit.
            terminal_authority: false,
            frontier: wire.frontier,
            inventory: wire.inventory,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct WarpNativePersistedState {
    pub(in super::super) schema_version: u32,
    pub(in super::super) parser_revision: u32,
    pub(in super::super) policy_revision: u32,
    pub(in super::super) canonical_route: PathBuf,
    pub(in super::super) source_identity: WarpNativeSourceIdentity,
    pub(in super::super) snapshot_revision: String,
    pub(in super::super) capability_digest: String,
    pub(in super::super) source_integrity_digest: String,
    pub(in super::super) core_generation_digest: String,
    checkpoint: WarpNativeCheckpointProof,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WarpNativePersistedStateWire {
    schema_version: u32,
    parser_revision: u32,
    policy_revision: u32,
    canonical_route: PathBuf,
    source_identity: WarpNativeSourceIdentity,
    snapshot_revision: String,
    capability_digest: String,
    source_integrity_digest: String,
    core_generation_digest: String,
    checkpoint: WarpNativeCheckpointProof,
}

impl<'de> Deserialize<'de> for WarpNativePersistedState {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WarpNativePersistedStateWire::deserialize(deserializer)?;
        Ok(Self {
            schema_version: wire.schema_version,
            parser_revision: wire.parser_revision,
            policy_revision: wire.policy_revision,
            canonical_route: wire.canonical_route,
            source_identity: wire.source_identity,
            snapshot_revision: wire.snapshot_revision,
            capability_digest: wire.capability_digest,
            source_integrity_digest: wire.source_integrity_digest,
            core_generation_digest: wire.core_generation_digest,
            checkpoint: wire.checkpoint,
        })
    }
}

impl WarpNativePersistedState {
    /// True only for EOF authority minted in this process. Deserialization
    /// always clears the serialized terminal observation.
    pub(in super::super) fn checkpoint_is_terminal(&self) -> bool {
        self.checkpoint.terminal_authority
    }

    pub(in super::super) fn checkpoint_frontier(&self) -> &WarpNativeFrontier {
        &self.checkpoint.frontier
    }

    pub(in super::super) fn is_supported(&self) -> bool {
        self.validate().is_ok()
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != WARP_NATIVE_STATE_SCHEMA_VERSION
            || self.parser_revision != WARP_NATIVE_PARSER_REVISION
            || self.policy_revision != WARP_NATIVE_POLICY_REVISION
        {
            return Err(CaptureError::InvalidPayload(
                "Warp persisted lifecycle revision is unsupported".to_owned(),
            ));
        }
        let route = self.canonical_route.to_string_lossy();
        if !self.canonical_route.is_absolute() || route.len() > WARP_NATIVE_ROUTE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(format!(
                "Warp persisted route must be absolute and at most \
                 {WARP_NATIVE_ROUTE_MAX_BYTES} bytes"
            )));
        }
        if self.snapshot_revision.is_empty()
            || self.snapshot_revision.len() > WARP_SNAPSHOT_REVISION_MAX_BYTES
            || !is_sha256_hex(&self.capability_digest)
            || !is_sha256_hex(&self.source_integrity_digest)
            || !is_sha256_hex(&self.core_generation_digest)
            || !self.checkpoint.frontier.is_persistable()
            || self.checkpoint.inventory
                != WarpNativeGenerationInventory::from_frontier(&self.checkpoint.frontier)
            || self.source_integrity_digest != hex_digest(self.checkpoint.frontier.source_digest)
            || self.core_generation_digest != hex_digest(self.checkpoint.frontier.core_digest)
        {
            return Err(CaptureError::InvalidPayload(
                "Warp persisted lifecycle state is inconsistent or exceeds cursor bounds"
                    .to_owned(),
            ));
        }
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > WARP_NATIVE_PERSISTED_STATE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(format!(
                "Warp persisted lifecycle state exceeds \
                 {WARP_NATIVE_PERSISTED_STATE_MAX_BYTES} encoded bytes"
            )));
        }
        Ok(())
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

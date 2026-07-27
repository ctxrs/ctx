use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{CaptureError, Result};

use super::{
    native_path::{
        GooseNativeProFrontier, GooseNativeProfile, GooseNativeScanSummary,
        GooseNativeSourceAuthority,
    },
    position::{GooseNativeScanPhase, GooseNativeScanPosition},
    source::GooseNativePhysicalSourceIdentity,
};

const GOOSE_NATIVE_PERSISTED_STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GooseNativeInventorySummary {
    pub(super) native_session_rows: u64,
    pub(super) native_message_rows: u64,
    pub(super) session_identity_digest: String,
    pub(super) session_identity_samples: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GooseNativePersistedState {
    pub(super) version: u32,
    pub(super) selected_path: PathBuf,
    pub(super) raw_generation_digest: String,
    pub(super) capability_digest: String,
    pub(super) semantic_digest: String,
    pub(super) physical_source_identity: GooseNativePhysicalSourceIdentity,
    pub(super) completed_inventory_token: String,
    pub(super) profile: GooseNativeProfile,
    pub(super) core_frontier: GooseNativeScanPosition,
    pub(super) pro_frontier: GooseNativeProFrontier,
    pub(super) inventory: GooseNativeInventorySummary,
    pub(super) complete: bool,
}

impl GooseNativePersistedState {
    pub(super) fn from_summary(summary: &GooseNativeScanSummary) -> Result<Self> {
        let selected_path = match &summary.source_authority {
            GooseNativeSourceAuthority::ExactDispatchedDatabase { path, .. } => path.clone(),
        };
        let state = Self {
            version: GOOSE_NATIVE_PERSISTED_STATE_VERSION,
            selected_path,
            raw_generation_digest: summary.raw_generation_digest.clone(),
            capability_digest: summary.capability_digest.clone(),
            semantic_digest: summary.semantic_digest.clone(),
            physical_source_identity: summary.physical_source_identity.clone(),
            completed_inventory_token: summary
                .completed_inventory_token
                .clone()
                .unwrap_or_default(),
            profile: summary.profile,
            core_frontier: summary.position,
            pro_frontier: summary.pro_frontier,
            inventory: summary.inventory.clone(),
            complete: summary.complete,
        };
        if !state.is_supported() {
            return Err(CaptureError::InvalidPayload(
                "Goose current lifecycle state is incomplete or unsupported".to_owned(),
            ));
        }
        Ok(state)
    }

    pub(super) fn is_supported(&self) -> bool {
        self.version == GOOSE_NATIVE_PERSISTED_STATE_VERSION
            && self.selected_path.is_absolute()
            && self.complete
            && self.core_frontier.phase == GooseNativeScanPhase::Complete
            && !self.completed_inventory_token.trim().is_empty()
            && self.completed_inventory_token.len() <= 4 * 1024
            && [
                &self.raw_generation_digest,
                &self.capability_digest,
                &self.semantic_digest,
                &self.inventory.session_identity_digest,
            ]
            .into_iter()
            .all(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
    }
}

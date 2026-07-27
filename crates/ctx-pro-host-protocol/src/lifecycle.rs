use serde::{Deserialize, Serialize};

use crate::AuthorizationRequest;

/// Byte length of the unpredictable, process-local graph-key deletion challenge.
pub const GRAPH_KEY_DELETION_CHALLENGE_BYTES: usize = 32;
pub const GRAPH_KEY_DELETION_CHALLENGE_TTL_SECONDS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareGraphKeyDeletionRequest {
    pub installation_key_thumbprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphKeyDeletionPrepared {
    pub challenge_base64url: String,
    pub expires_at_unix: i64,
    pub key_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmGraphKeyDeletionRequest {
    pub authorization: AuthorizationRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphKeyDeleted {
    pub deleted: bool,
}

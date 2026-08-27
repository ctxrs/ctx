use std::fmt;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use crate::{history::HistoryTurn, FxProviderError, FxProviderResult};

pub const MAX_DURABLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DURABLE_BASE64_BYTES: usize = MAX_DURABLE_BYTES.div_ceil(3) * 4;

macro_rules! hex_bytes {
    ($name:ident, $size:expr, $message:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub [u8; $size]);

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let encoded = String::deserialize(deserializer)?;
                if encoded.len() != $size * 2
                    || !encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(D::Error::custom($message));
                }
                let mut decoded = [0_u8; $size];
                for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
                    let high = hex_nibble(pair[0]).ok_or_else(|| D::Error::custom($message))?;
                    let low = hex_nibble(pair[1]).ok_or_else(|| D::Error::custom($message))?;
                    decoded[index] = (high << 4) | low;
                }
                Ok(Self(decoded))
            }
        }
    };
}

hex_bytes!(FxId, 16, "expected 32 lowercase hexadecimal characters");
hex_bytes!(FxDigest, 32, "expected 64 lowercase hexadecimal characters");

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FxAuthoritySource {
    NativeCreate,
    LegacyMigration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxAuthority {
    pub schema_version: u64,
    pub session_id: String,
    pub authority_id: FxId,
    pub storage_format: String,
    pub source: FxAuthoritySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxWatermark {
    pub schema_version: u64,
    pub session_id: String,
    pub log_generation: FxId,
    pub through_seq: u64,
    pub through_event_id: FxId,
    pub through_event_log_bytes: u64,
}

/// Fx durable bytes are UTF-8 JSON strings, or canonical base64 wrappers only
/// when the decoded bytes are not UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableBytes {
    Utf8(String),
    NonUtf8Base64(String),
}

impl DurableBytes {
    pub fn searchable(&self) -> String {
        match self {
            Self::Utf8(text) => text.clone(),
            Self::NonUtf8Base64(encoded) => format!("base64:{encoded}"),
        }
    }

    pub fn encoded_base64(&self) -> Option<&str> {
        match self {
            Self::Utf8(_) => None,
            Self::NonUtf8Base64(encoded) => Some(encoded),
        }
    }

    pub(crate) fn accounted_bytes(&self) -> usize {
        match self {
            Self::Utf8(text) | Self::NonUtf8Base64(text) => text.len(),
        }
    }
}

impl Serialize for DurableBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Utf8(text) => serializer.serialize_str(text),
            Self::NonUtf8Base64(data) => DurableBytesWrapper {
                encoding: "base64",
                data,
            }
            .serialize(serializer),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DurableBytesWrapper<'a> {
    encoding: &'static str,
    data: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedDurableBytesWrapper {
    encoding: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DurableBytesWire {
    Utf8(String),
    Wrapped(OwnedDurableBytesWrapper),
}

impl<'de> Deserialize<'de> for DurableBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match DurableBytesWire::deserialize(deserializer)? {
            DurableBytesWire::Utf8(text) if text.len() <= MAX_DURABLE_BYTES => Ok(Self::Utf8(text)),
            DurableBytesWire::Utf8(_) => Err(D::Error::custom("durable UTF-8 field exceeds limit")),
            DurableBytesWire::Wrapped(wrapper) => {
                if wrapper.encoding != "base64" || wrapper.data.len() > MAX_DURABLE_BASE64_BYTES {
                    return Err(D::Error::custom("invalid durable base64 wrapper"));
                }
                let decoded = STANDARD
                    .decode(&wrapper.data)
                    .map_err(|_| D::Error::custom("invalid durable base64 data"))?;
                if decoded.len() > MAX_DURABLE_BYTES
                    || std::str::from_utf8(&decoded).is_ok()
                    || STANDARD.encode(&decoded) != wrapper.data
                {
                    return Err(D::Error::custom("non-canonical durable base64 wrapper"));
                }
                Ok(Self::NonUtf8Base64(wrapper.data))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderId {
    #[default]
    Gateway,
    Codex,
    Grok,
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Gateway => "gateway",
            Self::Codex => "codex",
            Self::Grok => "grok",
        })
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.eq_ignore_ascii_case("gateway") {
            Ok(Self::Gateway)
        } else if value.eq_ignore_ascii_case("codex") {
            Ok(Self::Codex)
        } else if value.eq_ignore_ascii_case("grok") {
            Ok(Self::Grok)
        } else {
            Err(D::Error::custom("unknown fx provider"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionPreferences {
    #[serde(default)]
    pub provider: ProviderId,
    pub model: String,
    pub effort: String,
    pub fast_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleKind {
    Command,
    FileMutation,
    StructuredTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    pub id: u64,
    pub kind: PermissionRuleKind,
    pub canonical: DurableBytes,
    pub display_identity: DurableBytes,
    pub decision: PermissionDecision,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionState {
    pub schema_version: u8,
    pub next_generation: u64,
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionState {
    fn default() -> Self {
        Self {
            schema_version: 2,
            next_generation: 1,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageAvailability {
    Complete,
    Pending,
    Incomplete,
    Legacy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageModel {
    pub model: String,
    pub first_sequence: u64,
    pub total_cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub billable_web_search_calls: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    VercelOidcToken,
    AiGatewayApiKey,
    FxLogin,
    StoredKey,
    ChatgptSubscription,
    GrokSubscription,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePendingLegacy {
    pub id: String,
    pub sequence: u64,
    pub origin: String,
    pub team: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePendingScoped {
    pub id: String,
    pub sequence: u64,
    pub provider: ProviderId,
    pub origin: String,
    pub team: Option<String>,
    pub credential_source: Option<CredentialSource>,
    pub credential_identity: Option<FxDigest>,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UsagePending {
    Scoped(UsagePendingScoped),
    Legacy(UsagePendingLegacy),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSnapshot {
    pub billing: UsageAvailability,
    pub api_duration_complete: bool,
    pub wall_duration_complete: bool,
    pub code_complete: bool,
    pub next_sequence: u64,
    pub settled_through_sequence: u64,
    pub api_duration_ms: u64,
    pub wall_duration_ms: u64,
    pub total_cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub billable_web_search_calls: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub models: Vec<UsageModel>,
    pub pending: Vec<UsagePending>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCause {
    NetworkInterrupted,
    ResponseInterrupted,
    ProviderUnavailable,
    RateLimited,
    SystemResumed,
    Authentication,
    RequestLimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    RetryingRequest,
    ContinuingResponse,
    RegeneratingTool,
    ContinuingAfterTool,
    ReconcilingTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryToolState {
    None,
    ProvenUnexecuted,
    Confirmed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAuthority {
    pub provider: ProviderId,
    pub model: DurableBytes,
    pub credential_source: Option<CredentialSource>,
    pub credential_identity: Option<FxDigest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCheckpoint {
    pub version: u8,
    pub turn_id: u64,
    pub user: crate::history::UserTurn,
    pub assistant_source: DurableBytes,
    pub execution: crate::history::ExecutionMemory,
    pub cause: RecoveryCause,
    pub action: RecoveryAction,
    pub tool_state: RecoveryToolState,
    pub authority: RecoveryAuthority,
    pub requested_fast_mode: bool,
    pub fast_mode: bool,
    pub max_provider_attempts: u64,
    pub consumed_provider_attempts: u64,
    pub outstanding_reservation: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalState {
    pub id: String,
    pub origin_workspace_root: String,
    pub workspace_root: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub conversation_language: String,
    pub preferences: SessionPreferences,
    pub history: Vec<HistoryTurn>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    #[serde(default)]
    pub context_history_start: u64,
    #[serde(default)]
    pub permission_state: PermissionState,
    #[serde(default)]
    pub last_subagent_work_id: Option<String>,
    #[serde(default)]
    pub usage: Option<UsageSnapshot>,
    #[serde(default)]
    pub recovery_checkpoint: Option<RecoveryCheckpoint>,
}

impl CanonicalState {
    pub fn removed_turn_count(&self) -> u64 {
        self.history
            .first()
            .and_then(HistoryTurn::compacted_summary)
            .map_or(0, |summary| summary.removed_turn_count)
    }

    pub fn logical_turns(&self) -> FxProviderResult<Vec<LogicalTurn>> {
        let base = self.removed_turn_count();
        let skip =
            usize::from(self.history.first().is_some_and(|turn| {
                turn.kind() == crate::history::HistoryTurnKind::CompactedSummary
            }));
        self.history[skip..]
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                let offset = u64::try_from(index)
                    .map_err(|_| FxProviderError::InvalidState("logical turn index overflow"))?;
                Ok(LogicalTurn {
                    absolute_ordinal: base.checked_add(offset).ok_or(
                        FxProviderError::InvalidState("logical turn ordinal overflow"),
                    )?,
                    turn: turn.clone(),
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogicalTurn {
    pub absolute_ordinal: u64,
    pub turn: HistoryTurn,
}

pub(crate) fn validate_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

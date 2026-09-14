use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ctx_history_core::{CoreDiscoveryExclusion, EventType, TypedKey};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use super::rows::{CodexSessionRow, MAX_CODEX_DURABLE_SESSION_ID_BYTES};

pub(super) const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 8;
const CODEX_SEMANTIC_CHECKPOINT_PREFIX: &str = "codex.projector-checkpoint.v8:";
pub(super) const MAX_CODEX_PENDING_CALLS: usize = 24;
pub(super) const MAX_CODEX_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_TERMINAL_AUTHORITIES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CodexTerminalAuthorityCheckpointV0 {
    Exact {
        #[serde(with = "terminal_fingerprints_base64")]
        fingerprints: Vec<u64>,
    },
    Saturated,
}

impl CodexTerminalAuthorityCheckpointV0 {
    pub(super) fn exact(fingerprints: Vec<u64>) -> Self {
        Self::Exact { fingerprints }
    }

    pub(super) const fn saturated() -> Self {
        Self::Saturated
    }

    pub(super) fn fingerprints(&self) -> Option<&[u64]> {
        match self {
            Self::Exact { fingerprints } => Some(fingerprints),
            Self::Saturated => None,
        }
    }

    fn validate_wire_state(&self) -> Result<(), serde_json::Error> {
        if let Self::Exact { fingerprints } = self {
            if fingerprints.len() > MAX_CODEX_TERMINAL_AUTHORITIES
                || fingerprints.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(invalid_checkpoint_state());
            }
        }
        Ok(())
    }
}

mod terminal_fingerprints_base64 {
    use super::*;

    const FINGERPRINT_BYTES: usize = std::mem::size_of::<u64>();
    const MAX_PACKED_BYTES: usize = MAX_CODEX_TERMINAL_AUTHORITIES * FINGERPRINT_BYTES;
    const MAX_ENCODED_BYTES: usize = MAX_PACKED_BYTES.div_ceil(3) * 4;

    pub(super) fn serialize<S>(fingerprints: &[u64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut packed = Vec::with_capacity(fingerprints.len() * FINGERPRINT_BYTES);
        for fingerprint in fingerprints {
            packed.extend_from_slice(&fingerprint.to_be_bytes());
        }
        serializer.serialize_str(&BASE64_STANDARD.encode(packed))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_ENCODED_BYTES {
            return Err(D::Error::custom("Codex terminal authority is oversized"));
        }
        let packed = BASE64_STANDARD
            .decode(encoded)
            .map_err(|_| D::Error::custom("Codex terminal authority is malformed"))?;
        if packed.len() > MAX_PACKED_BYTES || packed.len() % FINGERPRINT_BYTES != 0 {
            return Err(D::Error::custom("Codex terminal authority is malformed"));
        }
        packed
            .chunks_exact(FINGERPRINT_BYTES)
            .map(|bytes| {
                <[u8; FINGERPRINT_BYTES]>::try_from(bytes)
                    .map(u64::from_be_bytes)
                    .map_err(|_| D::Error::custom("Codex terminal authority is malformed"))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CodexPendingCallOriginV0 {
    CurrentSession,
    CopiedFromAncestor { ancestor_native_session_id: String },
    Unproven,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexPendingCallV0 {
    pub(super) raw_ordinal: u64,
    pub(super) origin: CodexPendingCallOriginV0,
    pub(super) result_event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) discovery_exclusion: Option<CoreDiscoveryExclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    owner: Option<CodexSessionRow>,
    local_turn_started: bool,
    pending_calls: BTreeMap<String, CodexPendingCallV0>,
    terminal_authority: CodexTerminalAuthorityCheckpointV0,
}

pub(super) struct CodexSemanticCheckpointState<'a> {
    pub(super) owner: Option<&'a CodexSessionRow>,
    pub(super) local_turn_started: bool,
    pub(super) pending_calls: &'a BTreeMap<String, CodexPendingCallV0>,
    pub(super) terminal_authority: CodexTerminalAuthorityCheckpointV0,
}

impl CodexSemanticCheckpoint {
    pub(super) fn from_state(
        state: CodexSemanticCheckpointState<'_>,
    ) -> Result<Option<Self>, serde_json::Error> {
        let checkpoint = Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            owner: state.owner.cloned(),
            local_turn_started: state.local_turn_started,
            pending_calls: state.pending_calls.clone(),
            terminal_authority: state.terminal_authority,
        };
        checkpoint.validate_wire_state()?;
        let encoded = serde_json::to_vec(&checkpoint)?;
        Ok((encoded.len() <= MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES).then_some(checkpoint))
    }

    pub(super) fn encode_key(&self) -> Result<TypedKey, serde_json::Error> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(size_error(encoded.len()));
        }
        TypedKey::utf8(format!("{CODEX_SEMANTIC_CHECKPOINT_PREFIX}{encoded}"))
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error.to_string())))
    }

    pub(super) fn decode_key(key: &TypedKey) -> Result<Self, serde_json::Error> {
        let TypedKey::Utf8(value) = key else {
            return Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Codex semantic checkpoint must be UTF-8",
            )));
        };
        let encoded = value
            .strip_prefix(CODEX_SEMANTIC_CHECKPOINT_PREFIX)
            .ok_or_else(|| {
                serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Codex semantic checkpoint prefix is invalid",
                ))
            })?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(size_error(encoded.len()));
        }
        let checkpoint: Self = serde_json::from_str(encoded)?;
        checkpoint.validate_wire_state()?;
        Ok(checkpoint)
    }

    pub(super) fn direct_append_safe(&self) -> bool {
        self.owner.is_some() && self.validate_wire_state().is_ok()
    }

    pub(super) fn owner(&self) -> Option<&CodexSessionRow> {
        self.owner.as_ref()
    }

    pub(super) const fn local_turn_started(&self) -> bool {
        self.local_turn_started
    }

    pub(super) fn pending_calls(&self) -> &BTreeMap<String, CodexPendingCallV0> {
        &self.pending_calls
    }

    pub(super) fn terminal_authority(&self) -> &CodexTerminalAuthorityCheckpointV0 {
        &self.terminal_authority
    }

    fn validate_wire_state(&self) -> Result<(), serde_json::Error> {
        let pending_calls_valid = self.pending_calls.len() <= MAX_CODEX_PENDING_CALLS
            && self.pending_calls.iter().all(|(call_id, pending)| {
                !call_id.is_empty()
                    && call_id.len() <= MAX_CODEX_CALL_ID_BYTES
                    && matches!(
                        pending.result_event_type,
                        EventType::ToolOutput | EventType::CommandOutput
                    )
                    && match &pending.origin {
                        CodexPendingCallOriginV0::CurrentSession
                        | CodexPendingCallOriginV0::Unproven => true,
                        CodexPendingCallOriginV0::CopiedFromAncestor {
                            ancestor_native_session_id,
                        } => {
                            !ancestor_native_session_id.is_empty()
                                && ancestor_native_session_id.len()
                                    <= MAX_CODEX_DURABLE_SESSION_ID_BYTES
                                && self.owner.as_ref().is_some_and(|owner| {
                                    owner.native_session_id != *ancestor_native_session_id
                                        && owner.parent_native_session_id.as_deref()
                                            == Some(ancestor_native_session_id.as_str())
                                })
                        }
                    }
            });
        if self.version != CODEX_SEMANTIC_CHECKPOINT_VERSION
            || !pending_calls_valid
            || self.terminal_authority.validate_wire_state().is_err()
        {
            return Err(invalid_checkpoint_state());
        }
        Ok(())
    }
}

fn invalid_checkpoint_state() -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Codex semantic checkpoint state is invalid",
    ))
}

fn size_error(actual: usize) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Codex semantic checkpoint is {actual} bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ctx_history_core::ProviderNativeSessionRelationship;

    fn owner() -> CodexSessionRow {
        CodexSessionRow {
            native_session_id: "019fb100-0000-7000-8000-000000000002".to_owned(),
            parent_native_session_id: Some("019fb100-0000-7000-8000-000000000001".to_owned()),
            root_native_session_id: None,
            session_relationship: Some(ProviderNativeSessionRelationship::Forked),
            started_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            cwd: None,
            originator: None,
            cli_version: None,
            source_kind: None,
            external_agent_id: None,
            role_hint: None,
            model_provider: None,
            git: None,
        }
    }

    #[test]
    fn neutral_checkpoint_round_trip_contains_only_scanner_continuation() {
        let pending_calls = BTreeMap::new();
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: None,
            local_turn_started: false,
            pending_calls: &pending_calls,
            terminal_authority: CodexTerminalAuthorityCheckpointV0::exact(Vec::new()),
        })
        .unwrap()
        .unwrap();
        let key = checkpoint.encode_key().unwrap();
        let TypedKey::Utf8(encoded) = &key else {
            panic!("Codex checkpoint must stay UTF-8");
        };

        assert!(encoded.starts_with(CODEX_SEMANTIC_CHECKPOINT_PREFIX));
        assert!(!encoded.contains("repository"));
        assert!(!encoded.contains("confidence"));
        assert!(!encoded.contains("effect"));
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&key).unwrap(),
            checkpoint
        );
        assert!(!checkpoint.direct_append_safe());
    }

    #[test]
    fn resumed_checkpoint_retains_exact_pending_copied_call() {
        let owner = owner();
        let ancestor_native_session_id = owner.parent_native_session_id.clone().unwrap();
        let pending_calls = BTreeMap::from([(
            "copied-call".to_owned(),
            CodexPendingCallV0 {
                raw_ordinal: 7,
                origin: CodexPendingCallOriginV0::CopiedFromAncestor {
                    ancestor_native_session_id,
                },
                result_event_type: EventType::CommandOutput,
                discovery_exclusion: Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
            },
        )]);
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: Some(&owner),
            local_turn_started: false,
            pending_calls: &pending_calls,
            terminal_authority: CodexTerminalAuthorityCheckpointV0::exact(vec![1, 2, 3]),
        })
        .unwrap()
        .unwrap();
        let decoded =
            CodexSemanticCheckpoint::decode_key(&checkpoint.encode_key().unwrap()).unwrap();

        assert!(decoded.direct_append_safe());
        assert_eq!(decoded.pending_calls(), &pending_calls);
        assert_eq!(
            decoded.terminal_authority().fingerprints(),
            Some(&[1, 2, 3][..])
        );
    }

    #[test]
    fn maximum_exact_terminal_authority_fits_the_bounded_checkpoint() {
        let owner = owner();
        let pending_calls = BTreeMap::new();
        let fingerprints = (0..MAX_CODEX_TERMINAL_AUTHORITIES as u64).collect::<Vec<_>>();
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: Some(&owner),
            local_turn_started: false,
            pending_calls: &pending_calls,
            terminal_authority: CodexTerminalAuthorityCheckpointV0::exact(fingerprints.clone()),
        })
        .unwrap()
        .expect("the 4,096-entry authority must fit");
        let encoded = checkpoint.encode_key().unwrap();
        let decoded = CodexSemanticCheckpoint::decode_key(&encoded).unwrap();

        assert!(decoded.direct_append_safe());
        assert_eq!(
            decoded.terminal_authority().fingerprints(),
            Some(&fingerprints[..])
        );
    }

    #[test]
    fn saturated_terminal_authority_round_trips_explicitly() {
        let owner = owner();
        let pending_calls = BTreeMap::new();
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: Some(&owner),
            local_turn_started: false,
            pending_calls: &pending_calls,
            terminal_authority: CodexTerminalAuthorityCheckpointV0::saturated(),
        })
        .unwrap()
        .unwrap();
        let decoded =
            CodexSemanticCheckpoint::decode_key(&checkpoint.encode_key().unwrap()).unwrap();

        assert!(decoded.terminal_authority().fingerprints().is_none());
    }
}

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{JsonlResumableSha256, JsonlSha256State, JsonlSourceIdentity};

const JSONL_PREFIX_HASH_DOMAIN: &[u8] = b"ctx-direct-jsonl-nativepath-prefix-v1\0";

#[inline]
pub fn new_jsonl_prefix_hasher() -> JsonlResumableSha256 {
    let mut hasher = JsonlResumableSha256::new();
    hasher.update(JSONL_PREFIX_HASH_DOMAIN);
    hasher
}

#[inline]
pub fn jsonl_prefix_digest(hasher: &JsonlResumableSha256) -> [u8; 32] {
    hasher.digest()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl JsonlObservedTime {
    pub fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlFileObservation {
    length: u64,
    modified: JsonlObservedTime,
    readonly: bool,
    stable_identity: Option<[u8; 32]>,
    change_identity: Option<[u8; 32]>,
}

impl JsonlFileObservation {
    pub fn new(
        length: u64,
        modified: SystemTime,
        readonly: bool,
        stable_identity: Option<[u8; 32]>,
        change_identity: Option<[u8; 32]>,
    ) -> Self {
        Self {
            length,
            modified: JsonlObservedTime::from_system_time(modified),
            readonly,
            stable_identity,
            change_identity,
        }
    }

    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn same_stable_file(&self, current: &Self) -> bool {
        match (self.stable_identity, current.stable_identity) {
            (Some(previous), Some(current)) => previous == current,
            _ => false,
        }
    }

    pub fn supports_exact_revalidation(&self) -> bool {
        self.stable_identity.is_some() && self.change_identity.is_some()
    }

    /// Whether two strong observations differ only in the platform change
    /// stamp. Unix ctime and Windows ChangeTime can move when a hard link is
    /// added or removed even though the named ordinary file and its bytes are
    /// unchanged. This is only a candidate for content authentication: it is
    /// never proof of equality by itself.
    pub fn differs_only_by_change_identity(&self, current: &Self) -> bool {
        self.length == current.length
            && self.modified == current.modified
            && self.readonly == current.readonly
            && self.supports_exact_revalidation()
            && current.supports_exact_revalidation()
            && self.same_stable_file(current)
            && self.change_identity != current.change_identity
    }

    /// Whether `current` is physically eligible to contain the frozen prefix.
    /// Strict callers must still authenticate that prefix. A caller using an
    /// explicit append-only provider contract may instead trust that contract.
    pub fn admits_frozen_prefix_in(&self, current: &Self) -> bool {
        self == current
            || (current.length >= self.length
                && self.supports_exact_revalidation()
                && self.same_stable_file(current)
                && self.readonly == current.readonly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlCheckpoint {
    version: u32,
    identity: JsonlSourceIdentity,
    source_observation: JsonlFileObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    logical_eof: Option<u64>,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    complete_prefix_sha256_state: Option<JsonlSha256State>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admitted_eof_sha256_state: Option<JsonlSha256State>,
    next_physical_ordinal: u64,
    terminal: bool,
}

impl JsonlCheckpoint {
    const VERSION: u32 = 1;

    pub fn new(
        identity: JsonlSourceIdentity,
        source_observation: JsonlFileObservation,
        complete_prefix_end: u64,
        complete_prefix_sha256: [u8; 32],
        next_physical_ordinal: u64,
        terminal: bool,
    ) -> Self {
        Self {
            version: Self::VERSION,
            identity,
            source_observation,
            logical_eof: None,
            complete_prefix_end,
            complete_prefix_sha256,
            complete_prefix_sha256_state: None,
            admitted_eof_sha256_state: None,
            next_physical_ordinal,
            terminal,
        }
    }

    pub fn new_with_prefix_state(
        identity: JsonlSourceIdentity,
        source_observation: JsonlFileObservation,
        complete_prefix_end: u64,
        complete_prefix_hasher: &JsonlResumableSha256,
        admitted_eof_hasher: Option<&JsonlResumableSha256>,
        next_physical_ordinal: u64,
        terminal: bool,
    ) -> Self {
        Self::new_with_prefix_state_and_logical_eof(
            identity,
            source_observation,
            None,
            complete_prefix_end,
            complete_prefix_hasher,
            admitted_eof_hasher,
            next_physical_ordinal,
            terminal,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_prefix_state_and_logical_eof(
        identity: JsonlSourceIdentity,
        source_observation: JsonlFileObservation,
        logical_eof: Option<u64>,
        complete_prefix_end: u64,
        complete_prefix_hasher: &JsonlResumableSha256,
        admitted_eof_hasher: Option<&JsonlResumableSha256>,
        next_physical_ordinal: u64,
        terminal: bool,
    ) -> Self {
        Self {
            version: Self::VERSION,
            identity,
            source_observation,
            logical_eof,
            complete_prefix_end,
            complete_prefix_sha256: complete_prefix_hasher.digest(),
            complete_prefix_sha256_state: Some(complete_prefix_hasher.snapshot()),
            admitted_eof_sha256_state: admitted_eof_hasher.map(JsonlResumableSha256::snapshot),
            next_physical_ordinal,
            terminal,
        }
    }

    pub fn identity(&self) -> &JsonlSourceIdentity {
        &self.identity
    }

    pub fn source_observation(&self) -> &JsonlFileObservation {
        &self.source_observation
    }

    /// Provider-authoritative committed boundary when it is narrower than, or
    /// independently advances within, the retained physical observation.
    pub fn logical_eof(&self) -> Option<u64> {
        self.logical_eof
    }

    pub fn admitted_length(&self) -> u64 {
        self.logical_eof.unwrap_or(self.source_observation.length)
    }

    pub fn complete_prefix_end(&self) -> u64 {
        self.complete_prefix_end
    }

    pub fn complete_prefix_sha256(&self) -> &[u8; 32] {
        &self.complete_prefix_sha256
    }

    pub fn restore_complete_prefix_hasher(&self) -> Option<JsonlResumableSha256> {
        let hasher = JsonlResumableSha256::restore(self.complete_prefix_sha256_state.as_ref()?)?;
        let domain_bytes = u64::try_from(JSONL_PREFIX_HASH_DOMAIN.len()).ok()?;
        (hasher.bytes_hashed() == domain_bytes.checked_add(self.complete_prefix_end)?
            && hasher.digest() == self.complete_prefix_sha256)
            .then_some(hasher)
    }

    pub fn restore_admitted_eof_hasher(&self) -> Option<JsonlResumableSha256> {
        let hasher = JsonlResumableSha256::restore(self.admitted_eof_sha256_state.as_ref()?)?;
        (hasher.bytes_hashed() == self.admitted_length()).then_some(hasher)
    }

    pub fn admitted_eof_sha256(&self) -> Option<[u8; 32]> {
        self.restore_admitted_eof_hasher()
            .map(|hasher| hasher.digest())
    }

    pub fn authenticates_admitted_eof(&self) -> bool {
        self.admitted_eof_sha256().is_some() || self.complete_prefix_end == self.admitted_length()
    }

    pub fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub fn terminal(&self) -> bool {
        self.terminal
    }

    pub fn is_internally_consistent(&self) -> bool {
        let empty_prefix = self.complete_prefix_end == 0;
        let compressed_physical_ordinals = self.identity.has_compressed_physical_ordinals();
        let empty_prefix_is_exact = (self.next_physical_ordinal == 0
            || compressed_physical_ordinals)
            && self.complete_prefix_sha256 == jsonl_prefix_digest(&new_jsonl_prefix_hasher());
        // A decoded compressed stream may contain more logical records than
        // physical source bytes. Its ordinal only needs to be nonzero once the
        // certified physical prefix is nonempty; raw streams retain the
        // stricter record-count-to-byte bound.
        let nonempty_prefix_is_possible = self.next_physical_ordinal > 0
            && (compressed_physical_ordinals
                || self.next_physical_ordinal <= self.complete_prefix_end);
        self.version == Self::VERSION
            && self.admitted_length() <= self.source_observation.length
            && self.complete_prefix_end <= self.source_observation.length
            && self.complete_prefix_end <= self.admitted_length()
            && if empty_prefix {
                empty_prefix_is_exact
            } else {
                nonempty_prefix_is_possible
            }
            && (!self.terminal || self.complete_prefix_end == self.admitted_length())
            && self
                .complete_prefix_sha256_state
                .as_ref()
                .is_none_or(|_| self.restore_complete_prefix_hasher().is_some())
            && self
                .admitted_eof_sha256_state
                .as_ref()
                .is_none_or(|_| self.restore_admitted_eof_hasher().is_some())
    }

    pub fn supports(&self, identity: &JsonlSourceIdentity) -> bool {
        self.is_internally_consistent() && self.identity == *identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlSourceChange {
    Cold,
    Unchanged,
    Append,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlOversizedRecordPolicy {
    RejectSource,
    RejectRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlRecordEvidence {
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_digest: [u8; 32],
}

impl JsonlRecordEvidence {
    #[inline]
    pub fn new(
        physical_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        record_digest: [u8; 32],
    ) -> Self {
        Self {
            physical_ordinal,
            byte_start,
            byte_end_exclusive,
            record_digest,
        }
    }

    #[inline]
    pub fn physical_ordinal(self) -> u64 {
        self.physical_ordinal
    }

    #[inline]
    pub fn byte_start(self) -> u64 {
        self.byte_start
    }

    #[inline]
    pub fn byte_end_exclusive(self) -> u64 {
        self.byte_end_exclusive
    }

    #[inline]
    pub fn record_digest(self) -> [u8; 32] {
        self.record_digest
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JsonlRecordRef<'record> {
    bytes: &'record [u8],
    evidence: JsonlRecordEvidence,
    oversized: bool,
}

impl<'record> JsonlRecordRef<'record> {
    #[inline]
    pub fn new(bytes: &'record [u8], evidence: JsonlRecordEvidence, oversized: bool) -> Self {
        Self {
            bytes,
            evidence,
            oversized,
        }
    }

    #[doc(hidden)]
    pub fn for_test(bytes: &'record [u8], physical_ordinal: u64) -> Self {
        Self::new(
            bytes,
            JsonlRecordEvidence::new(
                physical_ordinal,
                0,
                bytes.len() as u64,
                Sha256::digest(bytes).into(),
            ),
            false,
        )
    }

    #[inline]
    pub fn bytes(self) -> &'record [u8] {
        self.bytes
    }

    #[inline]
    pub fn evidence(self) -> JsonlRecordEvidence {
        self.evidence
    }

    #[inline]
    pub fn oversized(self) -> bool {
        self.oversized
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlPage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlScanOutcome {
    checkpoint: JsonlCheckpoint,
}

impl JsonlScanOutcome {
    pub fn new(checkpoint: JsonlCheckpoint) -> Self {
        Self { checkpoint }
    }

    pub fn checkpoint(&self) -> &JsonlCheckpoint {
        &self.checkpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_identity_only_candidate_requires_all_portable_object_facts_to_match() {
        let retained =
            JsonlFileObservation::new(17, UNIX_EPOCH, false, Some([1; 32]), Some([2; 32]));
        let change_time_only =
            JsonlFileObservation::new(17, UNIX_EPOCH, false, Some([1; 32]), Some([3; 32]));

        assert!(retained.differs_only_by_change_identity(&change_time_only));
        assert!(
            !retained.differs_only_by_change_identity(&JsonlFileObservation::new(
                17,
                UNIX_EPOCH,
                false,
                Some([4; 32]),
                Some([3; 32]),
            ))
        );
        assert!(
            !retained.differs_only_by_change_identity(&JsonlFileObservation::new(
                18,
                UNIX_EPOCH,
                false,
                Some([1; 32]),
                Some([3; 32]),
            ))
        );
        assert!(
            !retained.differs_only_by_change_identity(&JsonlFileObservation::new(
                17,
                UNIX_EPOCH + std::time::Duration::from_secs(1),
                false,
                Some([1; 32]),
                Some([3; 32]),
            ))
        );
        assert!(
            !retained.differs_only_by_change_identity(&JsonlFileObservation::new(
                17,
                UNIX_EPOCH,
                true,
                Some([1; 32]),
                Some([3; 32]),
            ))
        );
        assert!(
            !retained.differs_only_by_change_identity(&JsonlFileObservation::new(
                17,
                UNIX_EPOCH,
                false,
                None,
                Some([3; 32]),
            ))
        );
    }

    fn continuation_checkpoint() -> JsonlCheckpoint {
        let bytes = b"one complete JSONL record\n";
        let mut complete = new_jsonl_prefix_hasher();
        complete.update(bytes);
        let mut admitted = JsonlResumableSha256::new();
        admitted.update(bytes);
        JsonlCheckpoint::new_with_prefix_state(
            JsonlSourceIdentity::new("test", "parser-v1", "policy-v1", [3; 32], "/tmp/test"),
            JsonlFileObservation::new(
                bytes.len() as u64,
                UNIX_EPOCH,
                false,
                Some([4; 32]),
                Some([5; 32]),
            ),
            bytes.len() as u64,
            &complete,
            Some(&admitted),
            1,
            true,
        )
    }

    #[test]
    fn continuation_state_is_optional_for_strict_readers_and_invalid_for_direct_resume() {
        let current = continuation_checkpoint();
        assert!(current.is_internally_consistent());
        assert!(current.restore_complete_prefix_hasher().is_some());
        assert!(current.restore_admitted_eof_hasher().is_some());

        let legacy = JsonlCheckpoint::new(
            current.identity.clone(),
            current.source_observation.clone(),
            current.complete_prefix_end,
            current.complete_prefix_sha256,
            current.next_physical_ordinal,
            current.terminal,
        );
        assert!(legacy.is_internally_consistent());
        assert!(legacy.restore_complete_prefix_hasher().is_none());
        assert!(legacy.restore_admitted_eof_hasher().is_none());
    }

    #[test]
    fn corrupt_or_wrong_bound_continuation_state_is_inert() {
        let current = continuation_checkpoint();
        let mut corrupt = serde_json::to_value(&current).unwrap();
        corrupt["complete_prefix_sha256_state"]["state"][0] = serde_json::json!(0);
        let corrupt: JsonlCheckpoint = serde_json::from_value(corrupt).unwrap();
        assert!(!corrupt.is_internally_consistent());
        assert!(corrupt.restore_complete_prefix_hasher().is_none());

        let mut wrong_bound = serde_json::to_value(&current).unwrap();
        wrong_bound["complete_prefix_end"] =
            serde_json::json!(current.complete_prefix_end.saturating_sub(1));
        wrong_bound["terminal"] = serde_json::json!(false);
        let wrong_bound: JsonlCheckpoint = serde_json::from_value(wrong_bound).unwrap();
        assert!(!wrong_bound.is_internally_consistent());
        assert!(wrong_bound.restore_complete_prefix_hasher().is_none());
    }

    #[test]
    fn only_standard_zstd_checkpoints_allow_more_ordinals_than_physical_bytes() {
        let observation = JsonlFileObservation::new(1, UNIX_EPOCH, false, None, None);
        let checkpoint = |policy_revision| {
            JsonlCheckpoint::new(
                JsonlSourceIdentity::new(
                    "test",
                    "parser-v1",
                    policy_revision,
                    [3; 32],
                    "/tmp/test",
                ),
                observation.clone(),
                1,
                [4; 32],
                2,
                true,
            )
        };

        assert!(!checkpoint("policy-v1").is_internally_consistent());
        assert!(checkpoint("policy-v1:standard-zstd-jsonl-v1").is_internally_consistent());
    }
}

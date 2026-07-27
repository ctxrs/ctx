use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ContentRef, ErrorClass, ProtocolError, PROJECTION_CONTRACT_VERSION, PROTOCOL_FINGERPRINT,
};

pub const MAX_JOURNAL_RECORDS_PER_BATCH: usize = 512;
pub const MAX_JOURNAL_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_JOURNAL_EVIDENCE_PER_RECORD: usize = 32;
pub const MAX_JOURNAL_IDENTITY_BYTES: usize = 4 * 1024;
pub const MAX_AUTHORIZED_REPOSITORY_ROOTS: usize = 128;
pub const MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES: usize = 4 * 1024;
pub const MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES: usize = 256 * 1024;
/// The complete JSON `HostEnvelope` carrying one journal request is capped at
/// four MiB, independently of the larger generic framing limit.
pub const MAX_JOURNAL_SYNC_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RESULT_CONTENT_ITEMS_PER_REQUEST: usize = MAX_JOURNAL_RECORDS_PER_BATCH;
pub const MAX_RESULT_CONTENT_BYTES_PER_ITEM: usize = 256 * 1024;
pub const MAX_RESULT_CONTENT_TOTAL_BYTES: usize = 1024 * 1024;

const JOURNAL_INITIAL_DIGEST_DOMAIN: &[u8] = b"ctx-pro-journal-initial-v1\0";
const JOURNAL_RECORD_DIGEST_DOMAIN: &[u8] = b"ctx-pro-journal-record-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalSyncMode {
    FullBaseline,
    Incremental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperation {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntityKind {
    Event,
    FileTouch,
    VcsChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalPosition {
    pub generation: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalCheckpoint {
    pub position: JournalPosition,
    pub contract_fingerprint: String,
    pub cumulative_digest: String,
}

impl JournalCheckpoint {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.position.generation == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "journal generation must be positive",
            ));
        };
        if self.contract_fingerprint != PROTOCOL_FINGERPRINT {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "journal checkpoint protocol fingerprint does not match Protocol V1",
            ));
        }
        validate_lower_hex(&self.cumulative_digest, "journal cumulative")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEvidenceIdentity {
    pub event_id: Uuid,
    pub source_id: Option<Uuid>,
    pub source_path: Option<String>,
    pub source_record_ordinal: Option<u64>,
    pub source_record_subrecord_index: Option<u32>,
    pub byte_start: Option<u64>,
    pub byte_end_exclusive: Option<u64>,
}

impl JournalEvidenceIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.event_id.is_nil() || self.source_id.is_some_and(|id| id.is_nil()) {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "journal evidence contains a nil canonical identity",
            ));
        }
        validate_optional_identity(&self.source_path, "journal evidence source path")?;
        if self.source_record_subrecord_index.is_some() && self.source_record_ordinal.is_none() {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "journal evidence subrecord requires a record ordinal",
            ));
        }
        match (self.byte_start, self.byte_end_exclusive) {
            (Some(start), Some(end)) if start <= end => {}
            (None, None) => {}
            _ => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "journal evidence byte range is incomplete or reversed",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalProvenanceIdentity {
    pub entity_kind: JournalEntityKind,
    pub stable_entity_id: Uuid,
    pub capture_source_id: Option<Uuid>,
    pub provider: Option<String>,
    pub provider_external_id: Option<String>,
}

impl JournalProvenanceIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.stable_entity_id.is_nil() || self.capture_source_id.is_some_and(|id| id.is_nil()) {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "journal provenance contains a nil canonical identity",
            ));
        }
        validate_optional_identity(&self.provider, "journal provenance provider")?;
        validate_optional_identity(&self.provider_external_id, "journal provenance external ID")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    pub generation: u64,
    pub sequence: u64,
    pub projection_contract_version: u32,
    pub entity_kind: JournalEntityKind,
    pub stable_entity_id: Uuid,
    pub entity_revision: u64,
    pub operation: JournalOperation,
    /// Canonical JSON projected by the public Store. Its digest uses recursively
    /// sorted object keys, no whitespace, and integer-only numbers. Deletes carry `None`.
    pub canonical_payload: Option<serde_json::Value>,
    pub payload_sha256: String,
    pub evidence: Vec<JournalEvidenceIdentity>,
    pub provenance: JournalProvenanceIdentity,
    pub cumulative_digest: String,
}

/// Complete normalized result bytes carried only for one helper request.
///
/// These bytes are excluded from canonical payload hashes, record digests,
/// cumulative digests, and durable journal chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultContentSidecar {
    pub journal_sequence: u64,
    pub stable_entity_id: Uuid,
    pub content_ref: ContentRef,
    pub content: String,
}

impl JournalRecord {
    pub fn validate(&self, prior_digest: &str) -> Result<(), ProtocolError> {
        if self.generation == 0 || self.sequence == 0 || self.entity_revision == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "journal generation, sequence, and entity revision must be positive",
            ));
        }
        if self.projection_contract_version != PROJECTION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "journal projection contract is not Protocol V1",
            ));
        }
        if self.stable_entity_id.is_nil()
            || self.provenance.entity_kind != self.entity_kind
            || self.provenance.stable_entity_id != self.stable_entity_id
        {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "journal record identity does not match its provenance",
            ));
        }
        self.provenance.validate()?;
        if self.evidence.len() > MAX_JOURNAL_EVIDENCE_PER_RECORD {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                format!(
                    "journal record has {} evidence anchors; maximum is {}",
                    self.evidence.len(),
                    MAX_JOURNAL_EVIDENCE_PER_RECORD
                ),
            ));
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        let payload_bytes = match (&self.operation, &self.canonical_payload) {
            (JournalOperation::Upsert, Some(payload)) => canonical_payload_bytes(payload)?,
            (JournalOperation::Delete, None) => Vec::new(),
            _ => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "journal upserts require a payload and tombstones must omit it",
                ));
            }
        };
        if sha256_hex(&payload_bytes) != self.payload_sha256 {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "journal payload digest does not match its canonical bytes",
            ));
        }
        validate_lower_hex(prior_digest, "prior journal cumulative")?;
        validate_lower_hex(&self.cumulative_digest, "journal cumulative")?;
        if self.cumulative_digest != journal_record_digest(prior_digest, self)? {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "journal cumulative digest does not match the record chain",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalSyncRequest {
    pub mode: JournalSyncMode,
    pub canonical_schema_version: u32,
    pub canonical_schema_identity: String,
    pub projection_contract_version: u32,
    pub contract_fingerprint: String,
    pub prior_checkpoint: JournalCheckpoint,
    /// Immutable terminal checkpoint captured in the same Store read transaction
    /// as every record in this request.
    pub frozen_through: JournalCheckpoint,
    /// Activity-observed repository locators the helper may inspect using only
    /// the exact Git executable handed over by the host. These are not source
    /// body permissions and do not authorize filesystem discovery.
    pub authorized_repository_roots: Vec<String>,
    pub records: Vec<JournalRecord>,
    pub result_contents: Vec<ResultContentSidecar>,
}

impl JournalSyncRequest {
    #[allow(clippy::too_many_lines)] // Exhaustive frozen journal invariants stay together.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.canonical_schema_version == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "canonical schema version must be positive",
            ));
        }
        if self.canonical_schema_identity.trim().is_empty()
            || self.canonical_schema_identity.len() > MAX_JOURNAL_IDENTITY_BYTES
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "canonical schema identity is empty or exceeds its byte bound",
            ));
        }
        if self.projection_contract_version != PROJECTION_CONTRACT_VERSION
            || self.contract_fingerprint != PROTOCOL_FINGERPRINT
        {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "journal request does not match the exact Protocol V1 contract",
            ));
        }
        self.prior_checkpoint.validate()?;
        self.frozen_through.validate()?;
        validate_authorized_repository_roots(&self.authorized_repository_roots)?;
        if self.prior_checkpoint.position.generation != self.frozen_through.position.generation
            || self.prior_checkpoint.position.sequence > self.frozen_through.position.sequence
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "journal checkpoints do not describe one forward-moving generation",
            ));
        }
        match self.mode {
            JournalSyncMode::FullBaseline if self.prior_checkpoint.position.sequence != 0 => {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "full baseline must start at sequence zero",
                ));
            }
            JournalSyncMode::Incremental if self.prior_checkpoint.position.sequence == 0 => {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "a new generation must be synchronized as a full baseline",
                ));
            }
            _ => {}
        }
        if self.prior_checkpoint.position.sequence == 0
            && self.prior_checkpoint.cumulative_digest
                != initial_journal_digest(self.prior_checkpoint.position.generation)
        {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "sequence-zero journal checkpoint has the wrong generation digest",
            ));
        }
        if self.records.len() > MAX_JOURNAL_RECORDS_PER_BATCH {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                format!(
                    "journal batch has {} records; maximum is {}",
                    self.records.len(),
                    MAX_JOURNAL_RECORDS_PER_BATCH
                ),
            ));
        }
        self.validate_result_contents()?;
        let mut sequence = self.prior_checkpoint.position.sequence;
        let mut digest = self.prior_checkpoint.cumulative_digest.as_str();
        for record in &self.records {
            sequence = sequence.checked_add(1).ok_or_else(|| {
                ProtocolError::new(ErrorClass::Sequence, "journal sequence overflowed")
            })?;
            if record.generation != self.prior_checkpoint.position.generation
                || record.sequence != sequence
                || record.sequence > self.frozen_through.position.sequence
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "journal batch records are not contiguous inside the frozen generation",
                ));
            }
            record.validate(digest)?;
            digest = &record.cumulative_digest;
        }
        if self.records.is_empty() && self.prior_checkpoint.position != self.frozen_through.position
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "a nonterminal journal request must contain the next record",
            ));
        }
        if sequence == self.frozen_through.position.sequence
            && digest != self.frozen_through.cumulative_digest
        {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "terminal journal record does not match the frozen cumulative digest",
            ));
        }
        if journal_sync_envelope_bytes(self)? > MAX_JOURNAL_SYNC_ENVELOPE_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "journal request exceeds its dedicated envelope byte bound",
            ));
        }
        Ok(())
    }

    fn validate_result_contents(&self) -> Result<(), ProtocolError> {
        if self.result_contents.len() > MAX_RESULT_CONTENT_ITEMS_PER_REQUEST {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "journal request exceeds its result-content item bound",
            ));
        }
        let mut total_bytes = 0_usize;
        let mut bindings = BTreeSet::new();
        for sidecar in &self.result_contents {
            let bytes = sidecar.content.as_bytes();
            if sidecar.journal_sequence == 0
                || sidecar.stable_entity_id.is_nil()
                || bytes.len() > MAX_RESULT_CONTENT_BYTES_PER_ITEM
            {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "result content has an invalid identity or exceeds its item byte bound",
                ));
            }
            total_bytes = total_bytes.checked_add(bytes.len()).ok_or_else(|| {
                ProtocolError::new(ErrorClass::Bounds, "result-content byte total overflowed")
            })?;
            if total_bytes > MAX_RESULT_CONTENT_TOTAL_BYTES {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "journal request exceeds its result-content total byte bound",
                ));
            }
            if !bindings.insert((sidecar.journal_sequence, sidecar.stable_entity_id)) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "result content must bind uniquely to one journal record",
                ));
            }
            if !sidecar.content_ref.verifies(bytes) {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "result content does not match its SHA-256 and byte length",
                ));
            }
            let Some(record) = self.records.iter().find(|record| {
                record.sequence == sidecar.journal_sequence
                    && record.stable_entity_id == sidecar.stable_entity_id
            }) else {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "result content does not bind to a record in this request",
                ));
            };
            let canonical_ref = record
                .canonical_payload
                .as_ref()
                .and_then(|payload| payload.pointer("/result/content_ref"))
                .and_then(|value| serde_json::from_value::<ContentRef>(value.clone()).ok());
            if record.entity_kind != JournalEntityKind::Event
                || record.operation != JournalOperation::Upsert
                || canonical_ref.as_ref() != Some(&sidecar.content_ref)
            {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "result content reference does not match the canonical result contract",
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn committed_checkpoint(&self) -> JournalCheckpoint {
        self.records.last().map_or_else(
            || self.prior_checkpoint.clone(),
            |record| JournalCheckpoint {
                position: JournalPosition {
                    generation: record.generation,
                    sequence: record.sequence,
                },
                contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
                cumulative_digest: record.cumulative_digest.clone(),
            },
        )
    }
}

/// Exact serialized size of the largest-shaped host envelope carrying `request`.
pub fn journal_sync_envelope_bytes(request: &JournalSyncRequest) -> Result<usize, ProtocolError> {
    let envelope = crate::HostEnvelope {
        sequence: u64::MAX,
        request_id: Uuid::from_u128(u128::MAX),
        message: crate::HostMessage::SyncJournal(request.clone()),
    };
    serde_json::to_vec(&envelope)
        .map(|bytes| bytes.len())
        .map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "journal envelope could not be encoded for bounds validation",
            )
        })
}

fn validate_authorized_repository_roots(roots: &[String]) -> Result<(), ProtocolError> {
    if roots.len() > MAX_AUTHORIZED_REPOSITORY_ROOTS {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "journal request exceeds its authorized repository root count",
        ));
    }
    let mut total = 0_usize;
    let mut prior: Option<&str> = None;
    for root in roots {
        if root.is_empty()
            || root.len() > MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES
            || root.chars().any(char::is_control)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "authorized repository root is empty, unsafe, or exceeds its byte bound",
            ));
        }
        if prior.is_some_and(|value| value >= root.as_str()) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "authorized repository roots must be strictly sorted and unique",
            ));
        }
        total = total.checked_add(root.len()).ok_or_else(|| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "authorized repository root bytes overflowed",
            )
        })?;
        prior = Some(root);
    }
    if total > MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "journal request exceeds its authorized repository root byte bound",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalSyncResult {
    pub committed_through: JournalCheckpoint,
    pub accepted_records: u32,
    pub replayed: bool,
    pub frozen_complete: bool,
}

/// Digest for the sequence-zero checkpoint of one immutable journal generation.
#[must_use]
pub fn initial_journal_digest(generation: u64) -> String {
    let mut hash = Sha256::new();
    hash.update(JOURNAL_INITIAL_DIGEST_DOMAIN);
    hash.update(generation.to_be_bytes());
    hash.update(PROTOCOL_FINGERPRINT.as_bytes());
    sha256_output_hex(&hash.finalize())
}

/// Computes the cumulative digest for `record` from the preceding checkpoint.
pub fn journal_record_digest(
    prior_digest: &str,
    record: &JournalRecord,
) -> Result<String, ProtocolError> {
    let prior = decode_lower_hex(prior_digest).ok_or_else(|| {
        ProtocolError::new(
            ErrorClass::InvalidRequest,
            "prior journal cumulative digest is invalid",
        )
    })?;
    #[derive(Serialize)]
    struct DigestInput<'a> {
        generation: u64,
        sequence: u64,
        projection_contract_version: u32,
        entity_kind: JournalEntityKind,
        stable_entity_id: Uuid,
        entity_revision: u64,
        operation: JournalOperation,
        payload_sha256: &'a str,
        evidence: &'a [JournalEvidenceIdentity],
        provenance: &'a JournalProvenanceIdentity,
    }
    let input = DigestInput {
        generation: record.generation,
        sequence: record.sequence,
        projection_contract_version: record.projection_contract_version,
        entity_kind: record.entity_kind,
        stable_entity_id: record.stable_entity_id,
        entity_revision: record.entity_revision,
        operation: record.operation,
        payload_sha256: &record.payload_sha256,
        evidence: &record.evidence,
        provenance: &record.provenance,
    };
    let bytes = serde_json::to_vec(&input).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "journal digest input could not be encoded",
        )
    })?;
    let mut hash = Sha256::new();
    hash.update(JOURNAL_RECORD_DIGEST_DOMAIN);
    hash.update(prior);
    hash.update(bytes);
    Ok(sha256_output_hex(&hash.finalize()))
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    sha256_output_hex(&Sha256::digest(bytes))
}

/// Canonical JSON bytes used by the Store and helper to verify payload identity.
pub fn canonical_payload_bytes(payload: &serde_json::Value) -> Result<Vec<u8>, ProtocolError> {
    if contains_float(payload) {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            "journal payload contains a non-canonical floating-point number",
        ));
    }
    let bytes = serde_json::to_vec(payload).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "journal payload could not be canonically encoded",
        )
    })?;
    if bytes.len() > MAX_JOURNAL_PAYLOAD_BYTES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("canonical journal payload exceeds {MAX_JOURNAL_PAYLOAD_BYTES} bytes"),
        ));
    }
    Ok(bytes)
}

fn contains_float(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => !number.is_i64() && !number.is_u64(),
        serde_json::Value::Array(values) => values.iter().any(contains_float),
        serde_json::Value::Object(values) => values.values().any(contains_float),
        _ => false,
    }
}

fn validate_optional_identity(value: &Option<String>, name: &str) -> Result<(), ProtocolError> {
    if value
        .as_deref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_JOURNAL_IDENTITY_BYTES)
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty or exceeds {MAX_JOURNAL_IDENTITY_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn validate_lower_hex(value: &str, name: &str) -> Result<(), ProtocolError> {
    if decode_lower_hex(value).is_none() {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{name} digest must be 64 lowercase hexadecimal characters"),
        ));
    }
    Ok(())
}

fn decode_lower_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(output)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn sha256_output_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

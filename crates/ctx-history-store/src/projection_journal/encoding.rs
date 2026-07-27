use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{JournalEntityKind, JournalOperation, Result, StoreError, PROJECTION_CONTRACT_VERSION};

const JOURNAL_INITIAL_DIGEST_DOMAIN: &[u8] = b"ctx-pro-journal-initial-v1\0";
const JOURNAL_RECORD_DIGEST_DOMAIN: &[u8] = b"ctx-pro-journal-record-v1\0";

pub(super) struct RecordDigestFields<'a> {
    pub operation: JournalOperation,
    pub payload_sha256: &'a str,
    pub evidence_json: &'a str,
    pub provenance_json: &'a str,
}

pub(super) fn validate_contract_fingerprint(value: &str) -> Result<()> {
    if !is_lower_hex_digest(value) {
        return Err(StoreError::InvalidProjectionContractFingerprint);
    }
    Ok(())
}

pub(super) fn validate_digest(value: &str, field: &str) -> Result<()> {
    if !is_lower_hex_digest(value) {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "invalid {field}"
        )));
    }
    Ok(())
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(super) fn canonical_json(value: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(value)?;
    reject_floats(&value)?;
    serde_json::to_string(&value).map_err(StoreError::from)
}

fn reject_floats(value: &Value) -> Result<()> {
    match value {
        Value::Number(number) if number.as_i64().is_none() && number.as_u64().is_none() => {
            Err(StoreError::InvalidProjectionJournalData(
                "floating-point canonical JSON is unsupported".to_owned(),
            ))
        }
        Value::Array(values) => {
            for value in values {
                reject_floats(value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for value in values.values() {
                reject_floats(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn generation_digest(generation: u64, fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(JOURNAL_INITIAL_DIGEST_DOMAIN);
    hasher.update(generation.to_be_bytes());
    hasher.update(fingerprint.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn content_digest(
    operation: JournalOperation,
    payload_sha256: &str,
    evidence_json: &str,
    provenance_json: &str,
) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"ctx-projection-journal-content-v1");
    hash_field(&mut hasher, operation.as_str().as_bytes());
    hash_field(&mut hasher, payload_sha256.as_bytes());
    hash_field(&mut hasher, evidence_json.as_bytes());
    hash_field(&mut hasher, provenance_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn record_chain_digest(
    previous: &str,
    generation: u64,
    sequence: u64,
    kind: JournalEntityKind,
    id: Uuid,
    revision: u64,
    fields: RecordDigestFields<'_>,
) -> Result<String> {
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
        evidence: &'a [super::JournalEvidenceIdentity],
        provenance: &'a super::JournalProvenanceIdentity,
    }
    let evidence =
        serde_json::from_str::<Vec<super::JournalEvidenceIdentity>>(fields.evidence_json)?;
    let provenance =
        serde_json::from_str::<super::JournalProvenanceIdentity>(fields.provenance_json)?;
    let input = DigestInput {
        generation,
        sequence,
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        entity_kind: kind,
        stable_entity_id: id,
        entity_revision: revision,
        operation: fields.operation,
        payload_sha256: fields.payload_sha256,
        evidence: &evidence,
        provenance: &provenance,
    };
    let bytes = serde_json::to_vec(&input)?;
    let mut hasher = Sha256::new();
    hasher.update(JOURNAL_RECORD_DIGEST_DOMAIN);
    hasher.update(decode_digest(previous));
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn decode_digest(value: &str) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    bytes
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub(super) fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

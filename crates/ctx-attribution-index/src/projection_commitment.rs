use sha2::{Digest as _, Sha256};
use thiserror::Error;

use ctx_history_core::{SourceKey, StableEntityId};

use super::model::ServingRecord;

const MAX_PROJECTION_RECORD_ID_BYTES: usize = 512;
const EVENT_OUTPUT_ROOT_DOMAIN: &[u8] = b"ctx-pro-event-output-root-v1\0";

#[derive(Debug, Error)]
pub enum ProjectionCommitmentError {
    #[error("projection commitment serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("projection commitment identity is invalid: {0}")]
    Invalid(&'static str),
    #[error("projection commitment count exceeds its fixed-width encoding")]
    Count,
}

pub fn canonical_flat_record_sha256(
    record: &ServingRecord,
) -> Result<[u8; 32], ProjectionCommitmentError> {
    record
        .validate()
        .map_err(|_| ProjectionCommitmentError::Invalid("Flat record"))?;
    Ok(Sha256::digest(serde_json::to_vec(record)?).into())
}

#[allow(clippy::too_many_arguments)]
pub fn event_output_root<'a>(
    publication_generation: u64,
    source: &SourceKey,
    event_id: StableEntityId,
    event_sequence: u64,
    core_record_sha256: &str,
    core_record_leaf_sha256: &str,
    records: impl IntoIterator<Item = (&'a str, [u8; 32])>,
) -> Result<(u32, String), ProjectionCommitmentError> {
    if publication_generation == 0 {
        return Err(ProjectionCommitmentError::Invalid("publication generation"));
    }
    source
        .validate_contract()
        .map_err(|_| ProjectionCommitmentError::Invalid("source identity"))?;
    event_id
        .validate_contract()
        .map_err(|_| ProjectionCommitmentError::Invalid("event identity"))?;
    if event_id.source_digest() != source.identity().digest()
        || event_id.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return Err(ProjectionCommitmentError::Invalid("event source identity"));
    }
    let core_record_sha256 = decode_sha256(core_record_sha256, "Core record digest")?;
    let core_record_leaf_sha256 =
        decode_sha256(core_record_leaf_sha256, "Core record leaf digest")?;
    let source_bytes = serde_json::to_vec(source)?;
    let source_len = u64::try_from(source_bytes.len())
        .map_err(|_| ProjectionCommitmentError::Invalid("source identity length"))?;
    let canonical_event = event_id
        .encode_canonical()
        .map_err(|_| ProjectionCommitmentError::Invalid("event canonical identity"))?;

    let records = records.into_iter().collect::<Vec<_>>();
    let flat_record_count =
        u32::try_from(records.len()).map_err(|_| ProjectionCommitmentError::Count)?;
    let mut hasher = Sha256::new();
    hasher.update(EVENT_OUTPUT_ROOT_DOMAIN);
    hasher.update(publication_generation.to_be_bytes());
    hasher.update(source_len.to_be_bytes());
    hasher.update(&source_bytes);
    hasher.update(canonical_event);
    hasher.update(event_sequence.to_be_bytes());
    hasher.update(core_record_sha256);
    hasher.update(core_record_leaf_sha256);
    hasher.update(flat_record_count.to_be_bytes());

    let mut prior_record_id: Option<&str> = None;
    for (record_id, record_sha256) in records {
        validate_record_id(record_id)?;
        if prior_record_id.is_some_and(|prior| prior.as_bytes() >= record_id.as_bytes()) {
            return Err(ProjectionCommitmentError::Invalid(
                "Flat record ordering or duplicate",
            ));
        }
        let record_id_len = u64::try_from(record_id.len())
            .map_err(|_| ProjectionCommitmentError::Invalid("Flat record identity length"))?;
        hasher.update(record_id_len.to_be_bytes());
        hasher.update(record_id.as_bytes());
        hasher.update(record_sha256);
        prior_record_id = Some(record_id);
    }
    Ok((flat_record_count, format!("{:x}", hasher.finalize())))
}

fn validate_record_id(record_id: &str) -> Result<(), ProjectionCommitmentError> {
    if record_id.is_empty()
        || record_id.len() > MAX_PROJECTION_RECORD_ID_BYTES
        || record_id.bytes().any(|byte| byte.is_ascii_control())
    {
        Err(ProjectionCommitmentError::Invalid("Flat record identity"))
    } else {
        Ok(())
    }
}

fn decode_sha256(value: &str, label: &'static str) -> Result<[u8; 32], ProjectionCommitmentError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProjectionCommitmentError::Invalid(label));
    }
    let decoded = hex::decode(value).map_err(|_| ProjectionCommitmentError::Invalid(label))?;
    decoded
        .try_into()
        .map_err(|_| ProjectionCommitmentError::Invalid(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_core::{IDENTITY_VERSION, SourceAnchor, StableEntityKind};

    fn source() -> SourceKey {
        SourceKey::derive(
            "fixture",
            "fixture_jsonl",
            "fixture-v1",
            1,
            SourceAnchor::CatalogLineage([3; 32]),
        )
        .unwrap()
    }

    fn event(source: &SourceKey, digest: [u8; 32]) -> StableEntityId {
        let mut uuid_bytes = [0_u8; 16];
        uuid_bytes.copy_from_slice(&digest[..16]);
        uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
        uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
        serde_json::from_value(serde_json::json!({
            "contract_version": IDENTITY_VERSION,
            "entity_kind": StableEntityKind::Event,
            "digest": digest,
            "source_digest": source.identity().digest(),
            "source_descriptor_digest": source.exact_descriptor_digest(),
            "uuid": uuid::Uuid::from_bytes(uuid_bytes),
        }))
        .unwrap()
    }

    #[test]
    fn empty_and_ordered_output_roots_are_deterministic() {
        let source = source();
        let event = event(&source, [7; 32]);
        let empty = event_output_root(
            9,
            &source,
            event,
            12,
            &"11".repeat(32),
            &"22".repeat(32),
            std::iter::empty(),
        )
        .unwrap();
        let records = [("a", [0x33; 32]), ("b", [0x44; 32])];
        let populated = event_output_root(
            9,
            &source,
            event,
            12,
            &"11".repeat(32),
            &"22".repeat(32),
            records,
        )
        .unwrap();
        assert_eq!(empty.0, 0);
        assert_eq!(populated.0, 2);
        assert_ne!(empty.1, populated.1);
    }

    #[test]
    fn duplicate_and_oversized_record_ids_fail() {
        let source = source();
        let event = event(&source, [8; 32]);
        let duplicate = [("same", [1; 32]), ("same", [2; 32])];
        assert!(
            event_output_root(
                1,
                &source,
                event,
                1,
                &"11".repeat(32),
                &"22".repeat(32),
                duplicate,
            )
            .is_err()
        );
        let oversized = "x".repeat(MAX_PROJECTION_RECORD_ID_BYTES + 1);
        assert!(
            event_output_root(
                1,
                &source,
                event,
                1,
                &"11".repeat(32),
                &"22".repeat(32),
                [(oversized.as_str(), [1; 32])],
            )
            .is_err()
        );
    }
}

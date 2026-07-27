use rusqlite::Row;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::projection::strip_local_complete_content_metadata;
use super::{CanonicalByteRange, CanonicalObservation};

pub(crate) fn canonical_semantic_digest(
    observation: &CanonicalObservation,
) -> serde_json::Result<String> {
    let mut semantic = observation.clone();
    semantic.semantic_digest.clear();
    if let Some(source) = &mut semantic.source {
        source.imported_observation = None;
        source.permitted_bytes = None;
    }
    strip_local_complete_content_metadata(&mut semantic.metadata);
    semantic.citation.source_sha256 = None;
    let encoded = serde_json::to_vec(&semantic)?;
    let mut hash = Sha256::new();
    hash.update(b"ctx-canonical-observation-semantic-v1\0");
    hash.update((encoded.len() as u64).to_be_bytes());
    hash.update(encoded);
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn parse_uuid_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value: String = row.get(index)?;
    Uuid::parse_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

pub(super) fn optional_uuid_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            Uuid::parse_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

pub(super) fn parse_json_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

pub(super) fn nonnegative_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

pub(super) fn json_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for component in path {
        current = current.get(*component)?;
    }
    current.as_u64()
}

pub(super) fn json_byte_range(metadata: &Value) -> Option<CanonicalByteRange> {
    let start = json_u64(metadata, &["byte_start"])
        .or_else(|| json_u64(metadata, &["metadata", "byte_start"]))?;
    let end_exclusive = json_u64(metadata, &["byte_end"])
        .or_else(|| json_u64(metadata, &["metadata", "byte_end"]))?;
    (end_exclusive >= start).then_some(CanonicalByteRange {
        start,
        end_exclusive,
    })
}

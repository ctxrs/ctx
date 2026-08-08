use std::io::{self, Write};

use ctx_history_core::{CoreRecord, CoreRecordError};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    BeginCoreMaterializationRequest, CoreSourceDelta, CoreSourceState,
    MAX_CORE_MATERIALIZER_REVISION_BYTES, MAX_CORE_SOURCE_STATES,
};
use crate::{ErrorClass, ProtocolError};

pub fn core_source_snapshot_sha256(sources: &[CoreSourceState]) -> Result<String, ProtocolError> {
    validate_source_states(sources)?;
    canonical_sha256(sources, "Core source snapshot encoding failed")
}

pub fn core_materialization_id(
    request: &BeginCoreMaterializationRequest,
    materializer_revision: &str,
) -> Result<String, ProtocolError> {
    request
        .acknowledgement_identity()?
        .materialization_id(materializer_revision)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreRecordDigests {
    pub core_record_sha256: String,
    pub core_record_leaf_sha256: String,
}

pub fn core_record_sha256(record: &CoreRecord) -> Result<String, ProtocolError> {
    let encoded = encode_core_record(record)?;
    Ok(core_record_sha256_from_encoded(&encoded))
}

/// Computes only the canonical record-state SHA from exact validated stored
/// Core JSON, without re-encoding the record or computing its frozen leaf.
pub fn core_record_sha256_from_encoded(encoded: &[u8]) -> String {
    hex_sha256(Sha256::digest(encoded))
}

/// Returns the frozen Core-record leaf while the exact `CoreRecord` is still
/// available at the Added/Replaced projection boundary.
pub fn core_record_leaf_sha256(record: &CoreRecord) -> Result<String, ProtocolError> {
    let encoded = encode_core_record(record)?;
    Ok(hex_sha256(core_record_leaf_digest(record, &encoded)?))
}

/// Computes the canonical record-state SHA and frozen Core leaf from one exact
/// `record.encode_stored()` traversal.
pub fn core_record_digests(record: &CoreRecord) -> Result<CoreRecordDigests, ProtocolError> {
    let encoded = encode_core_record(record)?;
    core_record_digests_from_encoded(record, &encoded)
}

/// Computes both protocol digests from exact validated stored Core JSON.
///
/// The caller must retain the decoded record produced from `encoded`. This is
/// the bounded Core-page seam used to avoid re-serializing a record that the
/// pinned Core generation already authenticated and decoded.
pub fn core_record_digests_from_encoded(
    record: &CoreRecord,
    encoded: &[u8],
) -> Result<CoreRecordDigests, ProtocolError> {
    Ok(CoreRecordDigests {
        core_record_sha256: core_record_sha256_from_encoded(encoded),
        core_record_leaf_sha256: hex_sha256(core_record_leaf_digest(record, encoded)?),
    })
}

fn encode_core_record(record: &CoreRecord) -> Result<Vec<u8>, ProtocolError> {
    record
        .encode_stored()
        .map_err(|error| invalid_contract("Core record", error))
}

fn core_record_leaf_digest(record: &CoreRecord, encoded: &[u8]) -> Result<[u8; 32], ProtocolError> {
    ctx_history_core::core_record_leaf_digest(record.event_id, encoded)
        .map_err(|error| invalid_contract("Core record leaf", error))
}

pub(super) fn validate_source_states(sources: &[CoreSourceState]) -> Result<(), ProtocolError> {
    if sources.len() > MAX_CORE_SOURCE_STATES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "Core source snapshot exceeds its source count bound",
        ));
    }
    let mut prior = None;
    for source in sources {
        source.validate()?;
        let current = source.source.identity().digest();
        if prior.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core source snapshot must be strictly ordered by stable source identity",
            ));
        }
        prior = Some(current);
    }
    Ok(())
}

fn core_source_state_exact_eq(left: &CoreSourceState, right: &CoreSourceState) -> bool {
    left.source.exact_descriptor_eq(&right.source)
        && left.core_record_accumulator == right.core_record_accumulator
        && left.event_count == right.event_count
}

pub(super) fn core_source_delta_exact_eq(left: &CoreSourceDelta, right: &CoreSourceDelta) -> bool {
    match (left, right) {
        (CoreSourceDelta::Present(left), CoreSourceDelta::Present(right)) => {
            core_source_state_exact_eq(left, right)
        }
        (CoreSourceDelta::Removed(left), CoreSourceDelta::Removed(right)) => {
            left.source.exact_descriptor_eq(&right.source)
        }
        _ => false,
    }
}

pub(super) fn core_record_content_bytes(record: &CoreRecord) -> Result<usize, ProtocolError> {
    record
        .content
        .encoded_content_bytes()
        .map_err(|error| match error {
            CoreRecordError::EncodedLengthOverflow => {
                ProtocolError::new(ErrorClass::Bounds, "Core record content bytes overflowed")
            }
            _ => ProtocolError::new(ErrorClass::Internal, "Core content byte accounting failed"),
        })
}

pub(super) fn validate_identity(value: &str, label: &'static str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_CORE_MATERIALIZER_REVISION_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{label} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_sha256(value: &str, label: &'static str) -> Result<(), ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{label} must be lowercase SHA-256"),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct CountingWriter {
    encoded_bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.encoded_bytes = self
            .encoded_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("encoded byte count overflowed"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn compact_json_encoded_len<T: Serialize + ?Sized>(
    value: &T,
) -> serde_json::Result<usize> {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.encoded_bytes)
}

pub(crate) fn validate_encoded_bound<T: Serialize + ?Sized>(
    value: &T,
    maximum: usize,
    message: &'static str,
) -> Result<(), ProtocolError> {
    if encoded_len(value)? > maximum {
        return Err(ProtocolError::new(ErrorClass::Bounds, message));
    }
    Ok(())
}

pub(crate) fn encoded_len<T: Serialize + ?Sized>(value: &T) -> Result<usize, ProtocolError> {
    compact_json_encoded_len(value)
        .map_err(|_| ProtocolError::new(ErrorClass::Internal, "protocol encoding failed"))
}

pub(super) fn encode_with_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    message: &'static str,
) -> Result<Vec<u8>, ProtocolError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| ProtocolError::new(ErrorClass::Internal, "protocol encoding failed"))?;
    if encoded.len() > maximum {
        return Err(ProtocolError::new(ErrorClass::Bounds, message));
    }
    Ok(encoded)
}

pub(super) fn canonical_sha256<T: Serialize + ?Sized>(
    value: &T,
    message: &'static str,
) -> Result<String, ProtocolError> {
    let encoded =
        serde_json::to_vec(value).map_err(|_| ProtocolError::new(ErrorClass::Internal, message))?;
    Ok(hex_sha256(Sha256::digest(encoded)))
}

pub(super) fn hex_sha256(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let digest = digest.as_ref();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

pub(super) fn invalid_contract(
    label: &'static str,
    error: impl std::fmt::Display,
) -> ProtocolError {
    ProtocolError::new(ErrorClass::InvalidRequest, format!("{label}: {error}"))
}

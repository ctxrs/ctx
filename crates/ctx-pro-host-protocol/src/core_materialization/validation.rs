use std::io::{self, Write};

use ctx_history_core::CoreRecord;
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

pub fn core_record_sha256(record: &CoreRecord) -> Result<String, ProtocolError> {
    record
        .validate_contract()
        .map_err(|error| invalid_contract("Core record", error))?;
    canonical_sha256(record, "Core record state encoding failed")
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
    let body = record
        .content
        .normalized_body
        .as_ref()
        .map_or(0, String::len);
    let structured = record
        .content
        .structured_content
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Core structured content encoding failed",
            )
        })?
        .map_or(0, |encoded| encoded.len());
    body.checked_add(structured).ok_or_else(|| {
        ProtocolError::new(ErrorClass::Bounds, "Core record content bytes overflowed")
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

pub(super) fn validate_sha256(value: &str, label: &'static str) -> Result<(), ProtocolError> {
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

pub(super) fn validate_encoded_bound<T: Serialize>(
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
    #[derive(Default)]
    struct Counter(usize);

    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("protocol encoded length overflowed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| ProtocolError::new(ErrorClass::Internal, "protocol encoding failed"))?;
    Ok(counter.0)
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
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn invalid_contract(
    label: &'static str,
    error: impl std::fmt::Display,
) -> ProtocolError {
    ProtocolError::new(ErrorClass::InvalidRequest, format!("{label}: {error}"))
}

use ctx_attribution_model::{ErrorClass, ProtocolError};
use ctx_history_core::{CoreRecord, CoreRecordError};
use serde::Serialize;
use std::io::{self, Write};
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

pub(super) fn invalid_contract(
    label: &'static str,
    error: impl std::fmt::Display,
) -> ProtocolError {
    ProtocolError::new(ErrorClass::InvalidRequest, format!("{label}: {error}"))
}

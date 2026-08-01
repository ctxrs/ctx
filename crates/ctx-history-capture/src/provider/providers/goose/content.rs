use crate::{
    native_source::NativeSqliteValue, record_evidence::sqlite_logical_record_digest, CaptureError,
    Result,
};

use super::schema;

pub(super) fn goose_logical_row_digest(values: &[NativeSqliteValue]) -> Result<[u8; 32]> {
    let digest = sqlite_logical_record_digest(values);
    let bytes = digest.as_str().as_bytes();
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        decoded[index] = decode_hex_nibble(pair[0])
            .and_then(|high| decode_hex_nibble(pair[1]).map(|low| (high << 4) | low))
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Goose logical-row evidence has an invalid digest".to_owned(),
                )
            })?;
    }
    Ok(decoded)
}

/// Stable message evidence excludes the parent and message SQLite rowids.
///
/// Goose requires `messages.id` to be an INTEGER PRIMARY KEY, so the native
/// message id remains in the digest while incidental parent-table rowids do
/// not turn a VACUUM into a logical replacement.
pub(super) fn goose_message_record_digest(values: &[NativeSqliteValue]) -> Result<[u8; 32]> {
    if values.len() != schema::GOOSE_MESSAGE_VALUE_COUNT + 1 {
        return Err(CaptureError::InvalidPayload(
            "Goose message evidence has an unexpected value count".to_owned(),
        ));
    }
    goose_logical_row_digest(&values[2..])
}

fn decode_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

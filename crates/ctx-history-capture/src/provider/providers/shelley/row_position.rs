use crate::captured_batch::{NativeLocator, NativePosition};
use crate::{CaptureError, Result};

use super::{SHELLEY_LOCATOR_KIND, SHELLEY_POSITION_BYTES, SHELLEY_POSITION_KIND};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShelleyCapturePhase {
    MessageKeyClassification,
    Messages,
    Conversations,
}

impl ShelleyCapturePhase {
    fn tag(self) -> u8 {
        match self {
            Self::MessageKeyClassification => 1,
            Self::Messages => 2,
            Self::Conversations => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::MessageKeyClassification),
            2 => Ok(Self::Messages),
            3 => Ok(Self::Conversations),
            _ => Err(CaptureError::InvalidPayload(
                "Shelley cursor has an unknown capture phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ShelleyKeyset {
    pub(super) phase: ShelleyCapturePhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
    pub(super) exhausted: bool,
    pub(super) pending_oversize_session: bool,
    pub(super) classification_has_valid_message: bool,
    pub(super) classification_all_keys_valid: bool,
}

pub(super) fn initial_shelley_position() -> Result<NativePosition> {
    NativePosition::new(SHELLEY_POSITION_KIND, vec![0]).map_err(shelley_position_error)
}

pub(super) fn encode_shelley_position(keyset: ShelleyKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(SHELLEY_POSITION_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&shelley_ordered_i64(keyset.rowid).to_be_bytes());
    value.push(u8::from(keyset.exhausted));
    value.push(u8::from(keyset.pending_oversize_session));
    value.push(u8::from(keyset.classification_has_valid_message));
    value.push(u8::from(keyset.classification_all_keys_valid));
    NativePosition::new(SHELLEY_POSITION_KIND, value).map_err(shelley_position_error)
}

pub(super) fn decode_shelley_position(position: &NativePosition) -> Result<Option<ShelleyKeyset>> {
    if position.kind() != SHELLEY_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Shelley cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != SHELLEY_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Shelley cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(ShelleyKeyset {
        phase: ShelleyCapturePhase::from_tag(position.value()[0])?,
        next_ordinal: shelley_decode_u64(&position.value()[1..9])?,
        rowid: shelley_unordered_i64(shelley_decode_u64(&position.value()[9..17])?),
        exhausted: shelley_decode_flag(position.value()[17], "exhaustion")?,
        pending_oversize_session: shelley_decode_flag(
            position.value()[18],
            "pending-oversize-session",
        )?,
        classification_has_valid_message: shelley_decode_flag(
            position.value()[19],
            "classification-has-valid-message",
        )?,
        classification_all_keys_valid: shelley_decode_flag(
            position.value()[20],
            "classification-all-keys-valid",
        )?,
    }))
}

pub(super) fn shelley_oversize_session_locator(rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(3);
    value.extend_from_slice(&shelley_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(SHELLEY_LOCATOR_KIND, value).map_err(shelley_position_error)
}

pub(super) fn shelley_locator(phase: ShelleyCapturePhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(phase.tag());
    value.extend_from_slice(&shelley_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(SHELLEY_LOCATOR_KIND, value).map_err(shelley_position_error)
}

fn shelley_decode_flag(value: u8, label: &str) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley cursor has an invalid {label} flag"
        ))),
    }
}

fn shelley_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Shelley cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn shelley_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn shelley_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn shelley_position_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

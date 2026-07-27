use crate::captured_batch::{NativeLocator, NativePosition};
use crate::{CaptureError, Result};

const WARP_POSITION_KIND: &str = "warp-conversation-task-keyset-v4";
const WARP_LOCATOR_KIND: &str = "warp-conversation-task-row-v2";
pub(super) const WARP_CONTENT_LOCATOR_KIND: &str = "warp-task-message-v1";
const WARP_POSITION_BYTES: usize = 1 + 8 + 1 + 1 + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WarpPhase {
    Conversations,
    Tasks,
}

impl WarpPhase {
    fn tag(self) -> u8 {
        match self {
            Self::Conversations => 1,
            Self::Tasks => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Conversations),
            2 => Ok(Self::Tasks),
            _ => Err(CaptureError::InvalidPayload(
                "Warp cursor has an unknown phase".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct WarpKeyset {
    pub(super) phase: WarpPhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
    pub(super) key_valid: bool,
}

pub(super) fn initial_warp_position() -> Result<NativePosition> {
    NativePosition::new(WARP_POSITION_KIND, vec![0]).map_err(warp_captured_error)
}

pub(super) fn encode_warp_position(keyset: WarpKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(WARP_POSITION_BYTES);
    value.push(4);
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.push(keyset.phase.tag());
    value.push(u8::from(keyset.key_valid));
    value.extend_from_slice(&warp_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(WARP_POSITION_KIND, value).map_err(warp_captured_error)
}

pub(super) fn decode_warp_position(position: &NativePosition) -> Result<Option<WarpKeyset>> {
    if position.kind() != WARP_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Warp cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != WARP_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Warp cursor has an invalid native-position payload".to_owned(),
        ));
    }
    if position.value()[0] != 4 || position.value()[10] > 1 {
        return Err(CaptureError::InvalidPayload(
            "Warp cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(WarpKeyset {
        phase: WarpPhase::from_tag(position.value()[9])?,
        next_ordinal: warp_decode_u64(&position.value()[1..9])?,
        key_valid: position.value()[10] == 1,
        rowid: warp_unordered_i64(warp_decode_u64(&position.value()[11..19])?),
    }))
}

fn warp_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Warp cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn warp_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn warp_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

pub(super) fn warp_locator(phase: WarpPhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(phase.tag());
    value.extend_from_slice(&warp_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(WARP_LOCATOR_KIND, value).map_err(warp_captured_error)
}

pub(super) fn warp_content_locator(rowid: i64, message_index: u32) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(12);
    value.extend_from_slice(&rowid.to_be_bytes());
    value.extend_from_slice(&message_index.to_be_bytes());
    NativeLocator::new(WARP_CONTENT_LOCATOR_KIND, value).map_err(warp_captured_error)
}

fn warp_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
#[path = "position_tests.rs"]
mod tests;

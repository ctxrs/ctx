use crate::captured_batch::{NativeLocator, NativePosition};
use crate::{CaptureError, Result};

const GOOSE_POSITION_KIND: &str = "goose-logical-row-keyset-v3";
const GOOSE_LOCATOR_KIND: &str = "goose-logical-row-v3";
const GOOSE_POSITION_BYTES: usize = 1 + 8 + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GooseCapturePhase {
    Sessions,
    Messages,
}

impl GooseCapturePhase {
    fn tag(self) -> u8 {
        match self {
            Self::Sessions => 1,
            Self::Messages => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Sessions),
            2 => Ok(Self::Messages),
            _ => Err(CaptureError::InvalidPayload(
                "Goose cursor has an unknown capture phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct GooseKeyset {
    pub(super) phase: GooseCapturePhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
}
pub(super) fn initial_goose_position() -> Result<NativePosition> {
    NativePosition::new(GOOSE_POSITION_KIND, vec![0])
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn encode_goose_position(keyset: GooseKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(GOOSE_POSITION_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&goose_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(GOOSE_POSITION_KIND, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn decode_goose_position(position: &NativePosition) -> Result<Option<GooseKeyset>> {
    if position.kind() != GOOSE_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Goose cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != GOOSE_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Goose cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(GooseKeyset {
        phase: GooseCapturePhase::from_tag(position.value()[0])?,
        next_ordinal: goose_decode_u64(&position.value()[1..9])?,
        rowid: goose_unordered_i64(goose_decode_u64(&position.value()[9..17])?),
    }))
}

pub(super) fn goose_locator(phase: GooseCapturePhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(phase.tag());
    value.extend_from_slice(&goose_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(GOOSE_LOCATOR_KIND, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn goose_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Goose cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn goose_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn goose_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

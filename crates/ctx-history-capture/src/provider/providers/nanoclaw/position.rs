use crate::captured_batch::{NativeLocator, NativePosition};
use crate::{CaptureError, Result};

use super::{nanoclaw_captured_error, NANOCLAW_LOCATOR_KIND, NANOCLAW_POSITION_KIND};

const NANOCLAW_POSITION_BYTES: usize = 1 + 8 + 1 + 8 + 1 + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NanoClawPositionPhase {
    NextSession,
    Messages,
}

impl NanoClawPositionPhase {
    fn tag(self) -> u8 {
        match self {
            Self::NextSession => 1,
            Self::Messages => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::NextSession),
            2 => Ok(Self::Messages),
            _ => Err(CaptureError::InvalidPayload(
                "NanoClaw cursor has an unknown phase".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum NanoClawMessageSource {
    Inbound,
    Outbound,
}

impl NanoClawMessageSource {
    pub(super) fn tag(self) -> u8 {
        match self {
            Self::Inbound => 1,
            Self::Outbound => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Option<Self>> {
        match tag {
            0 => Ok(None),
            1 => Ok(Some(Self::Inbound)),
            2 => Ok(Some(Self::Outbound)),
            _ => Err(CaptureError::InvalidPayload(
                "NanoClaw cursor has an unknown message source".to_owned(),
            )),
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }

    pub(super) fn table(self) -> &'static str {
        match self {
            Self::Inbound => "messages_in",
            Self::Outbound => "messages_out",
        }
    }

    pub(super) fn file_name(self) -> &'static str {
        match self {
            Self::Inbound => "inbound.db",
            Self::Outbound => "outbound.db",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct NanoClawKeyset {
    pub(super) next_ordinal: u64,
    pub(super) phase: NanoClawPositionPhase,
    pub(super) session_rowid: i64,
    pub(super) message_source: Option<NanoClawMessageSource>,
    pub(super) message_rowid: i64,
}

pub(super) fn initial_nanoclaw_position() -> Result<NativePosition> {
    NativePosition::new(NANOCLAW_POSITION_KIND, vec![0]).map_err(nanoclaw_captured_error)
}

pub(super) fn encode_nanoclaw_position(keyset: NanoClawKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(NANOCLAW_POSITION_BYTES);
    value.push(1);
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.push(keyset.phase.tag());
    value.extend_from_slice(&nanoclaw_ordered_i64(keyset.session_rowid).to_be_bytes());
    value.push(keyset.message_source.map_or(0, NanoClawMessageSource::tag));
    value.extend_from_slice(&nanoclaw_ordered_i64(keyset.message_rowid).to_be_bytes());
    NativePosition::new(NANOCLAW_POSITION_KIND, value).map_err(nanoclaw_captured_error)
}

pub(super) fn decode_nanoclaw_position(
    position: &NativePosition,
) -> Result<Option<NanoClawKeyset>> {
    if position.kind() != NANOCLAW_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != NANOCLAW_POSITION_BYTES || position.value()[0] != 1 {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw cursor has an invalid native-position payload".to_owned(),
        ));
    }
    let keyset = NanoClawKeyset {
        next_ordinal: nanoclaw_decode_u64(&position.value()[1..9])?,
        phase: NanoClawPositionPhase::from_tag(position.value()[9])?,
        session_rowid: nanoclaw_unordered_i64(nanoclaw_decode_u64(&position.value()[10..18])?),
        message_source: NanoClawMessageSource::from_tag(position.value()[18])?,
        message_rowid: nanoclaw_unordered_i64(nanoclaw_decode_u64(&position.value()[19..27])?),
    };
    if keyset.session_rowid <= 0
        || (keyset.phase == NanoClawPositionPhase::NextSession
            && (keyset.message_source.is_some() || keyset.message_rowid != 0))
        || (keyset.message_source.is_none() && keyset.message_rowid != 0)
    {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw cursor contains an invalid keyset".to_owned(),
        ));
    }
    Ok(Some(keyset))
}

pub(super) fn nanoclaw_locator(
    source: Option<NanoClawMessageSource>,
    rowid: i64,
) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(1 + 8);
    value.push(source.map_or(0, NanoClawMessageSource::tag));
    value.extend_from_slice(&nanoclaw_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(NANOCLAW_LOCATOR_KIND, value).map_err(nanoclaw_captured_error)
}

fn nanoclaw_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("NanoClaw cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn nanoclaw_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn nanoclaw_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

pub(super) fn nanoclaw_next_ordinal(ordinal: u64) -> Result<u64> {
    ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "NanoClaw captured row ordinal overflowed",
    ))
}

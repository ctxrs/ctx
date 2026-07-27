//! Durable keyset position and certified-cursor encoding.

use crate::captured_batch::{NativePosition, SourceObservation};
use crate::provider::importer::{BoundedParserCheckpoint, CertifiedProviderCursor};
use crate::{CaptureError, Result};

use super::{deepagents_captured_error, DEEPAGENTS_POSITION_KIND};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeepAgentsPhase {
    Writes,
    Threads,
}

impl DeepAgentsPhase {
    fn tag(self) -> u8 {
        match self {
            Self::Threads => 1,
            Self::Writes => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Threads),
            2 => Ok(Self::Writes),
            _ => Err(CaptureError::InvalidPayload(
                "Deep Agents cursor has an unknown phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DeepAgentsPositionKey {
    Write { rowid: i64, next_event_index: u64 },
    Thread { rowid: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeepAgentsPosition {
    pub(super) next_ordinal: u64,
    pub(super) key: DeepAgentsPositionKey,
}

pub(super) fn deepagents_cursor_candidate(
    source: &SourceObservation,
    position: &NativePosition,
) -> Result<CertifiedProviderCursor> {
    CertifiedProviderCursor::new(
        source.source_revision(),
        source.capture_revision(),
        source.policy_revision(),
        position.clone(),
        BoundedParserCheckpoint::from_serializable(&())?,
    )
}

pub(super) fn initial_deepagents_position() -> Result<NativePosition> {
    NativePosition::new(DEEPAGENTS_POSITION_KIND, vec![0]).map_err(deepagents_captured_error)
}

pub(super) fn encode_deepagents_position(position: DeepAgentsPosition) -> Result<NativePosition> {
    let mut bytes = Vec::new();
    bytes.push(2);
    bytes.extend_from_slice(&position.next_ordinal.to_be_bytes());
    match position.key {
        DeepAgentsPositionKey::Write {
            rowid,
            next_event_index,
        } => {
            bytes.push(DeepAgentsPhase::Writes.tag());
            bytes.extend_from_slice(&deepagents_ordered_i64(rowid).to_be_bytes());
            bytes.extend_from_slice(&next_event_index.to_be_bytes());
        }
        DeepAgentsPositionKey::Thread { rowid } => {
            bytes.push(DeepAgentsPhase::Threads.tag());
            bytes.extend_from_slice(&deepagents_ordered_i64(rowid).to_be_bytes());
        }
    }
    NativePosition::new(DEEPAGENTS_POSITION_KIND, bytes).map_err(deepagents_captured_error)
}

pub(super) fn decode_deepagents_position(
    position: &NativePosition,
) -> Result<Option<DeepAgentsPosition>> {
    if position.kind() != DEEPAGENTS_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() < 10 || position.value()[0] != 2 {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents cursor has an invalid native-position payload".to_owned(),
        ));
    }
    let next_ordinal = deepagents_decode_u64(&position.value()[1..9])?;
    let phase = DeepAgentsPhase::from_tag(position.value()[9])?;
    let mut offset = 10_usize;
    let key = match phase {
        DeepAgentsPhase::Writes => {
            let rowid_end = offset.checked_add(8).ok_or(CaptureError::InvalidPayload(
                "Deep Agents cursor offset overflowed".to_owned(),
            ))?;
            let rowid = deepagents_unordered_i64(deepagents_decode_u64(
                position.value().get(offset..rowid_end).ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Deep Agents cursor write rowid is truncated".to_owned(),
                    )
                })?,
            )?);
            let event_index_end = rowid_end
                .checked_add(8)
                .ok_or(CaptureError::InvalidPayload(
                    "Deep Agents cursor offset overflowed".to_owned(),
                ))?;
            let next_event_index = deepagents_decode_u64(
                position
                    .value()
                    .get(rowid_end..event_index_end)
                    .ok_or_else(|| {
                        CaptureError::InvalidPayload(
                            "Deep Agents cursor event index is truncated".to_owned(),
                        )
                    })?,
            )?;
            offset = event_index_end;
            DeepAgentsPositionKey::Write {
                rowid,
                next_event_index,
            }
        }
        DeepAgentsPhase::Threads => {
            let rowid_end = offset.checked_add(8).ok_or(CaptureError::InvalidPayload(
                "Deep Agents cursor offset overflowed".to_owned(),
            ))?;
            let rowid = deepagents_unordered_i64(deepagents_decode_u64(
                position.value().get(offset..rowid_end).ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Deep Agents cursor thread rowid is truncated".to_owned(),
                    )
                })?,
            )?);
            offset = rowid_end;
            DeepAgentsPositionKey::Thread { rowid }
        }
    };
    if offset != position.value().len() {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents cursor has trailing native-position bytes".to_owned(),
        ));
    }
    Ok(Some(DeepAgentsPosition { next_ordinal, key }))
}

pub(super) fn deepagents_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Deep Agents cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn deepagents_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

pub(super) fn deepagents_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

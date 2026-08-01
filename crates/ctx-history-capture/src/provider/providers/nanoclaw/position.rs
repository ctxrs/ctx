use serde::{Deserialize, Serialize};

use crate::{CaptureError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum NanoClawPositionPhase {
    NextSession,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NanoClawMessageSource {
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

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }

    pub(crate) fn table(self) -> &'static str {
        match self {
            Self::Inbound => "messages_in",
            Self::Outbound => "messages_out",
        }
    }

    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Self::Inbound => "inbound.db",
            Self::Outbound => "outbound.db",
        }
    }
}

/// Provider-owned safe boundary. It identifies the next logical row after a
/// committed page and is independent from both the Store cursor envelope and
/// the output sink cursor envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NanoClawFrontier {
    pub(super) next_ordinal: u64,
    pub(super) phase: NanoClawPositionPhase,
    pub(super) session_rowid: i64,
    pub(super) message_source: Option<NanoClawMessageSource>,
    pub(super) message_rowid: i64,
}

impl NanoClawFrontier {
    pub(super) const fn initial() -> Self {
        Self {
            next_ordinal: 0,
            phase: NanoClawPositionPhase::NextSession,
            session_rowid: 0,
            message_source: None,
            message_rowid: 0,
        }
    }
}

pub(super) fn nanoclaw_next_ordinal(ordinal: u64) -> Result<u64> {
    ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "NanoClaw NativePath row ordinal overflowed",
    ))
}

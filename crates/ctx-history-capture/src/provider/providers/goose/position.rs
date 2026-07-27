use serde::{Deserialize, Serialize};

pub(super) fn goose_message_locator(rowid: i64) -> (&'static str, Vec<u8>) {
    let mut value = Vec::with_capacity(9);
    value.push(2);
    value.extend_from_slice(&goose_ordered_i64(rowid).to_be_bytes());
    ("goose-logical-row-v3", value)
}

fn goose_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GooseNativeScanPhase {
    Sessions,
    Messages,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "rowid", rename_all = "snake_case")]
pub(super) enum GooseNativeRowKeyset {
    Unstarted,
    After(i64),
}

impl GooseNativeRowKeyset {
    pub(super) fn sql_operator(self) -> &'static str {
        match self {
            Self::Unstarted => ">=",
            Self::After(_) => ">",
        }
    }

    pub(super) fn bound(self) -> i64 {
        match self {
            Self::Unstarted => i64::MIN,
            Self::After(rowid) => rowid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GooseNativeScanPosition {
    pub(super) phase: GooseNativeScanPhase,
    pub(super) keyset: GooseNativeRowKeyset,
    pub(super) native_rows_seen: u64,
}

impl GooseNativeScanPosition {
    pub(super) fn initial() -> Self {
        Self {
            phase: GooseNativeScanPhase::Sessions,
            keyset: GooseNativeRowKeyset::Unstarted,
            native_rows_seen: 0,
        }
    }

    pub(super) fn advance(self, rowid: i64) -> Self {
        Self {
            keyset: GooseNativeRowKeyset::After(rowid),
            ..self
        }
    }

    pub(super) fn start_messages(self) -> Self {
        Self {
            phase: GooseNativeScanPhase::Messages,
            keyset: GooseNativeRowKeyset::Unstarted,
            native_rows_seen: self.native_rows_seen,
        }
    }

    pub(super) fn complete(self) -> Self {
        Self {
            phase: GooseNativeScanPhase::Complete,
            ..self
        }
    }
}

use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{native_source::NativeSqliteValue, CaptureError, Result};

use super::{
    relationships::{
        decode_shelley_conversation, decode_shelley_message, shelley_stable_event_index,
        ShelleyConversationRow, ShelleyMessageRow,
    },
    source::{shelley_retained_length_expr, with_shelley_length_preflight},
};

mod scanner;
pub(crate) mod source_backed;

const SHELLEY_PREFIX_DOMAIN: &[u8] = b"ctx-shelley-nativepath-prefix-v1\0";
const SHELLEY_PAGE_MAX_UNITS: usize = 64;
const SHELLEY_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
const SHELLEY_ROW_MAX_BYTES: usize = 3 * 1024 * 1024;
const SHELLEY_PAGE_FIXED_OVERHEAD: usize = 64 * 1024;

#[derive(Debug)]
enum ShelleyUnit<T> {
    Accepted {
        rowid: i64,
        retained_bytes: usize,
        value: T,
    },
    Rejected {
        rowid: i64,
        retained_bytes: usize,
        reason: String,
    },
}

impl<T> ShelleyUnit<T> {
    fn rowid(&self) -> i64 {
        match self {
            Self::Accepted { rowid, .. } | Self::Rejected { rowid, .. } => *rowid,
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Accepted { retained_bytes, .. } | Self::Rejected { retained_bytes, .. } => {
                *retained_bytes
            }
        }
    }
}

#[derive(Debug)]
struct ShelleyMessage {
    message: ShelleyMessageRow,
    conversation: ShelleyConversationRow,
    parent_bearing: bool,
    provider_event_index: u64,
}

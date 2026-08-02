use rusqlite::types::ValueRef;
use sha2::{Digest, Sha256};

use super::hash_bytes;

#[derive(Debug)]
pub(super) enum SqliteSourceValue {
    Null,
    Integer(i64),
    Real(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl SqliteSourceValue {
    pub(super) fn from_ref(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(value) => Self::Integer(value),
            ValueRef::Real(value) => Self::Real(value.to_bits()),
            ValueRef::Text(value) => Self::Text(value.to_vec()),
            ValueRef::Blob(value) => Self::Blob(value.to_vec()),
        }
    }

    pub(super) fn hash_into(&self, hasher: &mut Sha256) {
        match self {
            Self::Null => hasher.update([0]),
            Self::Integer(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            Self::Real(bits) => {
                hasher.update([2]);
                hasher.update(bits.to_le_bytes());
            }
            Self::Text(value) => {
                hasher.update([3]);
                hash_bytes(hasher, value);
            }
            Self::Blob(value) => {
                hasher.update([4]);
                hash_bytes(hasher, value);
            }
        }
    }

    pub(super) fn exact_text(&self) -> Option<&[u8]> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

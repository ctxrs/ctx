use sha2::{Digest, Sha256};

use crate::{native_source::NativeSqliteValue, CaptureError, Result};

pub(super) struct ForgeCodeRecordEvidence {
    record_digest: [u8; 32],
    canonical_record_bytes: u64,
}

impl ForgeCodeRecordEvidence {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        rowid: i64,
        conversation_id: &str,
        title: Option<&str>,
        workspace_id: i64,
        context: Option<&str>,
        created_at: &str,
        updated_at: Option<&str>,
        metrics: Option<&str>,
    ) -> Result<Self> {
        let values = vec![
            NativeSqliteValue::Integer(rowid),
            NativeSqliteValue::Text(conversation_id.to_owned()),
            native_text(title),
            NativeSqliteValue::Integer(workspace_id),
            native_text(context),
            NativeSqliteValue::Text(created_at.to_owned()),
            native_text(updated_at),
            native_text(metrics),
        ];
        Ok(Self {
            record_digest: forgecode_logical_record_digest(&values),
            canonical_record_bytes: forgecode_logical_record_bytes(&values)?,
        })
    }

    pub(super) fn record_digest(&self) -> [u8; 32] {
        self.record_digest
    }

    pub(super) fn canonical_record_bytes(&self) -> u64 {
        self.canonical_record_bytes
    }
}

fn forgecode_logical_record_digest(values: &[NativeSqliteValue]) -> [u8; 32] {
    // Preserve the released logical-row evidence domain across the removal of
    // the resolver that originally introduced it.
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    // SQLite rowid is acquisition-only; logical evidence starts with the
    // provider-native conversation values.
    let values = values.get(1..).unwrap_or_default();
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

fn forgecode_logical_record_bytes(values: &[NativeSqliteValue]) -> Result<u64> {
    values.iter().try_fold(8_u64, |total, value| {
        let value_bytes = match value {
            NativeSqliteValue::Null => 1,
            NativeSqliteValue::Integer(_) | NativeSqliteValue::RealBits(_) => 9,
            NativeSqliteValue::Text(value) => canonical_variable_value_bytes(value.len())?,
            NativeSqliteValue::Blob(value) => canonical_variable_value_bytes(value.len())?,
        };
        total
            .checked_add(value_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "ForgeCode canonical logical-row length overflowed",
            ))
    })
}

fn canonical_variable_value_bytes(length: usize) -> Result<u64> {
    u64::try_from(length)
        .ok()
        .and_then(|length| length.checked_add(9))
        .ok_or(CaptureError::SystemInvariant(
            "ForgeCode canonical logical-row value length overflowed",
        ))
}

fn native_text(value: Option<&str>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, |value| {
        NativeSqliteValue::Text(value.to_owned())
    })
}

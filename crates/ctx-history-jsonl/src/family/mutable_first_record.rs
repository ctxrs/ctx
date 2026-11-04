use serde_json::Value;

/// Provider-declared metadata whose fixed-width first record is not conversation
/// content. Only this record is canonicalized; all following bytes remain exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlMutableFirstRecord {
    /// OMP's v1 title slot occupies exactly 256 UTF-8 bytes, including LF.
    OmpTitleSlotV1,
}

impl JsonlMutableFirstRecord {
    pub const WIRE_BYTES: usize = 256;
    pub(super) const MASK: [u8; Self::WIRE_BYTES] = [0; Self::WIRE_BYTES];

    pub(super) const fn checkpoint_tag(self) -> &'static str {
        match self {
            Self::OmpTitleSlotV1 => "omp-title-slot-256-v1",
        }
    }

    /// Validates the metadata schema without imposing the mutable wire width.
    /// Legacy title records of other widths remain parse-compatible, but cannot
    /// acquire the mutable-prefix exemption.
    pub fn validates_record(self, bytes: &[u8]) -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return false;
        };
        match self {
            Self::OmpTitleSlotV1 => {
                value.get("type").and_then(Value::as_str) == Some("title")
                    && value.get("v").and_then(Value::as_f64) == Some(1.0)
                    && value.get("title").is_some_and(Value::is_string)
                    && value.get("updatedAt").is_some_and(Value::is_string)
                    && value.get("pad").is_some_and(Value::is_string)
                    && match value.get("source") {
                        None => true,
                        Some(source) => matches!(source.as_str(), Some("auto" | "user")),
                    }
            }
        }
    }

    pub(super) fn validates_framing(self, record: &[u8], wire_bytes: u64) -> bool {
        wire_bytes == Self::WIRE_BYTES as u64
            && record.len() == Self::WIRE_BYTES - 1
            && !record.contains(&b'\n')
            && !record.contains(&b'\r')
            && self.validates_record(record)
    }

    pub(super) fn validates_wire(self, bytes: &[u8; Self::WIRE_BYTES]) -> bool {
        bytes[Self::WIRE_BYTES - 1] == b'\n'
            && self.validates_framing(&bytes[..Self::WIRE_BYTES - 1], bytes.len() as u64)
    }
}

use std::fmt;

/// Lossless provider-native SQLite value used for persisted logical evidence.
#[derive(Clone, PartialEq, Eq)]
pub enum NativeSqliteValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl NativeSqliteValue {
    pub fn from_real(value: f64) -> Self {
        Self::RealBits(value.to_bits())
    }

    pub fn as_real(&self) -> Option<f64> {
        match self {
            Self::RealBits(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }
}

impl fmt::Debug for NativeSqliteValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::Integer(_) => formatter.write_str("Integer(<redacted>)"),
            Self::RealBits(_) => formatter.write_str("RealBits(<redacted>)"),
            Self::Text(value) => formatter
                .debug_struct("Text")
                .field("bytes", &value.len())
                .finish(),
            Self::Blob(value) => formatter
                .debug_struct("Blob")
                .field("bytes", &value.len())
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_bits_round_trip_and_debug_redacts_values() {
        let value = NativeSqliteValue::from_real(-0.0);
        assert_eq!(
            value.as_real().map(f64::to_bits),
            Some((-0.0_f64).to_bits())
        );
        assert_eq!(format!("{value:?}"), "RealBits(<redacted>)");
        assert_eq!(
            format!("{:?}", NativeSqliteValue::Text("secret".to_owned())),
            "Text { bytes: 6 }"
        );
    }
}

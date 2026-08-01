use std::fmt;

#[derive(PartialEq, Eq)]
pub(crate) enum NativeSqliteValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl NativeSqliteValue {
    pub(crate) fn from_real(value: f64) -> Self {
        Self::RealBits(value.to_bits())
    }

    pub(crate) fn as_real(&self) -> Option<f64> {
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

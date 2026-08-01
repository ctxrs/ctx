use std::fmt;

use thiserror::Error;

const MAX_NATIVE_KIND_BYTES: usize = 256;
const MAX_NATIVE_LOCATOR_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeLocator {
    kind: String,
    value: Vec<u8>,
}

impl NativeLocator {
    pub(crate) fn new(kind: impl Into<String>, value: Vec<u8>) -> Result<Self, NativeSourceError> {
        let locator = Self {
            kind: kind.into(),
            value,
        };
        validate_text("locator_kind", &locator.kind, MAX_NATIVE_KIND_BYTES)?;
        validate_native_locator_value_len(locator.value.len())?;
        Ok(locator)
    }

    pub(crate) fn value(&self) -> &[u8] {
        &self.value
    }
}

impl fmt::Debug for NativeLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeLocator")
            .field("kind", &self.kind)
            .field("value_bytes", &self.value.len())
            .finish()
    }
}

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

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum NativeSourceError {
    #[error("native source field {field} is empty")]
    EmptyField { field: &'static str },
    #[error("native source field {field} is too large: {actual} bytes, maximum {maximum}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
}

pub(crate) fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), NativeSourceError> {
    if value.is_empty() {
        return Err(NativeSourceError::EmptyField { field });
    }
    validate_bytes(field, value.as_bytes(), maximum)
}

fn validate_bytes(
    field: &'static str,
    value: &[u8],
    maximum: usize,
) -> Result<(), NativeSourceError> {
    if value.len() > maximum {
        return Err(NativeSourceError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

pub(crate) fn validate_native_locator_value_len(actual: usize) -> Result<(), NativeSourceError> {
    if actual > MAX_NATIVE_LOCATOR_BYTES {
        return Err(NativeSourceError::FieldTooLarge {
            field: "locator_value",
            actual,
            maximum: MAX_NATIVE_LOCATOR_BYTES,
        });
    }
    Ok(())
}

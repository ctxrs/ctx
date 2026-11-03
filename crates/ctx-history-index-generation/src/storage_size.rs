use std::path::Path;

use serde::Serialize;

use crate::{GenerationError, Result};

/// Logical file bytes of the active segments and Tantivy commit metadata.
/// The docstore contains Core records; it is part of the total, not another store.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct GenerationStorageSize {
    pub docstore_bytes: u64,
    pub terms_bytes: u64,
    pub postings_bytes: u64,
    pub positions_bytes: u64,
    pub fast_fields_bytes: u64,
    /// Field norms, deletion bitsets, and Tantivy commit metadata.
    pub other_bytes: u64,
    pub total_bytes: u64,
}

impl GenerationStorageSize {
    pub(crate) fn from_artifacts<'a>(
        artifacts: impl Iterator<Item = (&'a str, u64)>,
    ) -> Result<Self> {
        let mut size = Self::default();
        for (path, bytes) in artifacts {
            let component = match Path::new(path).extension().and_then(|value| value.to_str()) {
                Some("store") => &mut size.docstore_bytes,
                Some("term") => &mut size.terms_bytes,
                Some("idx") => &mut size.postings_bytes,
                Some("pos") => &mut size.positions_bytes,
                Some("fast") => &mut size.fast_fields_bytes,
                _ => &mut size.other_bytes,
            };
            *component = component
                .checked_add(bytes)
                .ok_or(GenerationError::CountOverflow)?;
            size.total_bytes = size
                .total_bytes
                .checked_add(bytes)
                .ok_or(GenerationError::CountOverflow)?;
        }
        Ok(size)
    }
}

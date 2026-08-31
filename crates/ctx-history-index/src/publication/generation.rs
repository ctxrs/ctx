use std::path::Path;

use tantivy::Index;

use crate::{lexical_schema, Result};
use ctx_history_index_format::register_body_analyzer;

pub(crate) use ctx_history_index_generation::{
    lexical_index_settings, slot_path, ActiveGenerationPointer, GenerationSlot,
    PointerPublicationOutcome, INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use ctx_history_index_generation::{ReclamationStage, ReclamationTestHookGuard};

pub(crate) fn load_active_generation_pointer(
    root: &Path,
) -> Result<Option<ActiveGenerationPointer>> {
    Ok(ctx_history_index_generation::load_active_generation_pointer(root)?)
}

pub(crate) fn open_slot_index(root: &Path, slot: &GenerationSlot) -> Result<Index> {
    let index = ctx_history_index_generation::open_slot_index(root, slot)?;
    register_body_analyzer(&index);
    Ok(index)
}

pub(crate) fn create_candidate_generation(
    root: &Path,
    base: Option<&GenerationSlot>,
    writer_memory_bytes: usize,
) -> Result<ctx_history_index_generation::CandidateGeneration> {
    let writer_memory_bytes =
        u64::try_from(writer_memory_bytes).map_err(|_| crate::IndexError::CountOverflow)?;
    let candidate = ctx_history_index_generation::create_candidate_generation(
        root,
        base,
        lexical_schema(),
        writer_memory_bytes,
    )?;
    register_body_analyzer(&candidate.index);
    Ok(candidate)
}

pub(crate) fn publish_active_generation_pointer(
    root: &Path,
    pointer: &ActiveGenerationPointer,
) -> Result<PointerPublicationOutcome> {
    Ok(ctx_history_index_generation::publish_active_generation_pointer(root, pointer)?)
}

#[cfg(not(windows))]
pub(crate) fn publish_active_generation_pointer_validated<F, T>(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    validate_before_replace: F,
) -> Result<PointerPublicationOutcome>
where
    F: FnOnce() -> ctx_history_index_generation::Result<T>,
{
    Ok(
        ctx_history_index_generation::publish_active_generation_pointer_validated(
            root,
            pointer,
            validate_before_replace,
        )?,
    )
}

pub(crate) fn sync_generation(path: &Path) -> Result<()> {
    Ok(ctx_history_index_generation::sync_generation(path)?)
}

pub(crate) fn reclaim_inactive_generation_directories(
    root: &Path,
    pointer: Option<&ActiveGenerationPointer>,
    lease: Option<&ctx_history_index_generation::GenerationRetentionLease>,
) -> Result<()> {
    Ok(
        ctx_history_index_generation::reclaim_inactive_generation_directories(
            root, pointer, lease,
        )?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexError, LEXICAL_SCHEMA_VERSION};
    use tantivy::{store::Compressor, IndexSettings};

    fn create_mismatched_slot(root: &Path) -> GenerationSlot {
        let directory_name = "generation-00000000000000000000000000000001";
        let path = root.join(INDEX_GENERATIONS_DIRECTORY).join(directory_name);
        std::fs::create_dir_all(&path).unwrap();
        let mismatched_settings = IndexSettings {
            docstore_compression: Compressor::Zstd(tantivy::store::ZstdCompressor {
                compression_level: Some(1),
            }),
            ..lexical_index_settings()
        };
        Index::builder()
            .schema(lexical_schema())
            .settings(mismatched_settings)
            .create_in_dir(&path)
            .unwrap();
        GenerationSlot::new("0".repeat(64), directory_name.to_owned(), "0".repeat(64)).unwrap()
    }

    #[test]
    fn candidate_schema_and_settings_roundtrip_exactly() {
        let root = tempfile::tempdir().unwrap();
        let candidate = create_candidate_generation(root.path(), None, 0).unwrap();
        assert_eq!(candidate.index.schema(), lexical_schema());
        assert_eq!(candidate.index.settings(), &lexical_index_settings());
        let slot = GenerationSlot::new(
            "0".repeat(64),
            candidate.directory_name.clone(),
            "0".repeat(64),
        )
        .unwrap();
        drop(candidate.index);
        let reopened = open_slot_index(root.path(), &slot).unwrap();
        assert_eq!(reopened.schema(), lexical_schema());
        assert_eq!(reopened.settings(), &lexical_index_settings());
    }

    #[test]
    fn current_schema_rejects_mismatched_physical_settings() {
        let root = tempfile::tempdir().unwrap();
        let slot = create_mismatched_slot(root.path());
        assert!(matches!(
            open_slot_index(root.path(), &slot),
            Err(IndexError::IndexSettingsMismatch(LEXICAL_SCHEMA_VERSION))
        ));
        assert!(matches!(
            create_candidate_generation(root.path(), Some(&slot), 0),
            Err(IndexError::IndexSettingsMismatch(LEXICAL_SCHEMA_VERSION))
        ));
    }
}

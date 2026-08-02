use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use crate::{
    analyzer::register_body_analyzer, durable_directory::DurableMmapDirectory,
    identity::is_generation_id, load_active_generation_pointer, load_manifest_for_metas,
    meta_generation, open_slot_index, payload_generation_id, searcher_generation, validate_schema,
    verify_searcher, verify_searcher_structure, GenerationManifest, IndexError, Result,
};
use tantivy::{ReloadPolicy, Searcher};

/// A verified reader pinned to one immutable lexical generation.
pub struct VerifiedIndex {
    pub(crate) searcher: Searcher,
    pub(crate) manifest: GenerationManifest,
    pub(crate) generation_id: String,
    pub(crate) semantic_eligible_event_count: OnceLock<u64>,
}

impl VerifiedIndex {
    /// Returns the generation named by the validated active pointer and commit
    /// payload without constructing a query reader or auditing documents.
    pub fn active_generation_id(root: impl AsRef<Path>) -> Result<Option<String>> {
        if !root.as_ref().is_dir() {
            return Ok(None);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let Some(pointer) = load_active_generation_pointer(&root)? else {
            return Ok(None);
        };
        let index = open_slot_index(&root, pointer.active())?;
        let generation_id =
            payload_generation_id(&index.load_metas()?)?.ok_or(IndexError::MissingCommitPayload)?;
        if generation_id != pointer.active().generation_id() {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        Ok(Some(generation_id))
    }

    /// Opens a generation, verifies every physical checksum, and audits every
    /// stored Core record plus its source and identity aggregates.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), true, None)
    }

    /// Opens a previously audited immutable generation for querying.
    ///
    /// The manifest digest, generation payload, schema/policy contract,
    /// Tantivy generation pin, and total document count are verified on every
    /// open. The publication-time O(document-count) identity audit is not
    /// repeated for every query.
    pub fn open_pinned(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), false, None)
    }

    /// Opens one exact generation retained as either the active or previous
    /// immutable slot. This lets a terminal publication remain observable
    /// after one serialized successor advances the active pointer.
    pub fn open_retained(root: impl AsRef<Path>, generation_id: &str) -> Result<Self> {
        if !is_generation_id(generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        Self::open_inner(root.as_ref(), false, Some(generation_id))
    }

    fn open_inner(
        root: &Path,
        audit_stored_core: bool,
        retained_generation_id: Option<&str>,
    ) -> Result<Self> {
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        let slot = match retained_generation_id {
            Some(generation_id) => std::iter::once(pointer.active())
                .chain(pointer.previous())
                .find(|slot| slot.generation_id() == generation_id)
                .ok_or_else(|| IndexError::GenerationNotRetained(generation_id.to_owned()))?,
            None => pointer.active(),
        };
        let index = open_slot_index(&root, slot)?;
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let manifest = load_manifest_for_metas(&root, &metas)?;
        if slot.generation_id() != manifest.generation_id()? {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if audit_stored_core {
            if !searcher.index().validate_checksum()?.is_empty() {
                return Err(IndexError::ChecksumMismatch);
            }
            verify_searcher(&searcher, &manifest)?;
        } else {
            verify_searcher_structure(&searcher, &manifest)?;
        }
        let generation_id = manifest.generation_id()?;
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            semantic_eligible_event_count: OnceLock::new(),
        })
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    pub fn document_count(&self) -> u64 {
        self.searcher.num_docs()
    }

    pub fn validate_checksums(&self) -> Result<HashSet<PathBuf>> {
        Ok(self.searcher.index().validate_checksum()?)
    }

    #[cfg(test)]
    pub(crate) fn count_term(&self, term_text: &str) -> Result<usize> {
        use tantivy::{collector::Count, query::TermQuery, schema::IndexRecordOption, Term};

        let body = crate::required_field(self.searcher.schema(), "body_search")?;
        let query = TermQuery::new(
            Term::from_field_text(body, term_text),
            IndexRecordOption::Basic,
        );
        Ok(self.searcher.search(&query, &Count)?)
    }
}

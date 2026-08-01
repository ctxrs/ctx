use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use crate::{
    analyzer::register_body_analyzer, durable_directory::DurableMmapDirectory,
    load_active_generation_pointer, load_manifest_for_metas, meta_generation, open_slot_index,
    payload_generation_id, searcher_generation, validate_schema, verify_searcher,
    verify_searcher_structure, GenerationManifest, IndexError, Result,
};
use tantivy::{Index, ReloadPolicy, Searcher};

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

    /// Opens a generation and performs the exhaustive publication audit.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), true)
    }

    /// Opens a previously audited immutable generation for querying.
    ///
    /// The manifest digest, generation payload, schema/policy contract,
    /// Tantivy generation pin, and total document count are verified on every
    /// open. The publication-time O(document-count) identity audit is not
    /// repeated for every query.
    pub fn open_pinned(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), false)
    }

    fn open_inner(root: &Path, exhaustive: bool) -> Result<Self> {
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let pointer = load_active_generation_pointer(&root)?;
        let legacy = pointer.is_none();
        let index = match &pointer {
            Some(pointer) => open_slot_index(&root, pointer.active())?,
            None if Index::exists(&control_directory).map_err(tantivy::TantivyError::from)? => {
                Index::open(control_directory)?
            }
            None => return Err(IndexError::MissingActiveGenerationPointer),
        };
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let manifest = load_manifest_for_metas(&root, &metas)?;
        if let Some(pointer) = &pointer {
            if pointer.active().generation_id() != manifest.generation_id()? {
                return Err(IndexError::InvalidActiveGenerationPointer);
            }
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if exhaustive || legacy {
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

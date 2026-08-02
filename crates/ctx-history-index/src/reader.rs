use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use crate::{
    analyzer::register_body_analyzer, durable_directory::DurableMmapDirectory, is_generation_id,
    load_active_generation_pointer, load_manifest_for_metas, meta_generation, open_slot_index,
    payload_generation_id, searcher_generation, validate_schema, verify_complete_searcher,
    verify_physical_integrity, verify_searcher_structure, ActiveGenerationPointer,
    GenerationManifest, GenerationSlot, IndexError, Result,
};
use tantivy::{ReloadPolicy, Searcher};

/// A verified reader pinned to one immutable lexical generation.
pub struct VerifiedIndex {
    pub(crate) searcher: Searcher,
    pub(crate) manifest: GenerationManifest,
    pub(crate) generation_id: String,
    pub(crate) semantic_eligibility_postings: OnceLock<crate::query::SemanticEligibilityPostings>,
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

    /// Opens exactly the requested active or retained previous generation.
    ///
    /// Resolution is bounded to the two immutable slots named by the
    /// publication pointer. If that pointer changes while the generation is
    /// being resolved, the complete resolution is retried once against the
    /// new pointer and then fails closed on any second change.
    ///
    /// Like [`Self::open_pinned`], this performs reopen-time structural
    /// verification of the selected manifest, payload, schema/policy contract,
    /// Tantivy generation pin, and total document count. It does not repeat
    /// publication-time physical checksums or the O(document-count) stored-Core
    /// identity and source audit.
    pub fn open_pinned_generation(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
    ) -> Result<Self> {
        Self::open_pinned_generation_with_loader(
            root.as_ref(),
            expected_generation_id,
            load_active_generation_pointer,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_pinned_generation_with_pointer_loader<F>(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
        load_pointer: F,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        Self::open_pinned_generation_with_loader(
            root.as_ref(),
            expected_generation_id,
            load_pointer,
        )
    }

    fn open_pinned_generation_with_loader<F>(
        root: &Path,
        expected_generation_id: &str,
        mut load_pointer: F,
    ) -> Result<Self>
    where
        F: FnMut(&Path) -> Result<Option<ActiveGenerationPointer>>,
    {
        if !is_generation_id(expected_generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();

        let first_pointer = load_pointer(&root)?;
        let first_result =
            Self::open_expected_generation(&root, first_pointer.as_ref(), expected_generation_id);
        let observed_pointer = load_pointer(&root)?;
        if observed_pointer == first_pointer {
            return first_result;
        }

        let retry_result = Self::open_expected_generation(
            &root,
            observed_pointer.as_ref(),
            expected_generation_id,
        );
        if load_pointer(&root)? != observed_pointer {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        retry_result
    }

    fn open_expected_generation(
        root: &Path,
        pointer: Option<&ActiveGenerationPointer>,
        expected_generation_id: &str,
    ) -> Result<Self> {
        let pointer = pointer.ok_or(IndexError::MissingActiveGenerationPointer)?;
        let slot = if pointer.active().generation_id() == expected_generation_id {
            pointer.active()
        } else if let Some(previous) = pointer
            .previous()
            .filter(|slot| slot.generation_id() == expected_generation_id)
        {
            previous
        } else {
            return Err(IndexError::PinnedGenerationNotRetained {
                expected_generation_id: expected_generation_id.to_owned(),
                active_generation_id: pointer.active().generation_id().to_owned(),
                previous_generation_id: pointer
                    .previous()
                    .map(|slot| slot.generation_id().to_owned()),
            });
        };
        Self::open_slot(root, slot, false, |actual_generation_id| {
            IndexError::PinnedGenerationMismatch {
                expected_generation_id: expected_generation_id.to_owned(),
                actual_generation_id,
            }
        })
    }

    fn open_inner(root: &Path, audit_stored_core: bool) -> Result<Self> {
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        Self::open_slot(&root, pointer.active(), audit_stored_core, |_| {
            IndexError::InvalidActiveGenerationPointer
        })
    }

    fn open_slot<F>(
        root: &Path,
        slot: &GenerationSlot,
        audit_stored_core: bool,
        generation_mismatch: F,
    ) -> Result<Self>
    where
        F: FnOnce(String) -> IndexError,
    {
        let index = open_slot_index(root, slot)?;
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let manifest = load_manifest_for_metas(root, &metas)?;
        let generation_id = manifest.generation_id()?;
        if slot.generation_id() != generation_id {
            return Err(generation_mismatch(generation_id));
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
            verify_complete_searcher(
                &searcher,
                &manifest,
                &crate::publication::slot_path(root, slot),
                slot.physical_integrity_digest(),
            )?;
        } else {
            verify_physical_integrity(
                &index,
                &crate::publication::slot_path(root, slot),
                slot.physical_integrity_digest(),
            )?;
            verify_searcher_structure(&searcher, &manifest)?;
        }
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            semantic_eligibility_postings: OnceLock::new(),
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

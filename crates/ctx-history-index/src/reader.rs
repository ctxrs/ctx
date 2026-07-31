use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use crate::{
    analyzer::register_body_analyzer, durable_directory::DurableMmapDirectory,
    load_manifest_for_metas, meta_generation, searcher_generation, validate_schema,
    verify_searcher, verify_searcher_structure, GenerationManifest, IndexError, Result,
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
        let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = directory.root_path().to_path_buf();
        let index = Index::open(directory)?;
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let manifest = load_manifest_for_metas(&root, &metas)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if exhaustive {
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

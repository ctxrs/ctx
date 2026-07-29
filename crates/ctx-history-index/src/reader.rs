use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use tantivy::{collector::Count, schema::IndexRecordOption, Index, ReloadPolicy, Searcher, Term};
use uuid::Uuid;

use crate::{
    durable_directory::DurableMmapDirectory, load_manifest_for_metas, meta_generation,
    required_field, searcher_generation, validate_schema, verify_searcher, GenerationManifest,
    IndexError, Result,
};

/// A verified reader pinned to one immutable lexical generation.
pub struct VerifiedIndex {
    pub(crate) searcher: Searcher,
    pub(crate) manifest: GenerationManifest,
    pub(crate) generation_id: String,
    pub(crate) semantic_eligible_event_count: OnceLock<u64>,
    pub(crate) custom_source_identity_events: OnceLock<Vec<(Uuid, String, String)>>,
}

impl VerifiedIndex {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = directory.root_path().to_path_buf();
        let index = Index::open(directory)?;
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
        verify_searcher(&searcher, &manifest)?;
        let generation_id = manifest.generation_id()?;
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            semantic_eligible_event_count: OnceLock::new(),
            custom_source_identity_events: OnceLock::new(),
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
        use tantivy::query::TermQuery;

        let body = required_field(self.searcher.schema(), "body_search")?;
        let query = TermQuery::new(
            Term::from_field_text(body, term_text),
            IndexRecordOption::Basic,
        );
        Ok(self.searcher.search(&query, &Count)?)
    }
}

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use ctx_history_index_format::{
    count_archive_sources, load_materialized_manifest_for_generation, load_publication_for_metas,
    load_publication_summary_for_metas, validate_read_schema, verify_searcher_document_count,
    GenerationManifest, LoadedPublication, LoadedPublicationSummary, ManifestSourceTotals,
    ManifestSummary, SourceRouteIdentity,
};
use ctx_history_index_generation::{GenerationReadLease, GenerationSlot};
use tantivy::{schema::Schema, IndexMeta, Searcher};

use crate::{IndexError, Result};

pub(super) struct LoadedSlotManifest {
    pub(super) generation_id: String,
    pub(super) indexed_documents: u64,
    pub(super) state: VerifiedManifestState,
}

impl LoadedSlotManifest {
    fn from_full(publication: LoadedPublication) -> Self {
        let (generation_id, manifest) = publication.into_parts();
        let indexed_documents = manifest.indexed_documents;
        Self {
            generation_id,
            indexed_documents,
            state: VerifiedManifestState::Loaded(manifest),
        }
    }

    /// Loads manifest metadata for a slot, using the cryptographically pinned
    /// summary sidecar when `lazy_manifest` is enabled and falling back to the
    /// authoritative full manifest if the sidecar is absent, unpinned,
    /// mismatched, or disagrees with the slot or schema.
    pub(super) fn load_for_slot<F>(
        root: &Path,
        metas: &IndexMeta,
        schema: &Schema,
        slot: &GenerationSlot,
        lazy_manifest: bool,
        generation_mismatch: F,
    ) -> Result<Self>
    where
        F: FnOnce(String) -> IndexError,
    {
        let (loaded, lexical_schema_version) = if lazy_manifest {
            match load_publication_summary_for_metas(root, metas, slot.summary_sha256())? {
                LoadedPublicationSummary::Summary(summary)
                    if slot.generation_id() == summary.generation_id
                        && validate_read_schema(schema, summary.lexical_schema_version).is_ok() =>
                {
                    let generation_id = summary.generation_id.clone();
                    let schema_version = summary.lexical_schema_version;
                    let indexed_documents = summary.indexed_documents;
                    (
                        Self {
                            generation_id,
                            indexed_documents,
                            state: VerifiedManifestState::Deferred {
                                summary,
                                root: root.to_path_buf(),
                                loaded: OnceLock::new(),
                            },
                        },
                        schema_version,
                    )
                }
                LoadedPublicationSummary::Summary(_) => {
                    let publication = load_publication_for_metas(root, metas)?;
                    let schema_version = publication.manifest().lexical_schema_version;
                    (Self::from_full(publication), schema_version)
                }
                LoadedPublicationSummary::Full(publication) => {
                    let schema_version = publication.manifest().lexical_schema_version;
                    (Self::from_full(publication), schema_version)
                }
            }
        } else {
            let publication = load_publication_for_metas(root, metas)?;
            let schema_version = publication.manifest().lexical_schema_version;
            (Self::from_full(publication), schema_version)
        };
        validate_read_schema(schema, lexical_schema_version)?;
        if slot.generation_id() != loaded.generation_id {
            return Err(generation_mismatch(loaded.generation_id));
        }
        Ok(loaded)
    }

    /// Verifies `searcher` document count against this manifest; if a summary
    /// sidecar disagrees, falls back to the authoritative full manifest and
    /// verifies strictly against it.
    pub(super) fn verify_document_count<F>(
        &mut self,
        root: &Path,
        metas: &IndexMeta,
        schema: &Schema,
        slot: &GenerationSlot,
        searcher: &Searcher,
        generation_mismatch: F,
    ) -> Result<()>
    where
        F: FnOnce(String) -> IndexError,
    {
        if verify_searcher_document_count(searcher, self.indexed_documents).is_ok() {
            return Ok(());
        }
        if matches!(self.state, VerifiedManifestState::Deferred { .. }) {
            let publication = load_publication_for_metas(root, metas)?;
            let schema_version = publication.manifest().lexical_schema_version;
            let full = Self::from_full(publication);
            validate_read_schema(schema, schema_version)?;
            if slot.generation_id() != full.generation_id {
                return Err(generation_mismatch(full.generation_id));
            }
            *self = full;
        }
        verify_searcher_document_count(searcher, self.indexed_documents)
    }
}

pub(super) enum VerifiedManifestState {
    Loaded(Arc<GenerationManifest>),
    Deferred {
        summary: Box<ManifestSummary>,
        root: PathBuf,
        loaded: OnceLock<Arc<GenerationManifest>>,
    },
}

impl VerifiedManifestState {
    pub(super) fn load_arc(
        &self,
        generation_id: &str,
        lease: Option<&GenerationReadLease>,
    ) -> Result<&Arc<GenerationManifest>> {
        match self {
            Self::Loaded(manifest) => Ok(manifest),
            Self::Deferred { root, loaded, .. } => {
                if let Some(manifest) = loaded.get() {
                    return Ok(manifest);
                }
                let materialized = match lease {
                    Some(lease) => lease
                        .with_root_access(|lease_root| {
                            load_materialized_manifest_for_generation(lease_root, generation_id)
                        })
                        .map_err(IndexError::from)??,
                    None => load_materialized_manifest_for_generation(root, generation_id)?,
                };
                Ok(loaded.get_or_init(|| materialized))
            }
        }
    }

    pub(super) fn certified_source_bytes(&self) -> u64 {
        match self {
            Self::Loaded(manifest) => manifest.certified_source_bytes,
            Self::Deferred { summary, .. } => summary.certified_source_bytes,
        }
    }

    pub(super) fn certified_source_count(&self) -> usize {
        match self {
            Self::Loaded(manifest) => manifest.sources.len(),
            Self::Deferred { summary, .. } => {
                usize::try_from(summary.source_count).unwrap_or(usize::MAX)
            }
        }
    }

    pub(super) fn certified_source_totals(&self) -> Result<ManifestSourceTotals> {
        match self {
            Self::Loaded(manifest) => ManifestSourceTotals::from_manifest(manifest),
            Self::Deferred { summary, .. } => Ok(summary.source_totals()),
        }
    }

    pub(super) fn has_archive_sources(&self) -> bool {
        match self {
            Self::Loaded(manifest) => count_archive_sources(manifest) > 0,
            Self::Deferred { summary, .. } => summary.archive_source_count > 0,
        }
    }

    pub(super) fn policy_schema_hash(&self) -> &str {
        match self {
            Self::Loaded(manifest) => &manifest.policy_schema_hash,
            Self::Deferred { summary, .. } => &summary.policy_schema_hash,
        }
    }

    pub(super) fn lexical_schema_version(&self) -> u32 {
        match self {
            Self::Loaded(manifest) => manifest.lexical_schema_version,
            Self::Deferred { summary, .. } => summary.lexical_schema_version,
        }
    }

    pub(super) fn route_is_present_nonempty(
        &self,
        generation_id: &str,
        lease: Option<&GenerationReadLease>,
        route: &SourceRouteIdentity,
    ) -> Result<bool> {
        if let Self::Deferred { summary, .. } = self {
            if let Some(routes) = summary.present_nonempty_route_identities.as_deref() {
                return Ok(routes
                    .binary_search_by(|candidate| candidate.as_str().cmp(route.as_str()))
                    .is_ok());
            }
        }
        let manifest = self.load_arc(generation_id, lease)?;
        Ok(manifest.source_route(route).is_some_and(|snapshot| {
            snapshot.missing_state().is_none() && !snapshot.sources().is_empty()
        }))
    }

    pub(super) fn has_loaded_full_manifest(&self) -> bool {
        match self {
            Self::Loaded(_) => true,
            Self::Deferred { loaded, .. } => loaded.get().is_some(),
        }
    }
}

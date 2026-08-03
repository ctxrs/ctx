use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    CoreRecord, SourceKey, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
};
use tantivy::Searcher;

use crate::{
    index_document::{core_content_bytes, IndexSourceFields},
    load_active_generation_pointer, prior_core_record, staging, verify_physical_integrity, Fields,
    GenerationSlot, IndexDocument, IndexError, Result, INDEX_GENERATIONS_DIRECTORY,
};

#[cfg(test)]
thread_local! {
    static FINAL_ENCODINGS: Cell<usize> = const { Cell::new(0) };
}

/// Immutable authority for canonical Core-record preparation.
///
/// Clones may run concurrently. They can consult only the pinned immutable
/// base generation for repository-certificate reuse and cannot mutate source
/// lifecycle or publication state.
#[derive(Clone)]
pub struct CoreRecordPreparer {
    fields: Fields,
    context_generation_id: Option<String>,
    base: Option<Arc<PreparationBase>>,
}

struct PreparationBase {
    root: PathBuf,
    slot: GenerationSlot,
    searcher: Searcher,
    physical_integrity_verified: Mutex<bool>,
}

impl CoreRecordPreparer {
    pub(crate) fn new(
        fields: Fields,
        context_generation_id: Option<String>,
        base: Option<(PathBuf, GenerationSlot, Searcher)>,
    ) -> Self {
        Self {
            fields,
            context_generation_id,
            base: base.map(|(root, slot, searcher)| {
                Arc::new(PreparationBase {
                    root,
                    slot,
                    searcher,
                    physical_integrity_verified: Mutex::new(false),
                })
            }),
        }
    }

    /// Reuses any matching immutable repository certificate, then performs
    /// exactly one final canonical encoding and derives every lexical
    /// projection and aggregate leaf from those same bytes. Corrupt base
    /// authority is returned as a rebuild-required error; preparation never
    /// persists publication state.
    pub fn prepare(&self, mut record: CoreRecord) -> Result<PreparedCoreRecord> {
        if record.normalization_revision != CORE_NORMALIZATION_REVISION
            || record.content.policy_revision != CORE_CONTENT_POLICY_REVISION
        {
            return Err(IndexError::CoreRecordPolicyRevisionMismatch {
                normalization: record.normalization_revision,
                expected_normalization: CORE_NORMALIZATION_REVISION,
                content: record.content.policy_revision,
                expected_content: CORE_CONTENT_POLICY_REVISION,
            });
        }
        if record.needs_prior_repository_certificate() {
            if let Some(base) = &self.base {
                base.validate_physical_integrity()?;
                let prior =
                    prior_core_record(&base.searcher, self.fields, record.event_id, &record.source)
                        .map_err(|error| base.rebuild_required(error))?;
                if let Some(prior) = prior {
                    let _ = record.reuse_prior_repository_certificate(&prior);
                }
            }
        }

        let source = record.source.clone();
        let source_token = crate::source_token(&source);
        let event_id = record.event_id;
        let core_content_bytes = core_content_bytes(&record.content)?;
        #[cfg(test)]
        FINAL_ENCODINGS.with(|count| count.set(count.get() + 1));
        let encoded_core_record: Arc<[u8]> = record.encode_stored()?.into();
        let encoded_core_bytes = encoded_core_record.len();
        let record_leaf = staging::core_record_leaf(event_id, &encoded_core_record)?;
        let record_accumulator_leaf =
            staging::core_record_accumulator_leaf(event_id, &record_leaf)?;
        let document = IndexDocument::from_core(
            self.fields,
            record,
            Arc::clone(&encoded_core_record),
            core_content_bytes,
            IndexSourceFields::new(&source, &source_token),
        )?;

        Ok(PreparedCoreRecord {
            base_generation_id: self.context_generation_id.clone(),
            source,
            source_token,
            encoded_core_bytes,
            encoded_core_record,
            record_accumulator_leaf,
            document,
        })
    }
}

impl PreparationBase {
    fn validate_physical_integrity(&self) -> Result<()> {
        let mut verified = self.physical_integrity_verified.lock().map_err(|_| {
            IndexError::WriterInvariant("base physical-integrity validation lock poisoned")
        })?;
        if *verified {
            return Ok(());
        }
        let pointer = load_active_generation_pointer(&self.root)?;
        if pointer.as_ref().map(|pointer| pointer.active()) != Some(&self.slot) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let generation_path = self
            .root
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(self.slot.directory());
        verify_physical_integrity(
            self.searcher.index(),
            &generation_path,
            self.slot.physical_integrity_digest(),
        )
        .map_err(|error| self.rebuild_required(error))?;
        *verified = true;
        Ok(())
    }

    fn rebuild_required(&self, error: IndexError) -> IndexError {
        IndexError::ActiveGenerationNeedsRebuild {
            generation_id: self.slot.generation_id().to_owned(),
            detail: error.to_string(),
        }
    }
}

/// Opaque immutable result of canonical Core preparation.
pub struct PreparedCoreRecord {
    base_generation_id: Option<String>,
    source: SourceKey,
    source_token: String,
    encoded_core_bytes: usize,
    encoded_core_record: Arc<[u8]>,
    record_accumulator_leaf: [u8; 32],
    document: IndexDocument,
}

impl PreparedCoreRecord {
    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    /// Exact byte length of the final post-certificate canonical encoding.
    pub fn encoded_core_bytes(&self) -> usize {
        self.encoded_core_bytes
    }

    pub(crate) fn base_generation_id(&self) -> Option<&str> {
        self.base_generation_id.as_deref()
    }

    pub(crate) fn source_token(&self) -> &str {
        &self.source_token
    }

    pub(crate) fn into_parts(self) -> PreparedCoreRecordParts {
        debug_assert_eq!(self.encoded_core_bytes, self.encoded_core_record.len());
        PreparedCoreRecordParts {
            record_accumulator_leaf: self.record_accumulator_leaf,
            document: self.document,
        }
    }
}

impl fmt::Debug for PreparedCoreRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCoreRecord")
            .field("source", &self.source)
            .field("encoded_core_bytes", &self.encoded_core_bytes)
            .finish_non_exhaustive()
    }
}

pub(crate) struct PreparedCoreRecordParts {
    pub(crate) record_accumulator_leaf: [u8; 32],
    pub(crate) document: IndexDocument,
}

#[cfg(test)]
pub(crate) fn reset_final_encoding_count() {
    FINAL_ENCODINGS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn final_encoding_count() -> usize {
    FINAL_ENCODINGS.with(Cell::get)
}

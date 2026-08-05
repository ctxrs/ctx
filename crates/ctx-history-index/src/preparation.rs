use std::{
    fmt,
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    CoreRecord, SourceKey, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use tantivy::Searcher;

use crate::{
    index_document::IndexSourceFields, load_active_generation_pointer, prior_core_record, staging,
    verify_physical_integrity, Fields, GenerationSlot, IndexDocument, IndexError, Result,
    INDEX_GENERATIONS_DIRECTORY,
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
    pub fn prepare(&self, record: CoreRecord) -> Result<PreparedCoreRecord> {
        match self
            .prepare_draft(record)?
            .materialize(MAX_ENCODED_CORE_RECORD_BYTES)?
        {
            PreparedCoreRecordMaterialization::Prepared(prepared) => Ok(prepared),
            PreparedCoreRecordMaterialization::CapacityExceeded(_) => {
                Err(IndexError::DocumentFieldTooLarge {
                    field: "core_record",
                    actual: MAX_ENCODED_CORE_RECORD_BYTES.saturating_add(1),
                    maximum: MAX_ENCODED_CORE_RECORD_BYTES,
                })
            }
        }
    }

    /// Resolves immutable base authority and validates one record without
    /// allocating its canonical stored encoding or lexical document. Callers
    /// that govern cross-thread memory can therefore acquire a permit before
    /// materialization begins.
    pub fn prepare_draft(&self, mut record: CoreRecord) -> Result<PreparedCoreRecordDraft> {
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

        let core_content_bytes = record.validate_contract_and_content_bytes()?;
        let source = record.source.clone();
        let source_token = crate::source_token(&source);
        Ok(PreparedCoreRecordDraft {
            fields: self.fields,
            base_generation_id: self.context_generation_id.clone(),
            record,
            source,
            source_token,
            core_content_bytes,
        })
    }
}

/// Opaque validated Core preparation state that has not allocated the final
/// stored encoding or index document.
pub struct PreparedCoreRecordDraft {
    fields: Fields,
    base_generation_id: Option<String>,
    record: CoreRecord,
    source: SourceKey,
    source_token: String,
    core_content_bytes: usize,
}

/// Result of attempting final materialization under a caller-owned exact-byte
/// permit. Capacity exhaustion returns the untouched draft so a bounded
/// scheduler can flush, acquire a larger permit, and retry.
// Boxing the ordinary prepared result would add one allocation to every
// indexed record. Keep the hot success path inline and box only the uncommon
// capacity retry.
#[allow(clippy::large_enum_variant)]
pub enum PreparedCoreRecordMaterialization {
    Prepared(PreparedCoreRecord),
    CapacityExceeded(Box<PreparedCoreRecordDraft>),
}

impl PreparedCoreRecordDraft {
    pub fn materialize(
        self,
        maximum_encoded_bytes: usize,
    ) -> Result<PreparedCoreRecordMaterialization> {
        let maximum_encoded_bytes = maximum_encoded_bytes.min(MAX_ENCODED_CORE_RECORD_BYTES);
        let mut encoded = BoundedJsonBuffer::new(maximum_encoded_bytes);
        if let Err(error) = serde_json::to_writer(&mut encoded, &self.record) {
            if encoded.capacity_exceeded() {
                return Ok(PreparedCoreRecordMaterialization::CapacityExceeded(
                    Box::new(self),
                ));
            }
            return Err(error.into());
        }
        let encoded_core_record = encoded.into_bytes();
        let encoded_core_bytes = encoded_core_record.len();
        if encoded_core_bytes == 0 {
            return Err(IndexError::EmptyDocumentField {
                field: "core_record",
            });
        }
        #[cfg(test)]
        FINAL_ENCODINGS.with(|count| count.set(count.get() + 1));
        let event_id = self.record.event_id;
        let record_leaf = staging::core_record_leaf(event_id, &encoded_core_record)?;
        let record_accumulator_leaf =
            staging::core_record_accumulator_leaf(event_id, &record_leaf)?;
        let document = IndexDocument::from_core(
            self.fields,
            self.record,
            encoded_core_record,
            self.core_content_bytes,
            IndexSourceFields::new(&self.source, &self.source_token),
        )?;

        Ok(PreparedCoreRecordMaterialization::Prepared(
            PreparedCoreRecord {
                base_generation_id: self.base_generation_id,
                source: self.source,
                source_token: self.source_token,
                encoded_core_bytes,
                record_accumulator_leaf,
                document,
            },
        ))
    }
}

struct BoundedJsonBuffer {
    bytes: Vec<u8>,
    maximum: usize,
    capacity_exceeded: bool,
}

impl BoundedJsonBuffer {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            capacity_exceeded: false,
        }
    }

    fn capacity_exceeded(&self) -> bool {
        self.capacity_exceeded
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(bytes.len()) else {
            self.capacity_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded Core record byte count overflowed",
            ));
        };
        if next_len > self.maximum {
            self.capacity_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded Core record exceeded its materialization permit",
            ));
        }
        if next_len > self.bytes.capacity() {
            let next_capacity = self
                .bytes
                .capacity()
                .max(1024)
                .saturating_mul(2)
                .min(self.maximum)
                .max(next_len);
            self.bytes
                .try_reserve_exact(next_capacity.saturating_sub(self.bytes.len()))
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
        let pointer = load_active_generation_pointer(&self.root)?
            .ok_or(IndexError::ConcurrentGenerationChange)?;
        if pointer.active() != &self.slot {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let generation_path = self
            .root
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(self.slot.directory());
        verify_physical_integrity(
            self.searcher.index(),
            &generation_path,
            Some(&pointer),
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

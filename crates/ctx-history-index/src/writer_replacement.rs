use super::*;
use ctx_history_core::StableEntityId;
use ctx_history_index_format::{
    validated_core_record_bytes, SessionAuthorityKey, SourceEventOrderKey,
};
use tantivy::{
    schema::{Document, Value},
    DocAddress, DocSet, TantivyDocument, TERMINATED,
};

// Charged against a portion of the existing warm indexing buffer budget.
// This bounds reconciliation allocations, not the process or reader-cache RSS.
const ENTRY_CHARGE: usize = 64;
const COMPARISON_OVERHEAD: usize = 128 * 1024;

#[derive(Clone, Copy)]
struct BaseEntry {
    digest: [u8; 32],
    address: DocAddress,
    encoded_bytes: usize,
    seen: bool,
}

#[derive(Clone)]
pub(super) struct Replacement {
    entries: Vec<BaseEntry>,
    charge: usize,
    pub(super) finished: bool,
}

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReplacementWork {
    pub(super) base_documents: usize,
    pub(super) source_deletions: usize,
    pub(super) event_deletions: usize,
    pub(super) document_adds: usize,
    pub(super) retained_documents: usize,
}

impl GenerationWriter {
    pub(super) fn begin_replacement(&mut self, source: &SourceKey) -> Result<Option<Replacement>> {
        // A previous candidate deletion may have removed these immutable base
        // documents already. Re-adding the source must restore them physically.
        if self.deletions.contains_key(source) || self.route_deletions.contains(source) {
            return Ok(None);
        }
        let Some(base) = self.base_publication.as_ref() else {
            return Ok(None);
        };
        let Some(certificate) = base
            .manifest()
            .sources
            .binary_search_by_key(&source.identity().digest(), |candidate| {
                candidate.observation().source().identity().digest()
            })
            .ok()
            .and_then(|index| base.manifest().sources.get(index))
            .filter(|candidate| candidate.observation().source().exact_descriptor_eq(source))
        else {
            return Ok(None);
        };
        let available = self.replacement_memory_bytes - self.replacement_memory_used;
        let replacement = load_base_entries(
            base.searcher(),
            self.fields,
            source,
            certificate.counts().indexed_documents,
            available,
        )
        .map_err(|error| self.replacement_base_failure(error))?;
        if let Some(replacement) = &replacement {
            self.replacement_memory_used += replacement.charge;
            #[cfg(test)]
            {
                self.replacement_work.base_documents += replacement.entries.len();
            }
        }
        Ok(replacement)
    }

    pub(super) fn stage_replacement_document(
        &mut self,
        token: &str,
        event: StableEntityId,
        document: IndexDocument,
    ) -> Result<()> {
        let entry = match self
            .pending
            .get(token)
            .and_then(|pending| pending.replacement.as_ref())
        {
            Some(replacement) if replacement.finished => {
                return Err(IndexError::DocumentSourceNotActive);
            }
            Some(replacement) => replacement
                .entries
                .binary_search_by_key(&event.digest(), |entry| entry.digest)
                .ok()
                .map(|index| (index, replacement.entries[index])),
            None => None,
        };
        let retain = if let Some((_, entry)) = entry {
            if entry.seen {
                return Err(IndexError::DuplicateEventIdentity(event.to_string()));
            }
            let base = self
                .base_publication
                .as_ref()
                .ok_or(IndexError::WriterInvariant(
                    "differential replacement lost its pinned base",
                ))?;
            matching_document(base.searcher(), self.fields, entry, &document)
                .map_err(|error| self.replacement_base_failure(error))?
        } else {
            false
        };
        if retain {
            #[cfg(test)]
            {
                self.replacement_work.retained_documents += 1;
            }
        } else {
            if let Some((_, entry)) = entry {
                self.delete_replacement_event(entry.digest)?;
            }
            self.add_index_document(document)?;
        }
        if let Some((index, _)) = entry {
            self.pending
                .get_mut(token)
                .and_then(|pending| pending.replacement.as_mut())
                .ok_or(IndexError::WriterInvariant(
                    "replacement inventory disappeared",
                ))?
                .entries[index]
                .seen = true;
        }
        Ok(())
    }

    pub(super) fn add_index_document(&mut self, document: IndexDocument) -> Result<()> {
        self.writer_mut()?.add_document(document).map_err(|error| {
            writer_publication::observe_candidate_failure(&self.root, error.into())
        })?;
        #[cfg(test)]
        {
            self.replacement_work.document_adds += 1;
        }
        Ok(())
    }

    fn delete_replacement_event(&mut self, digest: [u8; 32]) -> Result<()> {
        let term = Term::from_field_text(self.fields.event_identity_digest, &hex(&digest));
        self.writer_mut()?.delete_term(term);
        #[cfg(test)]
        {
            self.replacement_work.event_deletions += 1;
        }
        Ok(())
    }

    pub(super) fn finish_replacement(&mut self, token: &str) -> Result<()> {
        let Some(mut replacement) = self
            .pending
            .get_mut(token)
            .and_then(|p| p.replacement.take())
        else {
            return Ok(());
        };
        let result = replacement
            .entries
            .iter()
            .filter(|entry| !entry.seen)
            .try_for_each(|entry| self.delete_replacement_event(entry.digest));
        if result.is_ok() {
            self.replacement_memory_used -= replacement.charge;
            replacement.entries = Vec::new();
            replacement.charge = 0;
            replacement.finished = true;
        }
        self.pending
            .get_mut(token)
            .ok_or(IndexError::DocumentSourceNotActive)?
            .replacement = Some(replacement);
        result
    }

    // Checkpoints contain the pending sources that own every allocation.
    // Recompute only on rollback, not once per admitted source.
    pub(super) fn restore_replacement_memory(&mut self) {
        self.replacement_memory_used = self
            .pending
            .values()
            .filter_map(|pending| pending.replacement.as_ref())
            .map(|replacement| replacement.charge)
            .sum();
    }

    fn replacement_base_failure(&mut self, error: IndexError) -> IndexError {
        let Some(pointer) = self.active_pointer.as_ref() else {
            return error;
        };
        let error = classify_active_integrity_failure(&self.root, pointer.active(), error);
        self.reusable_base_rebuild_detail = Some(error.to_string());
        error
    }
}

fn load_base_entries(
    searcher: &Searcher,
    fields: Fields,
    source: &SourceKey,
    count: u64,
    available: usize,
) -> Result<Option<Replacement>> {
    let Ok(count) = usize::try_from(count) else {
        return Ok(None);
    };
    let entry_charge = ENTRY_CHARGE.max(std::mem::size_of::<BaseEntry>());
    let Some(initial_charge) = count
        .checked_mul(entry_charge)
        .and_then(|bytes| bytes.checked_add(COMPARISON_OVERHEAD))
    else {
        return Ok(None);
    };
    if count == 0 || initial_charge > available {
        return Ok(None);
    }
    let mut entries = Vec::new();
    if entries.try_reserve_exact(count).is_err() {
        return Ok(None);
    }
    let Some(metadata_charge) = entries
        .capacity()
        .checked_mul(entry_charge)
        .and_then(|bytes| bytes.checked_add(COMPARISON_OVERHEAD))
    else {
        return Ok(None);
    };
    if metadata_charge > available {
        return Ok(None);
    }
    let mut charge = metadata_charge;
    let prefix = SourceEventOrderKey::source_prefix(source);
    let end = SourceEventOrderKey::source_range_end(source);
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let inverted = segment.inverted_index(fields.source_event_order)?;
        let mut terms = inverted.terms().range().ge(prefix).lt(&end).into_stream()?;
        while terms.advance() {
            let order = SourceEventOrderKey::decode_for_source(source, terms.key())?;
            let mut postings =
                inverted.read_postings_from_terminfo(terms.value(), IndexRecordOption::Basic)?;
            while postings.doc() != TERMINATED {
                let doc_id = postings.doc();
                if !segment.is_deleted(doc_id) {
                    if entries.len() == count {
                        return Err(IndexError::InvalidStoredDocumentField("source_event_order"));
                    }
                    // Reserve the old stored bytes plus decoding space; prepared
                    // candidate bytes retain their existing pipeline ownership.
                    let Some(required) = order
                        .encoded_core_bytes()
                        .checked_mul(2)
                        .and_then(|bytes| bytes.checked_add(metadata_charge))
                    else {
                        return Ok(None);
                    };
                    charge = charge.max(required);
                    if charge > available {
                        return Ok(None);
                    }
                    entries.push(BaseEntry {
                        digest: order.event_digest(),
                        address: DocAddress::new(segment_ord as u32, doc_id),
                        encoded_bytes: order.encoded_core_bytes(),
                        seen: false,
                    });
                }
                postings.advance();
            }
        }
    }
    entries.sort_unstable_by_key(|entry| entry.digest);
    if entries.len() != count
        || entries
            .windows(2)
            .any(|pair| pair[0].digest == pair[1].digest)
    {
        return Err(IndexError::InvalidStoredDocumentField("source_event_order"));
    }
    Ok(Some(Replacement {
        entries,
        charge,
        finished: false,
    }))
}

fn matching_document(
    searcher: &Searcher,
    fields: Fields,
    entry: BaseEntry,
    current: &IndexDocument,
) -> Result<bool> {
    let current_core = current
        .iter_fields_and_values()
        .find(|(field, _)| *field == fields.core_record)
        .and_then(|(_, value)| value.as_bytes())
        .ok_or(IndexError::WriterInvariant(
            "prepared document lost its Core bytes",
        ))?;
    if current_core.len() != entry.encoded_bytes {
        return Ok(false);
    }
    let stored: TantivyDocument = searcher.doc(entry.address)?;
    let base_core = validated_core_record_bytes(searcher, entry.address, &stored, fields)?;
    let mut authorities = stored.get_all(fields.session_authority);
    let base_authority = authorities
        .next()
        .map(|value| {
            let bytes = value
                .as_bytes()
                .ok_or(IndexError::InvalidStoredDocumentField("session_authority"))?;
            SessionAuthorityKey::decode(bytes)?;
            Ok::<_, IndexError>(bytes)
        })
        .transpose()?;
    if authorities.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
    }
    let current_authority = current
        .iter_fields_and_values()
        .find(|(field, _)| *field == fields.session_authority)
        .and_then(|(_, value)| value.as_bytes());
    Ok(base_core == current_core && base_authority == current_authority)
}

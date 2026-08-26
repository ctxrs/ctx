use std::collections::HashMap;

use ctx_history_core::{CoreRecord, StableEntityId};
use ctx_history_index_format::{unique_required_bytes, SessionAuthorityKey};
use tantivy::{
    schema::IndexRecordOption, DocAddress, DocSet, Searcher, TantivyDocument, TERMINATED,
};
use uuid::Uuid;

use crate::{
    hex, merge_session_identity_facts, preparation::PreparedSessionIdentityFacts, Fields,
    IndexError, Result,
};

#[cfg(test)]
use std::cell::Cell;

pub(crate) const MAX_SESSION_WITNESS_VISITS: usize = 32;
/// Bounds the fixed segment fan-out before the sparse authority dictionary is
/// opened. Keep this aligned with the manual lexical query ceiling.
pub(crate) const MAX_SESSION_WITNESS_SEGMENT_PROBES: usize = 512;

/// Test-only accounting for the bounded base witness lookup.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PriorSessionIdentityLookupWork {
    pub(crate) segment_range_probes: usize,
    pub(crate) dictionary_terms: usize,
    pub(crate) postings: usize,
    pub(crate) core_decodes: usize,
}

#[cfg(test)]
thread_local! {
    static PRIOR_SESSION_IDENTITY_LOOKUP_WORK: Cell<PriorSessionIdentityLookupWork> =
        const { Cell::new(PriorSessionIdentityLookupWork { segment_range_probes: 0, dictionary_terms: 0, postings: 0, core_decodes: 0 }) };
}

#[cfg(test)]
pub(crate) fn reset_prior_session_identity_lookup_work() {
    PRIOR_SESSION_IDENTITY_LOOKUP_WORK
        .with(|work| work.set(PriorSessionIdentityLookupWork::default()));
}

#[cfg(test)]
pub(crate) fn prior_session_identity_lookup_work() -> PriorSessionIdentityLookupWork {
    PRIOR_SESSION_IDENTITY_LOOKUP_WORK.with(Cell::get)
}

#[cfg(test)]
fn note_segment_range_probe() {
    PRIOR_SESSION_IDENTITY_LOOKUP_WORK.with(|cell| {
        let mut work = cell.get();
        work.segment_range_probes = work.segment_range_probes.saturating_add(1);
        cell.set(work);
    });
}

#[cfg(test)]
fn note_dictionary_term() {
    PRIOR_SESSION_IDENTITY_LOOKUP_WORK.with(|cell| {
        let mut work = cell.get();
        work.dictionary_terms = work.dictionary_terms.saturating_add(1);
        cell.set(work);
    });
}

#[cfg(test)]
fn note_posting_visit() {
    PRIOR_SESSION_IDENTITY_LOOKUP_WORK.with(|cell| {
        let mut work = cell.get();
        work.postings = work.postings.saturating_add(1);
        cell.set(work);
    });
}

#[cfg(test)]
fn note_core_decode() {
    PRIOR_SESSION_IDENTITY_LOOKUP_WORK.with(|cell| {
        let mut work = cell.get();
        work.core_decodes = work.core_decodes.saturating_add(1);
        cell.set(work);
    });
}

#[cfg(not(test))]
fn note_segment_range_probe() {}

#[cfg(not(test))]
fn note_dictionary_term() {}

#[cfg(not(test))]
fn note_posting_visit() {}

#[cfg(not(test))]
fn note_core_decode() {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseWitnessSource {
    Active,
    Replaced,
    Deleted,
}

/// Reads the sparse witness field for one exact compact session range. Segment
/// range probes and carrier visits have independent small caps; dictionary
/// terms and postings, including tombstones, share the carrier cap. Only live
/// witness Core documents are decoded.
pub(crate) fn prior_session_identity_facts<F>(
    searcher: &Searcher,
    fields: Fields,
    session_id: StableEntityId,
    source_state: F,
) -> Result<Option<PreparedSessionIdentityFacts>>
where
    F: Fn(StableEntityId, &CoreRecord) -> Result<BaseWitnessSource>,
{
    let prefix = SessionAuthorityKey::uuid_prefix(session_id)?;
    let range_end = SessionAuthorityKey::uuid_range_end(session_id)?;
    let mut merged = None;
    let mut segment_probes = 0_usize;
    let mut visits = 0_usize;

    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        note_segment_range_probe();
        segment_probes = segment_probes
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        ensure_witness_segment_probe_cap(segment_probes)?;
        let inverted = segment.inverted_index(fields.session_authority)?;
        let mut terms = inverted
            .terms()
            .range()
            .ge(prefix)
            .lt(&range_end)
            .into_stream()?;
        while terms.advance() {
            note_dictionary_term();
            visits = visits.checked_add(1).ok_or(IndexError::CountOverflow)?;
            ensure_witness_visit_cap(visits)?;
            let key = SessionAuthorityKey::decode(terms.key())?;
            let (witness_session, witness_owner) = key.identities()?;
            let mut postings =
                inverted.read_postings_from_terminfo(terms.value(), IndexRecordOption::Basic)?;
            let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                note_posting_visit();
                visits = visits.checked_add(1).ok_or(IndexError::CountOverflow)?;
                ensure_witness_visit_cap(visits)?;
                if !segment.is_deleted(doc_id) {
                    note_core_decode();
                    let document: TantivyDocument =
                        searcher.doc(DocAddress::new(segment_ord, doc_id))?;
                    let stored_key = unique_required_bytes(
                        &document,
                        fields.session_authority,
                        "session_authority",
                    )?;
                    if stored_key != terms.key() {
                        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
                    }
                    let core = CoreRecord::decode_stored(unique_required_bytes(
                        &document,
                        fields.core_record,
                        "core_record",
                    )?)?;
                    if SessionAuthorityKey::for_core_record(&core)? != key
                        || core.session_id.encode_canonical()?
                            != witness_session.encode_canonical()?
                        || core.source.identity().encode_canonical()?
                            != witness_owner.encode_canonical()?
                    {
                        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
                    }
                    let state = source_state(witness_owner, &core)?;
                    if state != BaseWitnessSource::Deleted
                        && witness_session.encode_canonical()? != session_id.encode_canonical()?
                    {
                        return Err(IndexError::CompactIdentityCollision {
                            kind: "session",
                            uuid: session_id.as_uuid(),
                            existing_digest: hex(&session_id.digest()),
                            new_digest: hex(&witness_session.digest()),
                        });
                    }
                    if state == BaseWitnessSource::Active {
                        let facts = PreparedSessionIdentityFacts {
                            session_id: core.session_id,
                            source_owner: core.source.identity().digest(),
                            relationship: crate::preparation::PreparedSessionRelationship {
                                parent_session_id: core.parent_session_id,
                                root_session_id: core.root_session_id,
                                kind: core.session_relationship,
                            },
                        };
                        merged = Some(match merged {
                            Some(existing) => merge_session_identity_facts(existing, facts)?,
                            None => facts,
                        });
                    }
                }
                doc_id = postings.advance();
            }
        }
    }
    Ok(merged)
}

fn ensure_witness_visit_cap(visits: usize) -> Result<()> {
    if visits > MAX_SESSION_WITNESS_VISITS {
        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
    }
    Ok(())
}

fn ensure_witness_segment_probe_cap(probes: usize) -> Result<()> {
    if probes > MAX_SESSION_WITNESS_SEGMENT_PROBES {
        return Err(IndexError::SessionAuthorityWorkLimitExceeded {
            operation: "segment probes",
            maximum: MAX_SESSION_WITNESS_SEGMENT_PROBES,
        });
    }
    Ok(())
}

pub(crate) fn register_compact_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
    kind: &'static str,
    duplicate_is_error: bool,
) -> Result<()> {
    let uuid = identity.as_uuid();
    let digest = identity.digest();
    match identities.entry(uuid) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(digest);
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(entry) if *entry.get() == digest => {
            if duplicate_is_error {
                Err(IndexError::DuplicateEventIdentity(uuid.to_string()))
            } else {
                Ok(())
            }
        }
        std::collections::hash_map::Entry::Occupied(entry) => {
            Err(IndexError::CompactIdentityCollision {
                kind,
                uuid,
                existing_digest: hex(entry.get()),
                new_digest: hex(&digest),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_witness_segment_probe_cap_is_typed_and_inclusive() {
        assert!(ensure_witness_segment_probe_cap(MAX_SESSION_WITNESS_SEGMENT_PROBES).is_ok());
        let error =
            ensure_witness_segment_probe_cap(MAX_SESSION_WITNESS_SEGMENT_PROBES + 1).unwrap_err();
        assert!(matches!(
            &error,
            IndexError::SessionAuthorityWorkLimitExceeded {
                operation: "segment probes",
                maximum: MAX_SESSION_WITNESS_SEGMENT_PROBES,
            }
        ));
        assert!(crate::prior_session_identity_lookup_failure_is_passthrough(
            &error
        ));
    }
}

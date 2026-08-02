use std::collections::HashMap;

use ctx_history_core::{CoreRecord, SourceKey, StableEntityId};
use sha2::{Digest, Sha256};
use tantivy::{schema::IndexRecordOption, Searcher, TantivyDocument, Term};
use uuid::Uuid;

use crate::{Fields, IndexError, Result};

pub(crate) fn prior_core_record(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
    current_source: &SourceKey,
) -> Result<Option<CoreRecord>> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let term = Term::from_field_text(fields.event_id, &identity.as_uuid().to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(None);
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let mut prior = None;
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let bytes = document
            .get_first(fields.core_record)
            .and_then(|value| value.as_bytes())
            .ok_or(IndexError::EmptyDocumentField {
                field: "core_record",
            })?;
        crate::query::validate_core_record_encoded_bytes(searcher, address, bytes.len())?;
        let decoded = CoreRecord::decode_stored(bytes)?;
        if decoded.event_id != identity {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
        if !decoded.source.exact_descriptor_eq(current_source) {
            continue;
        }
        if prior.is_some() {
            return Err(IndexError::DuplicateEventIdentity(
                identity.as_uuid().to_string(),
            ));
        }
        prior = Some(decoded);
    }
    Ok(prior)
}

pub(crate) fn source_token(source: &SourceKey) -> String {
    hex(&source.identity().digest())
}

pub(crate) fn source_sort_key(source: &SourceKey) -> [u8; 32] {
    source.identity().digest()
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

pub(crate) fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

pub(crate) fn is_generation_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn hex(value: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

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
        let existing_source = document
            .get_first(fields.source_key)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "source_key",
            })?;
        if existing_source != source_token(current_source) {
            continue;
        }
        if prior.is_some() {
            return Err(IndexError::DuplicateEventIdentity(
                identity.as_uuid().to_string(),
            ));
        }
        let bytes = document
            .get_first(fields.core_record)
            .and_then(|value| value.as_bytes())
            .ok_or(IndexError::EmptyDocumentField {
                field: "core_record",
            })?;
        let decoded = CoreRecord::decode_stored(bytes)?;
        if decoded.event_id != identity || !decoded.source.exact_descriptor_eq(current_source) {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
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

pub(crate) fn register_session_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
) -> Result<()> {
    register_compact_identity(identities, identity, "session", false)
}

pub(crate) fn validate_event_identity_against_base(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
    current_source_token: &str,
    allow_replacement_from_same_source: bool,
) -> Result<()> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let uuid = identity.as_uuid();
    let term = Term::from_field_text(fields.event_id, &uuid.to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(());
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let new_digest = hex(&identity.digest());
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let existing_digest = document
            .get_first(fields.event_identity_digest)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "event_identity_digest",
            })?;
        let existing_source = document
            .get_first(fields.source_key)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "source_key",
            })?;
        if allow_replacement_from_same_source && existing_source == current_source_token {
            continue;
        }
        if existing_digest == new_digest {
            return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
        }
        return Err(IndexError::CompactIdentityCollision {
            kind: "event",
            uuid,
            existing_digest: existing_digest.to_owned(),
            new_digest,
        });
    }
    Ok(())
}

pub(crate) fn validate_session_identity_against_base(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
    current_source_token: &str,
) -> Result<()> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let uuid = identity.as_uuid();
    let term = Term::from_field_text(fields.session_id, &uuid.to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(());
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let new_digest = hex(&identity.digest());
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let existing_source = document
            .get_first(fields.source_key)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "source_key",
            })?;
        if existing_source == current_source_token {
            continue;
        }
        let existing_digest = document
            .get_first(fields.session_identity_digest)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "session_identity_digest",
            })?;
        if existing_digest == new_digest {
            return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
        }
        return Err(IndexError::CompactIdentityCollision {
            kind: "session",
            uuid,
            existing_digest: existing_digest.to_owned(),
            new_digest,
        });
    }
    Ok(())
}

pub(crate) fn validate_referenced_session_identity_against_base(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
) -> Result<()> {
    use tantivy::{collector::TopDocs, query::TermQuery, schema::Value as TantivyValue};

    let uuid = identity.as_uuid();
    let term = Term::from_field_text(fields.session_id, &uuid.to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(());
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    let new_digest = hex(&identity.digest());
    for (_, address) in hits {
        let document: TantivyDocument = searcher.doc(address)?;
        let existing_digest = document
            .get_first(fields.session_identity_digest)
            .and_then(|value| value.as_str())
            .ok_or(IndexError::EmptyDocumentField {
                field: "session_identity_digest",
            })?;
        if existing_digest != new_digest {
            return Err(IndexError::CompactIdentityCollision {
                kind: "session",
                uuid,
                existing_digest: existing_digest.to_owned(),
                new_digest,
            });
        }
    }
    Ok(())
}

pub(crate) fn register_event_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
) -> Result<()> {
    register_compact_identity(identities, identity, "event", true)
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

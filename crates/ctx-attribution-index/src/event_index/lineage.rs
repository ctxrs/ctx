//! Normalized, generation-local encoding for typed session and copied-event lineage.

use std::collections::BTreeMap;

use ctx_attribution_model::{EventCopyProofKind, SessionRelationshipKind};
use ctx_history_core::{StableEntityId, StableEntityKind};

use super::{
    COPIED_ORIGIN_ROW_BYTES, CompactIndexedCoreEventState, EventIndexError, EventIndexKey,
    IndexedCopiedEventOrigin, IndexedCoreEventLineage, IndexedCoreEventOriginKind,
    SESSION_DICTIONARY_ROW_BYTES, compare_compact_records, validate_identity_kind,
};

const SESSION_ORIGIN_SHIFT: u32 = 30;
const SESSION_ORIGIN_MASK: u32 = 0b11 << SESSION_ORIGIN_SHIFT;
const SESSION_ORDINAL_MASK: u32 = !SESSION_ORIGIN_MASK;
const UNKNOWN_ORIGIN_TAG: u32 = 0;
const UNIQUE_ORIGIN_TAG: u32 = 1;
const COPIED_ORIGIN_TAG: u32 = 2;
const STAGED_SESSION_ORIGIN_SHIFT: u32 = 14;
const STAGED_SESSION_ORDINAL_MASK: u32 = (1 << STAGED_SESSION_ORIGIN_SHIFT) - 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedCopiedOriginRow {
    pub event_ordinal: u32,
    pub origin: IndexedCopiedEventOrigin,
}

/// Canonical dictionaries derived from one immutable `EventIndex` or staged page.
/// Ordinals are local to that container and are never serialized into public output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventLineageTables {
    pub sessions: Vec<IndexedCoreEventLineage>,
    pub copied_origins: Vec<IndexedCopiedOriginRow>,
}

impl EventLineageTables {
    pub fn build(
        lineages: impl IntoIterator<Item = IndexedCoreEventLineage>,
    ) -> Result<(Vec<u32>, Self), EventIndexError> {
        let lineages = lineages.into_iter().collect::<Vec<_>>();
        let mut sessions =
            BTreeMap::<[u8; StableEntityId::CANONICAL_LEN], IndexedCoreEventLineage>::new();
        for lineage in &lineages {
            let mut session = lineage.clone();
            session.origin_kind = IndexedCoreEventOriginKind::Unknown;
            session.copied_from = None;
            let key = identity_bytes(session.session_id)?;
            match sessions.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    session.validate()?;
                    entry.insert(session);
                }
                std::collections::btree_map::Entry::Occupied(entry) if entry.get() != &session => {
                    return Err(EventIndexError::Conflict);
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
        if sessions.len() > SESSION_ORDINAL_MASK as usize {
            return Err(EventIndexError::Bound("session dictionary count"));
        }
        let sessions = sessions.into_values().collect::<Vec<_>>();
        let session_ordinals = sessions
            .iter()
            .enumerate()
            .map(|(ordinal, lineage)| {
                Ok((
                    identity_bytes(lineage.session_id)?,
                    u32::try_from(ordinal)
                        .map_err(|_| EventIndexError::Bound("session dictionary ordinal"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, EventIndexError>>()?;

        let mut session_refs = Vec::with_capacity(lineages.len());
        let mut copied_origins = Vec::new();
        for (event_ordinal, lineage) in lineages.into_iter().enumerate() {
            let session_ordinal = *session_ordinals
                .get(&identity_bytes(lineage.session_id)?)
                .ok_or(EventIndexError::Invalid("session dictionary reference"))?;
            let origin_tag = match (lineage.origin_kind, lineage.copied_from) {
                (IndexedCoreEventOriginKind::Unknown, None) => UNKNOWN_ORIGIN_TAG,
                (IndexedCoreEventOriginKind::UniqueToSession, None) => UNIQUE_ORIGIN_TAG,
                (IndexedCoreEventOriginKind::CopiedFromAncestor, Some(origin)) => {
                    validate_identity_kind(origin.ancestor_session_id, StableEntityKind::Session)?;
                    validate_identity_kind(origin.ancestor_event_id, StableEntityKind::Event)?;
                    copied_origins.push(IndexedCopiedOriginRow {
                        event_ordinal: u32::try_from(event_ordinal)
                            .map_err(|_| EventIndexError::Bound("copied event ordinal"))?,
                        origin,
                    });
                    COPIED_ORIGIN_TAG
                }
                _ => return Err(EventIndexError::Invalid("event origin shape")),
            };
            let session_ref = session_ordinal | (origin_tag << SESSION_ORIGIN_SHIFT);
            session_refs.push(session_ref);
        }
        Ok((
            session_refs,
            Self {
                sessions,
                copied_origins,
            },
        ))
    }

    #[cfg(test)]
    pub fn encoded_bytes(&self, event_count: usize) -> Result<usize, EventIndexError> {
        event_count
            .checked_mul(4)
            .and_then(|bytes| {
                self.sessions
                    .len()
                    .checked_mul(SESSION_DICTIONARY_ROW_BYTES)
                    .and_then(|session_bytes| bytes.checked_add(session_bytes))
            })
            .and_then(|bytes| {
                self.copied_origins
                    .len()
                    .checked_mul(COPIED_ORIGIN_ROW_BYTES)
                    .and_then(|copy_bytes| bytes.checked_add(copy_bytes))
            })
            .ok_or(EventIndexError::Bound("lineage table bytes"))
    }

    pub fn from_encoded(
        session_refs: &[u32],
        sessions: Vec<IndexedCoreEventLineage>,
        copied_origins: Vec<IndexedCopiedOriginRow>,
    ) -> Result<Self, EventIndexError> {
        let tables = Self {
            sessions,
            copied_origins,
        };
        tables.validate_refs(session_refs)?;
        Ok(tables)
    }

    pub fn validate_refs(&self, session_refs: &[u32]) -> Result<(), EventIndexError> {
        if self.sessions.len() > SESSION_ORDINAL_MASK as usize {
            return Err(EventIndexError::Corrupt("session dictionary count"));
        }
        let mut prior_session = None;
        for session in &self.sessions {
            session
                .validate()
                .map_err(|_| EventIndexError::Corrupt("session dictionary model"))?;
            if session.copied_from.is_some() {
                return Err(EventIndexError::Corrupt("session dictionary copied origin"));
            }
            let key = identity_bytes(session.session_id)
                .map_err(|_| EventIndexError::Corrupt("session dictionary identity"))?;
            if prior_session.as_ref().is_some_and(|prior| prior >= &key) {
                return Err(EventIndexError::Corrupt(
                    "session dictionary ordering or duplicate",
                ));
            }
            prior_session = Some(key);
        }

        let mut prior_copied_event = None;
        for row in &self.copied_origins {
            let event_ordinal = usize::try_from(row.event_ordinal)
                .map_err(|_| EventIndexError::Corrupt("copied event ordinal"))?;
            if event_ordinal >= session_refs.len()
                || decode_origin_tag(session_refs[event_ordinal])? != COPIED_ORIGIN_TAG
                || prior_copied_event.is_some_and(|prior| prior >= row.event_ordinal)
            {
                return Err(EventIndexError::Corrupt(
                    "copied origin reference or duplicate",
                ));
            }
            prior_copied_event = Some(row.event_ordinal);
        }
        if self.copied_origins.len()
            != session_refs
                .iter()
                .filter(|session_ref| {
                    decode_origin_tag(**session_ref)
                        .is_ok_and(|origin_tag| origin_tag == COPIED_ORIGIN_TAG)
                })
                .count()
        {
            return Err(EventIndexError::Corrupt("missing copied origin row"));
        }
        for session_ref in session_refs {
            let session_ordinal = usize::try_from(*session_ref & SESSION_ORDINAL_MASK)
                .map_err(|_| EventIndexError::Corrupt("session dictionary ordinal"))?;
            if session_ordinal >= self.sessions.len() || decode_origin_tag(*session_ref).is_err() {
                return Err(EventIndexError::Corrupt("session dictionary ordinal"));
            }
        }
        Ok(())
    }

    pub fn lineages(
        &self,
        session_refs: &[u32],
    ) -> Result<Vec<IndexedCoreEventLineage>, EventIndexError> {
        session_refs
            .iter()
            .enumerate()
            .map(|(event_ordinal, session_ref)| self.resolve(*session_ref, event_ordinal))
            .collect()
    }

    pub fn resolve(
        &self,
        session_ref: u32,
        event_ordinal: usize,
    ) -> Result<IndexedCoreEventLineage, EventIndexError> {
        let session_ordinal = usize::try_from(session_ref & SESSION_ORDINAL_MASK)
            .map_err(|_| EventIndexError::Corrupt("session dictionary ordinal"))?;
        let mut lineage = self
            .sessions
            .get(session_ordinal)
            .cloned()
            .ok_or(EventIndexError::Corrupt("session dictionary ordinal"))?;
        match decode_origin_tag(session_ref)? {
            UNKNOWN_ORIGIN_TAG => {
                lineage.origin_kind = IndexedCoreEventOriginKind::Unknown;
            }
            UNIQUE_ORIGIN_TAG => {
                lineage.origin_kind = IndexedCoreEventOriginKind::UniqueToSession;
            }
            COPIED_ORIGIN_TAG => {
                let event_ordinal = u32::try_from(event_ordinal)
                    .map_err(|_| EventIndexError::Corrupt("copied event ordinal"))?;
                lineage.origin_kind = IndexedCoreEventOriginKind::CopiedFromAncestor;
                lineage.copied_from = Some(
                    self.copied_origins
                        .binary_search_by_key(&event_ordinal, |row| row.event_ordinal)
                        .ok()
                        .and_then(|index| self.copied_origins.get(index))
                        .ok_or(EventIndexError::Corrupt("missing copied origin row"))?
                        .origin
                        .clone(),
                );
            }
            _ => return Err(EventIndexError::Corrupt("event origin tag")),
        }
        Ok(lineage)
    }
}

/// Bounded publication-time dictionary. Ordinary event rows retain one u32;
/// full session lineage is owned once per distinct session and copied origin
/// payload is owned only for copied events.
#[derive(Default)]
pub struct EventLineageAccumulator {
    session_ordinals: BTreeMap<[u8; StableEntityId::CANONICAL_LEN], u32>,
    sessions: Vec<IndexedCoreEventLineage>,
    copied_by_event: BTreeMap<EventIndexKey, IndexedCopiedEventOrigin>,
}

impl EventLineageAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session_ordinals: BTreeMap::new(),
            sessions: Vec::new(),
            copied_by_event: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.copied_by_event.is_empty()
    }

    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (self.sessions.len(), self.copied_by_event.len())
    }

    pub fn retain(
        &mut self,
        key: EventIndexKey,
        lineage: IndexedCoreEventLineage,
    ) -> Result<(u32, bool, bool), EventIndexError> {
        lineage.validate()?;
        let IndexedCoreEventLineage {
            session_id,
            parent_session_id,
            root_session_id,
            session_relationship,
            origin_kind,
            copied_from,
        } = lineage;
        let session = IndexedCoreEventLineage {
            session_id,
            parent_session_id,
            root_session_id,
            session_relationship,
            origin_kind: IndexedCoreEventOriginKind::Unknown,
            copied_from: None,
        };
        let identity = identity_bytes(session.session_id)?;
        let (ordinal, inserted_session) =
            if let Some(ordinal) = self.session_ordinals.get(&identity).copied() {
                let existing = usize::try_from(ordinal)
                    .ok()
                    .and_then(|ordinal| self.sessions.get(ordinal))
                    .ok_or(EventIndexError::Invalid("session dictionary ordinal"))?;
                if existing != &session {
                    return Err(EventIndexError::Conflict);
                }
                (ordinal, false)
            } else {
                let ordinal = u32::try_from(self.sessions.len())
                    .map_err(|_| EventIndexError::Bound("session dictionary ordinal"))?;
                if ordinal > SESSION_ORDINAL_MASK {
                    return Err(EventIndexError::Bound("session dictionary count"));
                }
                self.sessions.push(session);
                self.session_ordinals.insert(identity, ordinal);
                (ordinal, true)
            };
        let (origin_tag, inserted_copy) = match (origin_kind, copied_from) {
            (IndexedCoreEventOriginKind::Unknown, None) => (UNKNOWN_ORIGIN_TAG, false),
            (IndexedCoreEventOriginKind::UniqueToSession, None) => (UNIQUE_ORIGIN_TAG, false),
            (IndexedCoreEventOriginKind::CopiedFromAncestor, Some(origin)) => {
                if self.copied_by_event.insert(key, origin).is_some() {
                    return Err(EventIndexError::Conflict);
                }
                (COPIED_ORIGIN_TAG, true)
            }
            _ => return Err(EventIndexError::Invalid("event origin shape")),
        };
        Ok((
            ordinal | (origin_tag << SESSION_ORIGIN_SHIFT),
            inserted_session,
            inserted_copy,
        ))
    }

    pub fn absorb_page(
        &mut self,
        records: &mut [CompactIndexedCoreEventState],
        lineage: EventLineageTables,
    ) -> Result<(usize, usize), EventIndexError> {
        let mut remap = Vec::with_capacity(lineage.sessions.len());
        let mut inserted_sessions = 0_usize;
        for session in lineage.sessions {
            let (ordinal, inserted) = self.retain_session(session)?;
            remap.push(ordinal);
            inserted_sessions = inserted_sessions.saturating_add(usize::from(inserted));
        }
        for record in records.iter_mut() {
            let old = session_ordinal(record.session_ref)?;
            let new = *remap
                .get(old)
                .ok_or(EventIndexError::Invalid("session dictionary reference"))?;
            record.session_ref = new | (record.session_ref & SESSION_ORIGIN_MASK);
        }
        let inserted_copies = lineage.copied_origins.len();
        for copied in lineage.copied_origins {
            let record = records
                .get(
                    usize::try_from(copied.event_ordinal)
                        .map_err(|_| EventIndexError::Invalid("copied event ordinal"))?,
                )
                .ok_or(EventIndexError::Invalid("copied event ordinal"))?;
            if decode_origin_tag(record.session_ref)? != COPIED_ORIGIN_TAG
                || self
                    .copied_by_event
                    .insert(record.key(), copied.origin)
                    .is_some()
            {
                return Err(EventIndexError::Conflict);
            }
        }
        Ok((inserted_sessions, inserted_copies))
    }

    fn retain_session(
        &mut self,
        session: IndexedCoreEventLineage,
    ) -> Result<(u32, bool), EventIndexError> {
        session.validate()?;
        if session.origin_kind != IndexedCoreEventOriginKind::Unknown
            || session.copied_from.is_some()
        {
            return Err(EventIndexError::Invalid("session dictionary model"));
        }
        let identity = identity_bytes(session.session_id)?;
        if let Some(ordinal) = self.session_ordinals.get(&identity).copied() {
            let existing = usize::try_from(ordinal)
                .ok()
                .and_then(|ordinal| self.sessions.get(ordinal))
                .ok_or(EventIndexError::Invalid("session dictionary ordinal"))?;
            if existing != &session {
                return Err(EventIndexError::Conflict);
            }
            Ok((ordinal, false))
        } else {
            let ordinal = u32::try_from(self.sessions.len())
                .map_err(|_| EventIndexError::Bound("session dictionary ordinal"))?;
            if ordinal > SESSION_ORDINAL_MASK {
                return Err(EventIndexError::Bound("session dictionary count"));
            }
            self.sessions.push(session);
            self.session_ordinals.insert(identity, ordinal);
            Ok((ordinal, true))
        }
    }

    pub fn finish(
        mut self,
        records: &mut [CompactIndexedCoreEventState],
    ) -> Result<EventLineageTables, EventIndexError> {
        let mut ordered = self
            .session_ordinals
            .iter()
            .map(|(identity, ordinal)| (*identity, *ordinal))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(identity, _)| *identity);
        let mut remap = vec![0_u32; self.sessions.len()];
        let mut sessions = Vec::with_capacity(self.sessions.len());
        for (new_ordinal, (_, old_ordinal)) in ordered.into_iter().enumerate() {
            let old = usize::try_from(old_ordinal)
                .map_err(|_| EventIndexError::Bound("session dictionary ordinal"))?;
            let new = u32::try_from(new_ordinal)
                .map_err(|_| EventIndexError::Bound("session dictionary ordinal"))?;
            *remap
                .get_mut(old)
                .ok_or(EventIndexError::Invalid("session dictionary ordinal"))? = new;
            sessions.push(
                self.sessions
                    .get(old)
                    .cloned()
                    .ok_or(EventIndexError::Invalid("session dictionary ordinal"))?,
            );
        }
        for record in records.iter_mut() {
            let old = session_ordinal(record.session_ref)?;
            let new = *remap
                .get(old)
                .ok_or(EventIndexError::Invalid("session dictionary ordinal"))?;
            record.session_ref = new | (record.session_ref & SESSION_ORIGIN_MASK);
        }
        records.sort_by(compare_compact_records);
        let mut copied_origins = Vec::with_capacity(self.copied_by_event.len());
        for (event_ordinal, record) in records.iter().enumerate() {
            if decode_origin_tag(record.session_ref)? == COPIED_ORIGIN_TAG {
                copied_origins.push(IndexedCopiedOriginRow {
                    event_ordinal: u32::try_from(event_ordinal)
                        .map_err(|_| EventIndexError::Bound("copied event ordinal"))?,
                    origin: self
                        .copied_by_event
                        .remove(&record.key())
                        .ok_or(EventIndexError::Invalid("copied origin reference"))?,
                });
            }
        }
        if !self.copied_by_event.is_empty() {
            return Err(EventIndexError::Invalid("unreferenced copied origin"));
        }
        Ok(EventLineageTables {
            sessions,
            copied_origins,
        })
    }
}

impl IndexedCoreEventLineage {
    pub fn validate(&self) -> Result<(), EventIndexError> {
        validate_identity_kind(self.session_id, StableEntityKind::Session)?;
        if let Some(parent) = self.parent_session_id {
            validate_identity_kind(parent, StableEntityKind::Session)?;
        }
        if let Some(root) = self.root_session_id {
            validate_identity_kind(root, StableEntityKind::Session)?;
        }
        if let Some(copied) = &self.copied_from {
            validate_identity_kind(copied.ancestor_session_id, StableEntityKind::Session)?;
            validate_identity_kind(copied.ancestor_event_id, StableEntityKind::Event)?;
        }
        match (self.origin_kind, self.copied_from.is_some()) {
            (
                IndexedCoreEventOriginKind::Unknown | IndexedCoreEventOriginKind::UniqueToSession,
                false,
            )
            | (IndexedCoreEventOriginKind::CopiedFromAncestor, true) => {}
            _ => return Err(EventIndexError::Invalid("event origin shape")),
        }
        Ok(())
    }
}

pub fn encode_session_lineage(
    lineage: &IndexedCoreEventLineage,
) -> Result<[u8; SESSION_DICTIONARY_ROW_BYTES], EventIndexError> {
    lineage.validate()?;
    if lineage.copied_from.is_some() || lineage.origin_kind != IndexedCoreEventOriginKind::Unknown {
        return Err(EventIndexError::Invalid("session dictionary copied origin"));
    }
    let mut encoded = [0_u8; SESSION_DICTIONARY_ROW_BYTES];
    let mut offset = 0;
    write_identity(&mut encoded, &mut offset, lineage.session_id)?;
    write_optional_identity(&mut encoded, &mut offset, lineage.parent_session_id)?;
    write_optional_identity(&mut encoded, &mut offset, lineage.root_session_id)?;
    encoded[offset] = encode_session_relationship(lineage.session_relationship);
    Ok(encoded)
}

pub fn decode_session_lineage(encoded: &[u8]) -> Result<IndexedCoreEventLineage, EventIndexError> {
    if encoded.len() != SESSION_DICTIONARY_ROW_BYTES {
        return Err(EventIndexError::Corrupt("session dictionary encoding"));
    }
    let mut offset = 0;
    let session_id = decode_identity(
        read_identity_bytes(encoded, &mut offset)?,
        StableEntityKind::Session,
    )?;
    let parent_session_id = decode_optional_identity(
        encoded,
        &mut offset,
        StableEntityKind::Session,
        "session parent identity",
    )?;
    let root_session_id = decode_optional_identity(
        encoded,
        &mut offset,
        StableEntityKind::Session,
        "session root identity",
    )?;
    let session_relationship = decode_session_relationship(
        *encoded
            .get(offset)
            .ok_or(EventIndexError::Corrupt("session relationship"))?,
    )?;
    let lineage = IndexedCoreEventLineage {
        session_id,
        parent_session_id,
        root_session_id,
        session_relationship,
        origin_kind: IndexedCoreEventOriginKind::Unknown,
        copied_from: None,
    };
    lineage
        .validate()
        .map_err(|_| EventIndexError::Corrupt("session dictionary model"))?;
    Ok(lineage)
}

pub fn session_ordinal(session_ref: u32) -> Result<usize, EventIndexError> {
    usize::try_from(session_ref & SESSION_ORDINAL_MASK)
        .map_err(|_| EventIndexError::Corrupt("session dictionary ordinal"))
}

pub fn decode_event_origin_kind(
    session_ref: u32,
) -> Result<IndexedCoreEventOriginKind, EventIndexError> {
    match decode_origin_tag(session_ref)? {
        UNKNOWN_ORIGIN_TAG => Ok(IndexedCoreEventOriginKind::Unknown),
        UNIQUE_ORIGIN_TAG => Ok(IndexedCoreEventOriginKind::UniqueToSession),
        COPIED_ORIGIN_TAG => Ok(IndexedCoreEventOriginKind::CopiedFromAncestor),
        _ => Err(EventIndexError::Corrupt("event origin tag")),
    }
}

/// Journal pages contain at most 256 events, so their page-local session
/// dictionary needs only fourteen ordinal bits. The durable `EventIndex` keeps
/// the wider u32 representation; this compact form avoids paying that width in
/// every independently checked staging row.
pub fn encode_staged_session_ref(session_ref: u32) -> Result<u16, EventIndexError> {
    let ordinal = u32::try_from(session_ordinal(session_ref)?)
        .map_err(|_| EventIndexError::Bound("staged session dictionary ordinal"))?;
    if ordinal > STAGED_SESSION_ORDINAL_MASK {
        return Err(EventIndexError::Bound("staged session dictionary ordinal"));
    }
    let tag = decode_origin_tag(session_ref)?;
    u16::try_from(ordinal | (tag << STAGED_SESSION_ORIGIN_SHIFT))
        .map_err(|_| EventIndexError::Bound("staged session reference"))
}

pub fn decode_staged_session_ref(encoded: u16) -> Result<u32, EventIndexError> {
    let encoded = u32::from(encoded);
    let tag = encoded >> STAGED_SESSION_ORIGIN_SHIFT;
    match tag {
        UNKNOWN_ORIGIN_TAG | UNIQUE_ORIGIN_TAG | COPIED_ORIGIN_TAG => {
            Ok((encoded & STAGED_SESSION_ORDINAL_MASK) | (tag << SESSION_ORIGIN_SHIFT))
        }
        _ => Err(EventIndexError::Corrupt("staged event origin tag")),
    }
}

fn decode_origin_tag(session_ref: u32) -> Result<u32, EventIndexError> {
    let tag = (session_ref & SESSION_ORIGIN_MASK) >> SESSION_ORIGIN_SHIFT;
    match tag {
        UNKNOWN_ORIGIN_TAG | UNIQUE_ORIGIN_TAG | COPIED_ORIGIN_TAG => Ok(tag),
        _ => Err(EventIndexError::Corrupt("event origin tag")),
    }
}

pub fn encode_copied_origin(
    row: &IndexedCopiedOriginRow,
) -> Result<[u8; COPIED_ORIGIN_ROW_BYTES], EventIndexError> {
    validate_identity_kind(row.origin.ancestor_session_id, StableEntityKind::Session)?;
    validate_identity_kind(row.origin.ancestor_event_id, StableEntityKind::Event)?;
    let mut encoded = [0_u8; COPIED_ORIGIN_ROW_BYTES];
    encoded[..4].copy_from_slice(&row.event_ordinal.to_le_bytes());
    let mut offset = 4;
    write_identity(&mut encoded, &mut offset, row.origin.ancestor_session_id)?;
    write_identity(&mut encoded, &mut offset, row.origin.ancestor_event_id)?;
    encoded[offset] = encode_copy_proof(row.origin.proof);
    Ok(encoded)
}

pub fn decode_copied_origin(encoded: &[u8]) -> Result<IndexedCopiedOriginRow, EventIndexError> {
    if encoded.len() != COPIED_ORIGIN_ROW_BYTES {
        return Err(EventIndexError::Corrupt("copied origin encoding"));
    }
    let event_ordinal = u32::from_le_bytes(
        encoded[..4]
            .try_into()
            .map_err(|_| EventIndexError::Corrupt("copied event ordinal"))?,
    );
    let mut offset = 4;
    let ancestor_session_id = decode_identity(
        read_identity_bytes(encoded, &mut offset)?,
        StableEntityKind::Session,
    )?;
    let ancestor_event_id = decode_identity(
        read_identity_bytes(encoded, &mut offset)?,
        StableEntityKind::Event,
    )?;
    let proof = decode_copy_proof(
        *encoded
            .get(offset)
            .ok_or(EventIndexError::Corrupt("copied-event proof"))?,
    )?;
    Ok(IndexedCopiedOriginRow {
        event_ordinal,
        origin: IndexedCopiedEventOrigin {
            ancestor_session_id,
            ancestor_event_id,
            proof,
        },
    })
}

fn identity_bytes(
    identity: StableEntityId,
) -> Result<[u8; StableEntityId::CANONICAL_LEN], EventIndexError> {
    identity
        .encode_canonical()
        .map_err(|_| EventIndexError::Invalid("stable identity"))
}

fn write_identity(
    encoded: &mut [u8],
    offset: &mut usize,
    identity: StableEntityId,
) -> Result<(), EventIndexError> {
    let end = offset
        .checked_add(StableEntityId::CANONICAL_LEN)
        .ok_or(EventIndexError::Bound("stable identity encoding"))?;
    encoded
        .get_mut(*offset..end)
        .ok_or(EventIndexError::Bound("stable identity encoding"))?
        .copy_from_slice(&identity_bytes(identity)?);
    *offset = end;
    Ok(())
}

fn write_optional_identity(
    encoded: &mut [u8],
    offset: &mut usize,
    identity: Option<StableEntityId>,
) -> Result<(), EventIndexError> {
    let marker = encoded
        .get_mut(*offset)
        .ok_or(EventIndexError::Bound("optional identity marker"))?;
    *marker = u8::from(identity.is_some());
    *offset += 1;
    if let Some(identity) = identity {
        write_identity(encoded, offset, identity)
    } else {
        let end = offset
            .checked_add(StableEntityId::CANONICAL_LEN)
            .ok_or(EventIndexError::Bound("optional identity encoding"))?;
        encoded
            .get_mut(*offset..end)
            .ok_or(EventIndexError::Bound("optional identity encoding"))?
            .fill(0);
        *offset = end;
        Ok(())
    }
}

fn read_identity_bytes<'a>(
    encoded: &'a [u8],
    offset: &mut usize,
) -> Result<&'a [u8], EventIndexError> {
    let end = offset
        .checked_add(StableEntityId::CANONICAL_LEN)
        .ok_or(EventIndexError::Corrupt("stable identity range"))?;
    let identity = encoded
        .get(*offset..end)
        .ok_or(EventIndexError::Corrupt("stable identity range"))?;
    *offset = end;
    Ok(identity)
}

fn decode_optional_identity(
    encoded: &[u8],
    offset: &mut usize,
    kind: StableEntityKind,
    invalid: &'static str,
) -> Result<Option<StableEntityId>, EventIndexError> {
    let present = *encoded
        .get(*offset)
        .ok_or(EventIndexError::Corrupt(invalid))?;
    *offset += 1;
    let identity = read_identity_bytes(encoded, offset)?;
    match present {
        0 if identity.iter().all(|byte| *byte == 0) => Ok(None),
        1 => decode_identity(identity, kind).map(Some),
        _ => Err(EventIndexError::Corrupt(invalid)),
    }
}

fn decode_identity(
    encoded: &[u8],
    kind: StableEntityKind,
) -> Result<StableEntityId, EventIndexError> {
    let identity = StableEntityId::decode_canonical(encoded)
        .map_err(|_| EventIndexError::Corrupt("stable identity encoding"))?;
    validate_identity_kind(identity, kind)
        .map_err(|_| EventIndexError::Corrupt("stable identity kind"))?;
    Ok(identity)
}

const fn encode_session_relationship(value: SessionRelationshipKind) -> u8 {
    match value {
        SessionRelationshipKind::Root => 0,
        SessionRelationshipKind::Delegated => 1,
        SessionRelationshipKind::Forked => 2,
        SessionRelationshipKind::ResumedFrom => 3,
        SessionRelationshipKind::WorkflowChild => 4,
        SessionRelationshipKind::RelatedUnknown => 5,
    }
}

fn decode_session_relationship(value: u8) -> Result<SessionRelationshipKind, EventIndexError> {
    match value {
        0 => Ok(SessionRelationshipKind::Root),
        1 => Ok(SessionRelationshipKind::Delegated),
        2 => Ok(SessionRelationshipKind::Forked),
        3 => Ok(SessionRelationshipKind::ResumedFrom),
        4 => Ok(SessionRelationshipKind::WorkflowChild),
        5 => Ok(SessionRelationshipKind::RelatedUnknown),
        _ => Err(EventIndexError::Corrupt("session relationship")),
    }
}

const fn encode_copy_proof(value: EventCopyProofKind) -> u8 {
    match value {
        EventCopyProofKind::NativeEventIdentity => 0,
        EventCopyProofKind::NativeCopiedFromField => 1,
        EventCopyProofKind::NativeCallResultIdentity => 2,
        EventCopyProofKind::CertifiedOrderedPrefix => 3,
    }
}

fn decode_copy_proof(value: u8) -> Result<EventCopyProofKind, EventIndexError> {
    match value {
        0 => Ok(EventCopyProofKind::NativeEventIdentity),
        1 => Ok(EventCopyProofKind::NativeCopiedFromField),
        2 => Ok(EventCopyProofKind::NativeCallResultIdentity),
        3 => Ok(EventCopyProofKind::CertifiedOrderedPrefix),
        _ => Err(EventIndexError::Corrupt("copied-event proof")),
    }
}
